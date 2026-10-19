// SPDX-License-Identifier: LGPL-3.0-only
#![allow(unused_imports, dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WAIT_TIMEOUT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::INFINITE;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, KillTimer,
    MsgWaitForMultipleObjectsEx, PeekMessageW, PostMessageW, RegisterClassExW, SetTimer,
    TranslateMessage, HWND_MESSAGE, MSG, MWMO_ALERTABLE, PM_REMOVE, QS_ALLINPUT, WM_QUIT, WM_TIMER,
    WNDCLASSEXW,
};
pub(crate) fn remove_posted_timer_event(hwnd: HWND, timer_id: u32) {
    if !hwnd.is_null() {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, hwnd, WM_TIMER, WM_TIMER, PM_REMOVE) != 0 {
                if msg.wParam as u32 == timer_id {
                    continue;
                } else {
                    let _ = PENDING_WM_TIMERS.try_with(|q| {
                        if let Ok(mut borrow) = q.try_borrow_mut() {
                            borrow.push(msg.wParam as u32);
                        }
                    });
                }
            }
        }
    }

    let _ = PENDING_WM_TIMERS.try_with(|q| {
        if let Ok(mut borrow) = q.try_borrow_mut() {
            borrow.retain(|&id| id != timer_id);
        }
    });
}

use crate::event::{NativeEventFilter, NativeEventFilterChain, NativeMessage};
pub use crate::event_loop::dispatcher::DispatchResult;
use crate::event_loop::dispatcher::{EventDispatcher, EventDispatcherHandle};
use crate::object::{ObjectId, QObject};
use crate::poison::lock_unpoisoned;
use crate::timer::{calculate_next_timeout, current_time_ms, TimerEntry, TimerId, TimerRegistry};

thread_local! {
    static PENDING_WM_TIMERS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    /// `wakeup_pending` flags of this thread's dispatchers, keyed by internal HWND.
    static WAKEUP_FLAGS: RefCell<HashMap<usize, Arc<AtomicBool>>> = RefCell::new(HashMap::new());
}

pub const WM_FERROKIT_WAKEUP: u32 = 0x0400 + 101;

const CLASS_NAME: &[u16] = &[
    'Q' as u16, 't' as u16, 'r' as u16, 's' as u16, 'M' as u16, 'e' as u16, 's' as u16, 's' as u16,
    'a' as u16, 'g' as u16, 'e' as u16, 'W' as u16, 'i' as u16, 'n' as u16, 'd' as u16, 'o' as u16,
    'w' as u16, 'C' as u16, 'l' as u16, 'a' as u16, 's' as u16, 's' as u16, 0,
];

/// Window procedure entry point. Unwinding out of an `extern "system"` callback aborts the
/// process, so a raising handler is contained here and the message falls back to the default.
unsafe extern "system" fn internal_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match catch_unwind(AssertUnwindSafe(|| {
        internal_wnd_proc_inner(hwnd, msg, wparam, lparam)
    })) {
        Ok(result) => result,
        Err(_) => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn internal_wnd_proc_inner(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TIMER {
        let timer_id = wparam as u32;
        if crate::timer::has_thread_timer_context() {
            dispatch_thread_timer(timer_id, hwnd);
        } else {
            PENDING_WM_TIMERS.with(|q| q.borrow_mut().push(timer_id));
        }
        return 0;
    }

    if msg == WM_FERROKIT_WAKEUP {
        // Reached only when a native modal loop (e.g. sizing) dispatches the wake-up
        // message itself; the regular `process_events` consumes it via PeekMessage.
        let _ = WAKEUP_FLAGS.try_with(|flags| {
            if let Some(flag) = flags.borrow().get(&(hwnd as usize)) {
                flag.store(false, Ordering::Release);
            }
        });
        let _ = crate::event_loop::pump_posted_events_modal();
        return 0;
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Restores `in_timer_event` (and the single-shot/KillTimer bookkeeping) on every exit path,
/// including unwinding out of a raising timer slot.
struct InTimerGuard<'a> {
    id: TimerId,
    raw_id: u32,
    internal_hwnd: HWND,
    single_shot: bool,
    rearm_now_ms: Option<u64>,
    rearmed_elapse: &'a Cell<Option<u32>>,
}

impl Drop for InTimerGuard<'_> {
    fn drop(&mut self) {
        if !self.single_shot {
            let rearm_now_ms = self.rearm_now_ms.unwrap_or_else(current_time_ms);
            let mut rearm_elapse: Option<u32> = None;
            let mut should_kill = false;
            crate::timer::with_thread_timer_context(|ctx| {
                let mut reg = lock_unpoisoned(&ctx.registry);
                if let Some(entry) = reg.get_mut(self.id) {
                    entry.in_timer_event = false;
                    if entry.interval_ms > crate::timer::WIN32_TIMER_MAX_MS {
                        rearm_elapse = Some(crate::timer::win32_timer_elapse(
                            entry.next_fire_ms.saturating_sub(rearm_now_ms).max(1),
                        ));
                    }
                } else {
                    should_kill = true;
                }
            });
            if should_kill {
                crate::timer::kill_native(self.internal_hwnd, self.id);
            } else if let Some(elapse) = rearm_elapse {
                self.rearmed_elapse.set(Some(elapse));
                if !self.internal_hwnd.is_null() {
                    unsafe {
                        SetTimer(self.internal_hwnd, self.raw_id as usize, elapse, None);
                    }
                }
            }
        }
    }
}

fn dispatch_thread_timer(raw_id: u32, internal_hwnd: HWND) {
    let _ = dispatch_thread_timer_impl(raw_id, internal_hwnd, current_time_ms(), None);
}

/// Processes a `WM_TIMER` notification for `raw_id` with explicit pre-callback (`now_ms`) and
/// post-callback (`rearm_now_ms`) timestamps.
///
/// Because Win32 `SetTimer` caps `uElapse` at `USER_TIMER_MAXIMUM` (`0x7FFF_FFFF` ms ≈ 24.85 days),
/// a timer whose `next_fire_ms` is further in the future receives an intermediate `WM_TIMER`
/// while `now_ms < entry.next_fire_ms`. That tick must neither fire the callback nor consume a
/// single-shot timer; instead it re-arms `SetTimer` for the remaining wait
/// (`(entry.next_fire_ms - now_ms).min(WIN32_TIMER_MAX_MS)`). When a repeating multi-slice timer
/// does fire (`now_ms >= entry.next_fire_ms`), the post-callback re-arm deducts the callback's
/// execution time via `entry.next_fire_ms.saturating_sub(rearm_now_ms).max(1)`.
pub(crate) fn dispatch_thread_timer_at(
    raw_id: u32,
    internal_hwnd: HWND,
    now_ms: u64,
    rearm_now_ms: u64,
) -> Option<u32> {
    dispatch_thread_timer_impl(raw_id, internal_hwnd, now_ms, Some(rearm_now_ms))
}

fn dispatch_thread_timer_impl(
    raw_id: u32,
    internal_hwnd: HWND,
    now_ms: u64,
    rearm_now_ms: Option<u64>,
) -> Option<u32> {
    let id = TimerId(raw_id);
    let precise = crate::timer::is_precise_source(raw_id);
    crate::timer::acknowledge_source(raw_id);
    let mut receiver_opt = None;
    let mut single_shot = false;
    let mut unexpired_elapse: Option<u32> = None;

    crate::timer::with_thread_timer_context(|ctx| {
        let mut reg = lock_unpoisoned(&ctx.registry);
        if let Some(entry) = reg.get_mut(id) {
            if !entry.in_timer_event {
                if precise && now_ms < entry.next_fire_ms {
                    // A multimedia tick before the expiry: hold it if it is the tick of this
                    // expiry, drop it if it is a grid point a late callback already consumed.
                    if crate::timer::classify_early_tick(
                        now_ms,
                        entry.next_fire_ms,
                        entry.interval_ms,
                    ) == crate::timer::EarlyTick::Defer
                    {
                        crate::timer::defer_source(raw_id);
                    }
                } else if !precise && now_ms < entry.next_fire_ms {
                    unexpired_elapse = Some(crate::timer::win32_timer_elapse(
                        entry.next_fire_ms - now_ms,
                    ));
                } else {
                    entry.in_timer_event = true;
                    let (adjusted, next_fire) = if precise {
                        crate::timer::next_grid_deadline(
                            entry.interval_ms,
                            entry.next_fire_ms,
                            now_ms,
                        )
                    } else {
                        calculate_next_timeout(&mut entry.timer_type, entry.interval_ms, now_ms)
                    };
                    entry.interval_ms = adjusted;
                    entry.next_fire_ms = next_fire;
                    single_shot = entry.single_shot;
                    receiver_opt = Some(entry.receiver);
                    if single_shot {
                        reg.unregister(id);
                    }
                }
            }
        }
    });

    if let Some(elapse) = unexpired_elapse {
        if !internal_hwnd.is_null() {
            unsafe {
                SetTimer(internal_hwnd, raw_id as usize, elapse, None);
            }
        }
        return Some(elapse);
    }

    let Some(receiver) = receiver_opt else {
        return None;
    };
    let rearmed_elapse = Cell::new(None);
    {
        let _in_timer = InTimerGuard {
            id,
            raw_id,
            internal_hwnd,
            single_shot,
            rearm_now_ms,
            rearmed_elapse: &rearmed_elapse,
        };

        if single_shot {
            crate::timer::kill_native(internal_hwnd, id);
            remove_posted_timer_event(internal_hwnd, raw_id);
        }

        // Python/PySide6 contains a raising timer slot and keeps the loop and other timers running.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if matches!(
                crate::event_loop::deliver_timer_event(receiver, raw_id as u64),
                crate::event_loop::TimerDelivery::NotDeliverable
            ) {
                crate::timer::dispatch_single_shot_callback(receiver);
            }
        }));
    }
    rearmed_elapse.get()
}

static REGISTER_CLASS_ONCE: Once = Once::new();

fn ensure_window_class_registered() {
    REGISTER_CLASS_ONCE.call_once(|| unsafe {
        windows_sys::Win32::Media::timeBeginPeriod(1);

        let h_instance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(internal_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: h_instance,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: CLASS_NAME.as_ptr(),
            hIconSm: ptr::null_mut(),
        };
        RegisterClassExW(&wc);
    });
}

pub struct Win32EventDispatcher {
    pub internal_hwnd: HWND,
    pub wakeup_pending: Arc<AtomicBool>,
    pub native_filters: NativeEventFilterChain,
}

impl Default for Win32EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Win32EventDispatcher {
    pub fn new() -> Self {
        ensure_window_class_registered();

        let internal_hwnd = unsafe {
            let h_instance = GetModuleHandleW(ptr::null());
            CreateWindowExW(
                0,
                CLASS_NAME.as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                h_instance,
                ptr::null(),
            )
        };

        if internal_hwnd.is_null() {
            panic!("Failed to create Win32 message-only window for Win32EventDispatcher");
        }

        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, internal_hwnd, 0, 0, PM_REMOVE) != 0 {
                DefWindowProcW(msg.hwnd, msg.message, msg.wParam, msg.lParam);
            }
        }

        let wakeup_pending = Arc::new(AtomicBool::new(false));
        let _ = WAKEUP_FLAGS.try_with(|flags| {
            flags
                .borrow_mut()
                .insert(internal_hwnd as usize, Arc::clone(&wakeup_pending));
        });

        Self {
            internal_hwnd,
            wakeup_pending,
            native_filters: NativeEventFilterChain::new(),
        }
    }

    pub fn wake_up(&self) {
        if !self.wakeup_pending.swap(true, Ordering::Release) {
            unsafe {
                PostMessageW(self.internal_hwnd, WM_FERROKIT_WAKEUP, 0, 0);
            }
        }
    }

    pub fn clone_handle(&self) -> Win32EventDispatcherHandle {
        Win32EventDispatcherHandle {
            internal_hwnd: self.internal_hwnd,
            wakeup_pending: Arc::clone(&self.wakeup_pending),
        }
    }

    pub fn install_native_event_filter(&self, filter: Box<dyn NativeEventFilter>) {
        self.native_filters.install(filter);
    }

    pub fn filter_native_event(
        &self,
        event_type: &str,
        msg: &NativeMessage,
        result: &mut isize,
    ) -> bool {
        self.native_filters.filter_native(event_type, msg, result)
    }

    pub fn process_events(
        &self,
        can_wait: bool,
        next_timer_timeout: Option<Duration>,
    ) -> DispatchResult {
        let deadline = next_timer_timeout.map(|d| std::time::Instant::now() + d);

        loop {
            let mut processed_ui_message = false;
            let mut awoken = false;

            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        return DispatchResult::Quit(msg.wParam as i32);
                    }
                    let mut native_result = 0isize;
                    if self.native_filters.filter_native(
                        "windows_generic_MSG",
                        &NativeMessage::Windows(&msg),
                        &mut native_result,
                    ) {
                        if msg.hwnd == self.internal_hwnd && msg.message == WM_TIMER {
                            // The filter consumed this tick; the multimedia timer may post again.
                            crate::timer::acknowledge_source(msg.wParam as u32);
                        }
                        processed_ui_message = true;
                        continue;
                    }

                    if msg.hwnd == self.internal_hwnd {
                        if msg.message == WM_FERROKIT_WAKEUP {
                            self.wakeup_pending.store(false, Ordering::Release);
                            awoken = true;
                        } else if msg.message == WM_TIMER {
                            DispatchMessageW(&msg);
                        } else {
                            DefWindowProcW(msg.hwnd, msg.message, msg.wParam, msg.lParam);
                        }
                        continue;
                    }

                    processed_ui_message = true;
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if awoken {
                return DispatchResult::Awoken;
            }

            if processed_ui_message {
                return DispatchResult::Normal;
            }

            if !can_wait {
                return DispatchResult::Timeout;
            }

            let timeout_ms = match deadline {
                Some(dl) => {
                    let now = std::time::Instant::now();
                    if now >= dl {
                        return DispatchResult::Timeout;
                    }
                    let remaining = dl - now;
                    remaining.as_millis().min(u32::MAX as u128) as u32
                }
                None => INFINITE,
            };

            let wait_ret = unsafe {
                MsgWaitForMultipleObjectsEx(0, ptr::null(), timeout_ms, QS_ALLINPUT, MWMO_ALERTABLE)
            };

            if wait_ret == WAIT_TIMEOUT {
                return DispatchResult::Timeout;
            }
        }
    }

    pub fn process_events_with_timers(
        &self,
        can_wait: bool,
        next_timer_timeout: Option<Duration>,
        registry: &Mutex<TimerRegistry>,
    ) -> DispatchResult {
        let result = self.process_events(can_wait, next_timer_timeout);
        self.send_timer_events(registry);
        result
    }

    pub fn register_timer(&self, entry: &TimerEntry) {
        if entry.interval_ms == 0 {
            return;
        }
        crate::timer::arm_native(
            self.internal_hwnd,
            entry.id,
            entry.interval_ms,
            entry.timer_type,
        );
    }

    pub fn unregister_timer(&self, entry: &TimerEntry) {
        if entry.interval_ms == 0 {
            return;
        }
        let id = if entry.win32_timer_id != 0 {
            entry.win32_timer_id as usize
        } else {
            entry.id.0 as usize
        };
        crate::timer::kill_native(self.internal_hwnd, TimerId(id as u32));
    }

    pub fn send_timer_events(&self, registry: &Mutex<TimerRegistry>) {
        let _ = self.send_timer_events_with_clock(registry, current_time_ms, current_time_ms);
    }

    pub fn send_timer_events_at(
        &self,
        registry: &Mutex<TimerRegistry>,
        now_ms: u64,
        rearm_now_ms: u64,
    ) -> Vec<u32> {
        self.send_timer_events_with_clock(registry, || now_ms, || rearm_now_ms)
    }

    fn send_timer_events_with_clock(
        &self,
        registry: &Mutex<TimerRegistry>,
        mut pre_clock: impl FnMut() -> u64,
        mut post_clock: impl FnMut() -> u64,
    ) -> Vec<u32> {
        let mut pending = PENDING_WM_TIMERS.with(|q| std::mem::take(&mut *q.borrow_mut()));
        for raw_id in crate::timer::take_deferred_sources() {
            if !pending.contains(&raw_id) {
                pending.push(raw_id);
            }
        }
        let mut rearmed = Vec::new();
        for raw_id in pending {
            let id = TimerId(raw_id);
            crate::timer::acknowledge_source(raw_id);
            let now_ms = pre_clock();
            let unexpired_elapse = {
                let reg = lock_unpoisoned(registry);
                reg.get(id).and_then(|entry| {
                    if !entry.in_timer_event
                        && !crate::timer::is_precise_source(raw_id)
                        && now_ms < entry.next_fire_ms
                    {
                        Some(crate::timer::win32_timer_elapse(
                            entry.next_fire_ms - now_ms,
                        ))
                    } else {
                        None
                    }
                })
            };
            if let Some(elapse) = unexpired_elapse {
                rearmed.push(elapse);
                if !self.internal_hwnd.is_null() {
                    unsafe {
                        SetTimer(self.internal_hwnd, raw_id as usize, elapse, None);
                    }
                }
                continue;
            }

            let Some((receiver, single_shot)) =
                crate::timer::begin_timer_delivery(registry, id, now_ms)
            else {
                continue;
            };
            if single_shot {
                lock_unpoisoned(registry).unregister(id);
                crate::timer::kill_native(self.internal_hwnd, id);
                remove_posted_timer_event(self.internal_hwnd, raw_id);
            }

            if matches!(
                crate::event_loop::deliver_timer_event(receiver, raw_id as u64),
                crate::event_loop::TimerDelivery::NotDeliverable
            ) {
                crate::timer::dispatch_single_shot_callback(receiver);
            }

            if !single_shot {
                let rearm_now_ms = post_clock();
                let mut rearm_elapse: Option<u32> = None;
                let still_registered = lock_unpoisoned(registry)
                    .get_mut(id)
                    .map(|entry| {
                        entry.in_timer_event = false;
                        if entry.interval_ms > crate::timer::WIN32_TIMER_MAX_MS {
                            rearm_elapse = Some(crate::timer::win32_timer_elapse(
                                entry.next_fire_ms.saturating_sub(rearm_now_ms).max(1),
                            ));
                        }
                    })
                    .is_some();
                if !still_registered {
                    crate::timer::kill_native(self.internal_hwnd, id);
                } else if let Some(elapse) = rearm_elapse {
                    rearmed.push(elapse);
                    if !self.internal_hwnd.is_null() {
                        unsafe {
                            SetTimer(self.internal_hwnd, raw_id as usize, elapse, None);
                        }
                    }
                }
            }
        }
        rearmed
    }
}

#[derive(Debug, Clone)]
pub struct Win32EventDispatcherHandle {
    internal_hwnd: HWND,
    wakeup_pending: Arc<AtomicBool>,
}

impl Win32EventDispatcherHandle {
    pub fn wake_up(&self) {
        if !self.wakeup_pending.swap(true, Ordering::Release) {
            unsafe {
                PostMessageW(self.internal_hwnd, WM_FERROKIT_WAKEUP, 0, 0);
            }
        }
    }
}

impl EventDispatcherHandle for Win32EventDispatcherHandle {
    fn wake_up(&self) {
        self.wake_up();
    }
}

unsafe impl Send for Win32EventDispatcherHandle {}
unsafe impl Sync for Win32EventDispatcherHandle {}

impl EventDispatcher for Win32EventDispatcher {
    fn wake_up(&self) {
        self.wake_up();
    }

    fn clone_handle(&self) -> Arc<dyn EventDispatcherHandle> {
        Arc::new(self.clone_handle())
    }

    fn install_native_event_filter(&self, filter: Box<dyn NativeEventFilter>) {
        self.install_native_event_filter(filter);
    }

    fn filter_native_event(
        &self,
        event_type: &str,
        msg: &NativeMessage,
        result: &mut isize,
    ) -> bool {
        self.filter_native_event(event_type, msg, result)
    }

    fn process_events(
        &self,
        can_wait: bool,
        next_timer_timeout: Option<Duration>,
    ) -> DispatchResult {
        self.process_events(can_wait, next_timer_timeout)
    }

    fn register_timer(&self, entry: &TimerEntry) {
        self.register_timer(entry);
    }

    fn unregister_timer(&self, entry: &TimerEntry) {
        self.unregister_timer(entry);
    }

    fn send_timer_events(&self, registry: &Mutex<TimerRegistry>) {
        self.send_timer_events(registry);
    }
}

unsafe impl Send for Win32EventDispatcher {}
unsafe impl Sync for Win32EventDispatcher {}
impl Drop for Win32EventDispatcher {
    fn drop(&mut self) {
        if !self.internal_hwnd.is_null() {
            let key = self.internal_hwnd as usize;
            let _ = WAKEUP_FLAGS.try_with(|flags| {
                flags.borrow_mut().remove(&key);
            });
            crate::timer::precise::disarm_all_for(self.internal_hwnd);
            unsafe {
                DestroyWindow(self.internal_hwnd);
            }
            self.internal_hwnd = ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::NonNull;

    #[test]
    fn test_cross_thread_wakeup() {
        let dispatcher = Win32EventDispatcher::new();
        let dispatcher_clone = dispatcher.clone_handle();

        let start = std::time::Instant::now();

        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            dispatcher_clone.wake_up();
        });

        let result = dispatcher.process_events(true, None);

        let elapsed = start.elapsed();
        assert_eq!(result, DispatchResult::Awoken);
        assert!(elapsed >= std::time::Duration::from_millis(45));
        assert!(elapsed < std::time::Duration::from_millis(150));
    }

    #[test]
    fn test_timeout_sleep() {
        let dispatcher = Win32EventDispatcher::new();
        let start = std::time::Instant::now();

        let result = dispatcher.process_events(true, Some(std::time::Duration::from_millis(50)));

        let elapsed = start.elapsed();
        assert_eq!(result, DispatchResult::Timeout);
        assert!(elapsed >= std::time::Duration::from_millis(45));
        assert!(elapsed < std::time::Duration::from_millis(100));
    }

    #[test]
    fn test_quit_message() {
        std::thread::spawn(|| {
            let dispatcher = Win32EventDispatcher::new();

            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostQuitMessage(42);
            }

            let result = dispatcher.process_events(false, None);
            assert_eq!(result, DispatchResult::Quit(42));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn test_high_frequency_wakeup_dedup() {
        let dispatcher = Win32EventDispatcher::new();
        let mut handles = Vec::new();

        for _ in 0..10 {
            let handle = dispatcher.clone_handle();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    handle.wake_up();
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let result = dispatcher.process_events(false, None);
        assert_eq!(result, DispatchResult::Awoken);
        assert!(!dispatcher.wakeup_pending.load(Ordering::Acquire));
    }

    #[test]
    fn test_register_timer_wm_timer_dispatch() {
        use crate::object::ObjectData;
        use crate::timer::TimerType;
        use std::sync::{Arc, Mutex};

        struct MockReceiver {
            data: ObjectData,
            hits: Arc<Mutex<Vec<u64>>>,
        }

        impl QObject for MockReceiver {
            fn object_data(&self) -> &ObjectData {
                &self.data
            }

            fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
            fn timer_event(&mut self, timer_id: u64) {
                self.hits.lock().unwrap().push(timer_id);
            }
        }

        let hits = Arc::new(Mutex::new(Vec::new()));
        let mut receiver = MockReceiver {
            data: ObjectData::new(ObjectId(7)),
            hits: Arc::clone(&hits),
        };
        let registry = Mutex::new(TimerRegistry::new());
        let id = registry
            .lock()
            .unwrap()
            .register(ObjectId(7), 20, TimerType::Precise, true);
        let entry = registry.lock().unwrap().get(id).unwrap().clone();

        let dispatcher = Win32EventDispatcher::new();
        dispatcher.register_timer(&entry);
        unsafe { crate::object::register_qobject_ptr(NonNull::new_unchecked(&raw mut receiver)) };

        let deadline = std::time::Instant::now() + Duration::from_millis(1500);
        while hits.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            let _ = dispatcher.process_events_with_timers(
                true,
                Some(Duration::from_millis(50)),
                &registry,
            );
        }

        let fired = hits.lock().unwrap().clone();
        assert!(!fired.is_empty(), "expected at least one WM_TIMER dispatch");
        assert!(fired.iter().all(|&h| h == id.0 as u64));

        let leftover = registry.lock().unwrap().get(id).cloned();
        if let Some(entry) = leftover {
            dispatcher.unregister_timer(&entry);
            registry.lock().unwrap().unregister(id);
        }
    }

    /// The raw route (`Timer::start_with`, `EventDispatcher::register_timer`) gives a timer the
    /// same native source as the thread-context route: a multimedia timer for `Precise`, a
    /// `SetTimer` timer for the coarse types.
    #[test]
    fn test_register_timer_native_source_follows_the_timer_type() {
        use crate::object::ObjectId;
        use crate::timer::{precise, TimerType};
        use windows_sys::Win32::Media::timeKillEvent;
        use windows_sys::Win32::UI::WindowsAndMessaging::KillTimer;

        let dispatcher = Win32EventDispatcher::new();
        let mut registry = TimerRegistry::new();

        let precise_id = registry.register(ObjectId(1), 20, TimerType::Precise, false);
        let precise_entry = registry.get(precise_id).unwrap().clone();
        dispatcher.register_timer(&precise_entry);
        let handle = precise::native_handle(precise_id)
            .expect("a Precise timer started on the raw route is a multimedia timer");
        assert_eq!(
            unsafe { KillTimer(dispatcher.internal_hwnd, precise_id.0 as usize) },
            0,
            "and not also a SetTimer timer"
        );
        dispatcher.unregister_timer(&precise_entry);
        assert_eq!(precise::native_handle(precise_id), None);
        // TIMERR_NOCANDO: the multimedia timer is gone.
        assert_eq!(unsafe { timeKillEvent(handle) }, 97);

        let coarse_id = registry.register(ObjectId(1), 100, TimerType::Coarse, false);
        let coarse_entry = registry.get(coarse_id).unwrap().clone();
        dispatcher.register_timer(&coarse_entry);
        assert_eq!(precise::native_handle(coarse_id), None);
        assert_ne!(
            unsafe { KillTimer(dispatcher.internal_hwnd, coarse_id.0 as usize) },
            0,
            "a coarse timer keeps its SetTimer timer"
        );
    }

    #[test]
    fn test_native_event_filter_intercept_in_dispatcher() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_USER};

        struct HotkeyFilter {
            intercepted: Arc<AtomicBool>,
        }
        impl NativeEventFilter for HotkeyFilter {
            fn native_event_filter(
                &mut self,
                event_type: &str,
                msg: &NativeMessage,
                _result: &mut isize,
            ) -> bool {
                if event_type == "windows_generic_MSG" {
                    if let NativeMessage::Windows(m) = msg {
                        if m.message == WM_USER + 77 {
                            self.intercepted.store(true, Ordering::Release);
                            return true;
                        }
                    }
                }
                false
            }
        }

        let dispatcher = Win32EventDispatcher::new();
        let intercepted = Arc::new(AtomicBool::new(false));

        dispatcher.install_native_event_filter(Box::new(HotkeyFilter {
            intercepted: Arc::clone(&intercepted),
        }));

        unsafe {
            PostMessageW(dispatcher.internal_hwnd, WM_USER + 77, 0, 0);
        }

        let res = dispatcher.process_events(false, None);
        assert!(matches!(
            res,
            DispatchResult::Normal | DispatchResult::Timeout
        ));

        assert!(intercepted.load(Ordering::Acquire));
    }

    /// Removes and counts the `WM_FERROKIT_WAKEUP` messages queued for `hwnd`, returning the last.
    fn take_wakeup_messages(hwnd: HWND) -> (usize, Option<MSG>) {
        let mut count = 0;
        let mut last = None;
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, hwnd, WM_FERROKIT_WAKEUP, WM_FERROKIT_WAKEUP, PM_REMOVE) != 0 {
                count += 1;
                last = Some(msg);
            }
        }
        (count, last)
    }

    fn counting_metacall(counter: &Arc<std::sync::atomic::AtomicUsize>) -> crate::event::Event {
        let counter = Arc::clone(counter);
        crate::event::Event::new(crate::event::EventKind::MetaCall(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })))
    }

    #[test]
    fn test_wakeup_message_is_deduplicated_and_pumped_by_wnd_proc() {
        let el = crate::event_loop::EventLoop::new();
        let hwnd = el.dispatcher().internal_hwnd;
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        el.post_event(ObjectId(0), counting_metacall(&delivered));
        el.post_event(ObjectId(0), counting_metacall(&delivered));

        let (count, msg) = take_wakeup_messages(hwnd);
        assert_eq!(count, 1, "two posts share one wake-up message");
        assert!(el.dispatcher().wakeup_pending.load(Ordering::Acquire));

        // Simulate the native modal loop dispatching the wake-up message itself.
        unsafe {
            DispatchMessageW(&msg.unwrap());
        }
        assert_eq!(delivered.load(Ordering::SeqCst), 2);
        assert!(
            !el.dispatcher().wakeup_pending.load(Ordering::Acquire),
            "wnd_proc must clear wakeup_pending"
        );

        // A later post wakes again (pending was reset).
        el.post_event(ObjectId(0), counting_metacall(&delivered));
        let (count, msg) = take_wakeup_messages(hwnd);
        assert_eq!(count, 1);
        unsafe {
            DispatchMessageW(&msg.unwrap());
        }
        assert_eq!(delivered.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_wnd_proc_pump_defers_events_posted_during_dispatch() {
        let el = crate::event_loop::EventLoop::new();
        let hwnd = el.dispatcher().internal_hwnd;
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let inner = Arc::clone(&delivered);
        el.post_event(
            ObjectId(0),
            crate::event::Event::new(crate::event::EventKind::MetaCall(Box::new(move |_| {
                // Posts during dispatch: lands in the next turn and triggers a new wake-up.
                crate::event_loop::post_event_to_thread(
                    crate::object::ThreadId::current(),
                    ObjectId(0),
                    counting_metacall(&inner),
                );
            }))),
        );

        let (_, msg) = take_wakeup_messages(hwnd);
        unsafe {
            DispatchMessageW(&msg.unwrap());
        }
        assert_eq!(
            delivered.load(Ordering::SeqCst),
            0,
            "same-turn post not delivered"
        );

        let (count, msg) = take_wakeup_messages(hwnd);
        assert_eq!(count, 1, "new post re-armed the wake-up");
        unsafe {
            DispatchMessageW(&msg.unwrap());
        }
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_modal_pump_keeping_a_deferred_delete_rearms_the_wake_up() {
        // Loop level 1 makes the stamp (0, 0) deliverable (qcoreapplication.cpp:1880-1885).
        crate::object::ThreadContext::set_current_loop_level(1);
        let el = crate::event_loop::EventLoop::new();
        let hwnd = el.dispatcher().internal_hwnd;

        // A callback of the normal pump opens a native modal loop, as a menu popup does.
        el.post_event(
            ObjectId(0),
            crate::event::Event::new(crate::event::EventKind::MetaCall(Box::new(|_| {
                let _ = crate::event_loop::pump_posted_events_modal();
            }))),
        );
        // Owned by this loop level: the modal pump keeps it queued for the next turn.
        el.post_event(
            ObjectId(900_004),
            crate::event::Event::new(crate::event::EventKind::DeferredDelete {
                loop_level: 0,
                scope_level: 0,
            }),
        );
        // Consume the wake-up (clears `wakeup_pending`) without running the window procedure.
        let _ = el.dispatcher().process_events(false, None);
        assert_eq!(take_wakeup_messages(hwnd).0, 0);

        el.send_posted_events();
        assert_eq!(
            take_wakeup_messages(hwnd).0,
            1,
            "the kept deferred delete re-arms the wake-up"
        );
        crate::object::ThreadContext::set_current_loop_level(0);
    }

    #[test]
    fn test_modal_pump_keeping_a_loop_blocked_deferred_delete_does_not_rearm_the_wake_up() {
        let el = crate::event_loop::EventLoop::new();
        let hwnd = el.dispatcher().internal_hwnd;
        crate::object::ThreadContext::set_current_loop_level(2);

        el.post_event(
            ObjectId(0),
            crate::event::Event::new(crate::event::EventKind::MetaCall(Box::new(|_| {
                let _ = crate::event_loop::pump_posted_events_modal();
            }))),
        );
        // Requested by an outer loop level: this loop level (2) cannot deliver it.
        el.post_event(
            ObjectId(900_004),
            crate::event::Event::new(crate::event::EventKind::DeferredDelete {
                loop_level: 1,
                scope_level: 0,
            }),
        );
        let _ = el.dispatcher().process_events(false, None);
        assert_eq!(take_wakeup_messages(hwnd).0, 0);

        el.send_posted_events();
        assert_eq!(
            take_wakeup_messages(hwnd).0,
            0,
            "a loop-blocked deferred delete must not re-arm the wake-up (no busy spin)"
        );
        crate::object::ThreadContext::set_current_loop_level(0);
    }

    #[test]
    fn early_wm_timer_for_u64_max_does_not_fire_or_consume_single_shot_and_keeps_waiting() {
        use crate::timer::{Timer, TimerType, WIN32_TIMER_MAX_MS};
        use std::sync::atomic::AtomicUsize;

        let el = crate::event_loop::EventLoop::new();
        let hwnd = el.dispatcher().internal_hwnd;

        let rep_hits = Arc::new(AtomicUsize::new(0));
        let single_hits = Arc::new(AtomicUsize::new(0));
        let fn_single_hits = Arc::new(AtomicUsize::new(0));

        let rep_timer = Timer::new();
        rep_timer.set_timer_type(TimerType::Precise);
        {
            let h = Arc::clone(&rep_hits);
            rep_timer.timeout.connect(move |()| {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        let rep_id = rep_timer.start_with_interval(u64::MAX);

        let single_timer = Timer::new();
        single_timer.set_timer_type(TimerType::Precise);
        single_timer.set_single_shot(true);
        {
            let h = Arc::clone(&single_hits);
            single_timer.timeout.connect(move |()| {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        let single_id = single_timer.start_with_interval(u64::MAX);

        {
            let h = Arc::clone(&fn_single_hits);
            Timer::single_shot(u64::MAX, move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        let (fn_single_id, fn_single_deadline) = {
            let reg = el.timer_registry().lock().unwrap();
            assert_eq!(reg.len(), 3);
            let id = reg
                .expired_timers(u64::MAX)
                .into_iter()
                .find(|&id| id != rep_id && id != single_id)
                .expect("Timer::single_shot entry must be registered");
            let deadline = reg.get(id).unwrap().next_fire_ms;
            (id, deadline)
        };

        // Simulate early WM_TIMER notifications at USER_TIMER_MAXIMUM (0x7FFF_FFFF) and
        // 2 * USER_TIMER_MAXIMUM (where >= WIN32_TIMER_MAX_MS remains for all three timers),
        // and then 1 ms before each timer's deadline (`u64::MAX - 1` for Precise, and
        // `fn_single_deadline - 1` for VeryCoarse `Timer::single_shot`).
        for &now_ms in &[WIN32_TIMER_MAX_MS, WIN32_TIMER_MAX_MS * 2] {
            assert_eq!(
                dispatch_thread_timer_at(rep_id.0, hwnd, now_ms, now_ms),
                Some(WIN32_TIMER_MAX_MS as u32)
            );
            assert_eq!(
                dispatch_thread_timer_at(single_id.0, hwnd, now_ms, now_ms),
                Some(WIN32_TIMER_MAX_MS as u32)
            );
            assert_eq!(
                dispatch_thread_timer_at(fn_single_id.0, hwnd, now_ms, now_ms),
                Some(WIN32_TIMER_MAX_MS as u32)
            );
        }
        assert_eq!(
            dispatch_thread_timer_at(rep_id.0, hwnd, u64::MAX - 1, u64::MAX - 1),
            Some(1)
        );
        assert_eq!(
            dispatch_thread_timer_at(single_id.0, hwnd, u64::MAX - 1, u64::MAX - 1),
            Some(1)
        );
        assert_eq!(
            dispatch_thread_timer_at(
                fn_single_id.0,
                hwnd,
                fn_single_deadline - 1,
                fn_single_deadline - 1,
            ),
            Some(1)
        );

        assert_eq!(rep_hits.load(Ordering::SeqCst), 0);
        assert_eq!(single_hits.load(Ordering::SeqCst), 0);
        assert_eq!(fn_single_hits.load(Ordering::SeqCst), 0);
        assert!(rep_timer.is_active());
        assert!(single_timer.is_active());
        assert_eq!(el.timer_registry().lock().unwrap().len(), 3);
        assert!(
            el.next_timeout().is_some(),
            "event loop must keep waiting for the remaining interval"
        );

        // Also verify `send_timer_events_at` on `PENDING_WM_TIMERS` ignores an unexpired tick
        // without consuming the single-shot timer.
        PENDING_WM_TIMERS.with(|q| q.borrow_mut().push(single_id.0));
        assert_eq!(
            el.dispatcher().send_timer_events_at(
                el.timer_registry(),
                WIN32_TIMER_MAX_MS,
                WIN32_TIMER_MAX_MS,
            ),
            vec![WIN32_TIMER_MAX_MS as u32]
        );
        assert_eq!(single_hits.load(Ordering::SeqCst), 0);
        assert!(single_timer.is_active());

        drop(rep_timer);
        drop(single_timer);
        el.timer_registry().lock().unwrap().unregister(fn_single_id);
    }

    #[test]
    fn multi_slice_wm_timer_waits_for_remaining_interval_then_fires_at_deadline() {
        use crate::timer::{Timer, TimerType, WIN32_TIMER_MAX_MS};
        use std::sync::atomic::AtomicUsize;

        let el = crate::event_loop::EventLoop::new();
        let hwnd = el.dispatcher().internal_hwnd;
        let long_interval: u64 = 3_000_000_000; // > USER_TIMER_MAXIMUM (2_147_483_647)

        let rep_hits = Arc::new(AtomicUsize::new(0));
        let single_hits = Arc::new(AtomicUsize::new(0));

        let rep_timer = Timer::new();
        rep_timer.set_timer_type(TimerType::Precise);
        {
            let h = Arc::clone(&rep_hits);
            rep_timer.timeout.connect(move |()| {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        let rep_id = rep_timer.start_with_interval(long_interval);

        let single_timer = Timer::new();
        single_timer.set_timer_type(TimerType::Precise);
        single_timer.set_single_shot(true);
        {
            let h = Arc::clone(&single_hits);
            single_timer.timeout.connect(move |()| {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        let single_id = single_timer.start_with_interval(long_interval);

        let (rep_deadline, single_deadline) = {
            let reg = el.timer_registry().lock().unwrap();
            (
                reg.get(rep_id).unwrap().next_fire_ms,
                reg.get(single_id).unwrap().next_fire_ms,
            )
        };

        // 1. First WM_TIMER arrives after the initial 0x7FFF_FFFF cap: 852,516,353 ms remain.
        let remaining: u64 = long_interval - WIN32_TIMER_MAX_MS;
        assert_eq!(remaining, 852_516_353);
        assert_eq!(
            dispatch_thread_timer_at(
                rep_id.0,
                hwnd,
                rep_deadline - remaining,
                rep_deadline - remaining,
            ),
            Some(remaining as u32),
            "repeating timer must re-arm SetTimer for the remaining 852,516,353 ms"
        );
        assert_eq!(
            dispatch_thread_timer_at(
                single_id.0,
                hwnd,
                single_deadline - remaining,
                single_deadline - remaining,
            ),
            Some(remaining as u32),
            "single-shot timer must re-arm SetTimer for the remaining 852,516,353 ms without being consumed"
        );
        assert_eq!(rep_hits.load(Ordering::SeqCst), 0);
        assert_eq!(single_hits.load(Ordering::SeqCst), 0);
        assert!(rep_timer.is_active());
        assert!(single_timer.is_active());
        assert_eq!(
            el.timer_registry()
                .lock()
                .unwrap()
                .get(rep_id)
                .unwrap()
                .next_fire_ms,
            rep_deadline
        );
        assert_eq!(
            el.timer_registry()
                .lock()
                .unwrap()
                .get(single_id)
                .unwrap()
                .next_fire_ms,
            single_deadline
        );

        // 2. Second WM_TIMER arrives at the deadline: both callbacks fire once, single-shot is
        //    consumed, and repeating timer advances to `rep_deadline + long_interval` and re-arms
        //    SetTimer back to `WIN32_TIMER_MAX_MS`.
        assert_eq!(
            dispatch_thread_timer_at(rep_id.0, hwnd, rep_deadline, rep_deadline),
            Some(WIN32_TIMER_MAX_MS as u32)
        );
        assert_eq!(
            dispatch_thread_timer_at(single_id.0, hwnd, single_deadline, single_deadline),
            None
        );
        assert_eq!(rep_hits.load(Ordering::SeqCst), 1);
        assert_eq!(single_hits.load(Ordering::SeqCst), 1);
        assert!(rep_timer.is_active());
        assert!(
            !single_timer.is_active()
                && el.timer_registry().lock().unwrap().get(single_id).is_none(),
            "single-shot timer is unregistered only after reaching its deadline"
        );
        assert_eq!(
            el.timer_registry()
                .lock()
                .unwrap()
                .get(rep_id)
                .unwrap()
                .next_fire_ms,
            rep_deadline + long_interval
        );

        // 3. Intermediate WM_TIMER during the second period is again ignored and re-armed for the
        //    remaining slice, then fires on the second deadline.
        let second_deadline = rep_deadline + long_interval;
        assert_eq!(
            dispatch_thread_timer_at(
                rep_id.0,
                hwnd,
                second_deadline - remaining,
                second_deadline - remaining,
            ),
            Some(remaining as u32)
        );
        assert_eq!(rep_hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            dispatch_thread_timer_at(rep_id.0, hwnd, second_deadline, second_deadline),
            Some(WIN32_TIMER_MAX_MS as u32)
        );
        assert_eq!(rep_hits.load(Ordering::SeqCst), 2);

        drop(rep_timer);
    }

    #[test]
    fn post_callback_rearm_deducts_callback_duration_from_next_wait_interval() {
        use crate::timer::{Timer, TimerType};
        use std::sync::atomic::AtomicUsize;

        let el = crate::event_loop::EventLoop::new();
        let hwnd = el.dispatcher().internal_hwnd;
        let long_interval: u64 = 3_000_000_000; // > USER_TIMER_MAXIMUM (2_147_483_647)
        let callback_duration: u64 = 900_000_000;
        // Remaining wait after a 900,000,000 ms callback: 3,000,000,000 - 900,000,000 = 2,100,000,000 ms
        // (< WIN32_TIMER_MAX_MS = 2,147_483,647 ms). Reusing pre-callback `now_ms` would wrongly
        // re-arm for WIN32_TIMER_MAX_MS instead of 2,100,000,000 ms.
        let expected_post_cb_wait: u32 = (long_interval - callback_duration) as u32;
        assert_eq!(expected_post_cb_wait, 2_100_000_000);

        let hits = Arc::new(AtomicUsize::new(0));
        let timer = Timer::new();
        timer.set_timer_type(TimerType::Precise);
        {
            let h = Arc::clone(&hits);
            timer.timeout.connect(move |()| {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        let id = timer.start_with_interval(long_interval);
        let first_deadline = el
            .timer_registry()
            .lock()
            .unwrap()
            .get(id)
            .unwrap()
            .next_fire_ms;

        // 1. `dispatch_thread_timer_at`: pre-callback `now_ms = first_deadline`, post-callback
        //    `rearm_now_ms = first_deadline + callback_duration`.
        let rearmed = dispatch_thread_timer_at(
            id.0,
            hwnd,
            first_deadline,
            first_deadline + callback_duration,
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            el.timer_registry()
                .lock()
                .unwrap()
                .get(id)
                .unwrap()
                .next_fire_ms,
            first_deadline + long_interval
        );
        assert_eq!(
            rearmed,
            Some(expected_post_cb_wait),
            "dispatch_thread_timer_at must deduct callback duration using post-callback timestamp"
        );

        // 2. `Win32EventDispatcher::send_timer_events_at`: pre-callback `now_ms = second_deadline`,
        //    post-callback `rearm_now_ms = second_deadline + callback_duration`.
        let second_deadline = first_deadline + long_interval;
        PENDING_WM_TIMERS.with(|q| q.borrow_mut().push(id.0));
        let rearmed_vec = el.dispatcher().send_timer_events_at(
            el.timer_registry(),
            second_deadline,
            second_deadline + callback_duration,
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(
            el.timer_registry()
                .lock()
                .unwrap()
                .get(id)
                .unwrap()
                .next_fire_ms,
            second_deadline + long_interval
        );
        assert_eq!(
            rearmed_vec,
            vec![expected_post_cb_wait],
            "send_timer_events_at must deduct callback duration using post-callback timestamp"
        );

        // 3. When the callback duration exceeds the entire next period (`rearm_now_ms >= next_fire_ms`),
        //    the re-armed delay clamps to `1` ms (`saturating_sub(rearm_now_ms).max(1)`).
        let third_deadline = second_deadline + long_interval;
        let fourth_deadline = third_deadline + long_interval;
        PENDING_WM_TIMERS.with(|q| q.borrow_mut().push(id.0));
        let overrun_vec = el.dispatcher().send_timer_events_at(
            el.timer_registry(),
            third_deadline,
            fourth_deadline + 500,
        );
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        assert_eq!(overrun_vec, vec![1]);
        // The callback's duration is deducted from the wait once: the absolute deadline
        // stays based on the pre-callback timestamp, so it is never pushed back by it.
        assert_eq!(
            el.timer_registry()
                .lock()
                .unwrap()
                .get(id)
                .unwrap()
                .next_fire_ms,
            fourth_deadline
        );

        drop(timer);
    }
}
