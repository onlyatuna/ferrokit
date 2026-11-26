// SPDX-License-Identifier: LGPL-3.0-only
pub mod macos;

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogIcon {
    Info,
    Warning,
    Error,
    Question,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageBoxButtons {
    Ok,
    OkCancel,
    YesNo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageBoxResult {
    Ok,
    Cancel,
    Yes,
    No,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    pub patterns: Vec<String>,
    pub description: String,
}

/// Synchronous native dialog operations provided by the platform backend.
///
/// On macOS every method must be called on the main thread (AppKit constraint) and panics
/// otherwise, before any dialog is created. Other platforms accept calls from any thread.
pub trait PlatformDialogs: Send + Sync {
    fn message_box(
        &self,
        title: &str,
        message: &str,
        icon: DialogIcon,
        buttons: MessageBoxButtons,
    ) -> MessageBoxResult;

    fn open_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf>;

    fn open_files(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<Vec<PathBuf>>;

    fn save_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf>;

    fn select_folder(&self, title: &str, initial_path: &str) -> Option<PathBuf>;

    /// Whether this backend may safely execute dialogs on a background worker thread.
    ///
    /// Returns `true` by default (`SystemDialogs` / `tinyfiledialogs`). Backends with main-thread
    /// affinity (such as AppKit on macOS) override this to `false`.
    fn runs_on_worker_thread(&self) -> bool {
        true
    }
}

/// Native dialog backend backed by `tinyfiledialogs`.
///
/// Prefer the non-blocking `*_async` functions (or [`AsyncDialogs`]) from GUI threads so modal
/// dialogs run on a worker thread without blocking the event loop.
#[derive(Debug, Default)]
pub struct SystemDialogs;

impl SystemDialogs {
    fn native_icon(icon: DialogIcon) -> tinyfiledialogs::MessageBoxIcon {
        match icon {
            DialogIcon::Info => tinyfiledialogs::MessageBoxIcon::Info,
            DialogIcon::Warning => tinyfiledialogs::MessageBoxIcon::Warning,
            DialogIcon::Error => tinyfiledialogs::MessageBoxIcon::Error,
            DialogIcon::Question => tinyfiledialogs::MessageBoxIcon::Question,
        }
    }

    fn filter_parts(filter: Option<&FileFilter>) -> Option<(Vec<&str>, &str)> {
        filter.map(|filter| {
            (
                filter.patterns.iter().map(String::as_str).collect(),
                filter.description.as_str(),
            )
        })
    }
}

/// Serializes calls into `tinyfiledialogs`, which stores dialog results in an internal C static
/// buffer that is not safe for concurrent access across worker threads.
static TINYFILEDIALOGS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl PlatformDialogs for SystemDialogs {
    fn message_box(
        &self,
        title: &str,
        message: &str,
        icon: DialogIcon,
        buttons: MessageBoxButtons,
    ) -> MessageBoxResult {
        let icon = Self::native_icon(icon);
        let _guard = ferrokit_core::poison::lock_unpoisoned(&TINYFILEDIALOGS_LOCK);
        match buttons {
            MessageBoxButtons::Ok => {
                tinyfiledialogs::message_box_ok(title, message, icon);
                MessageBoxResult::Ok
            }
            MessageBoxButtons::OkCancel => {
                match tinyfiledialogs::message_box_ok_cancel(
                    title,
                    message,
                    icon,
                    tinyfiledialogs::OkCancel::Ok,
                ) {
                    tinyfiledialogs::OkCancel::Ok => MessageBoxResult::Ok,
                    tinyfiledialogs::OkCancel::Cancel => MessageBoxResult::Cancel,
                }
            }
            MessageBoxButtons::YesNo => {
                match tinyfiledialogs::message_box_yes_no(
                    title,
                    message,
                    icon,
                    tinyfiledialogs::YesNo::No,
                ) {
                    tinyfiledialogs::YesNo::Yes => MessageBoxResult::Yes,
                    tinyfiledialogs::YesNo::No => MessageBoxResult::No,
                }
            }
        }
    }

    fn open_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf> {
        let filter = Self::filter_parts(filter);
        let _guard = ferrokit_core::poison::lock_unpoisoned(&TINYFILEDIALOGS_LOCK);
        tinyfiledialogs::open_file_dialog(
            title,
            initial_path,
            filter.as_ref().map(|(p, d)| (&p[..], *d)),
        )
        .map(PathBuf::from)
    }

    fn open_files(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<Vec<PathBuf>> {
        let filter = Self::filter_parts(filter);
        let _guard = ferrokit_core::poison::lock_unpoisoned(&TINYFILEDIALOGS_LOCK);
        tinyfiledialogs::open_file_dialog_multi(
            title,
            initial_path,
            filter.as_ref().map(|(p, d)| (&p[..], *d)),
        )
        .map(|paths| paths.into_iter().map(PathBuf::from).collect())
    }

    fn save_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf> {
        let filter = Self::filter_parts(filter);
        let _guard = ferrokit_core::poison::lock_unpoisoned(&TINYFILEDIALOGS_LOCK);
        tinyfiledialogs::save_file_dialog_with_filter(
            title,
            initial_path,
            filter.as_ref().map(|(p, _)| &p[..]).unwrap_or(&[]),
            filter.as_ref().map(|(_, d)| *d).unwrap_or("All files"),
        )
        .map(PathBuf::from)
    }

    fn select_folder(&self, title: &str, initial_path: &str) -> Option<PathBuf> {
        let _guard = ferrokit_core::poison::lock_unpoisoned(&TINYFILEDIALOGS_LOCK);
        tinyfiledialogs::select_folder_dialog(title, initial_path).map(PathBuf::from)
    }
}

#[cfg(not(target_os = "macos"))]
static SYSTEM_DIALOGS: SystemDialogs = SystemDialogs;
#[cfg(target_os = "macos")]
static COCOA_DIALOGS: crate::dialogs::macos::CocoaDialogs<crate::dialogs::macos::NativeRuntime> =
    crate::dialogs::macos::CocoaDialogs::new(crate::dialogs::macos::NativeRuntime);

/// Returns the OS-native dialog implementation (AppKit panels and alerts on macOS).
pub fn platform_dialogs() -> &'static dyn PlatformDialogs {
    #[cfg(target_os = "macos")]
    {
        &COCOA_DIALOGS
    }
    #[cfg(not(target_os = "macos"))]
    {
        &SYSTEM_DIALOGS
    }
}

/// Handle for an in-flight non-blocking native dialog launched via [`AsyncDialogs`] or the
/// `*_async` helpers.
pub struct DialogHandle<T> {
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    rx: std::sync::mpsc::Receiver<T>,
}

impl<T> DialogHandle<T> {
    /// Returns `true` once the dialog has completed and its result is ready.
    pub fn is_finished(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Returns the dialog result without blocking if it has completed.
    pub fn try_recv(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }

    /// Blocks until the dialog completes and returns its result.
    pub fn wait(self) -> Option<T> {
        self.rx.recv().ok()
    }
}

fn dispatch_dialog_async<T, F, C>(runs_on_worker: bool, work: F, on_done: C) -> DialogHandle<T>
where
    T: Clone + Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    C: FnOnce(T) + Send + 'static,
{
    use ferrokit_core::event::{Event, EventKind};
    use ferrokit_core::event_loop::post_event_to_thread;
    use ferrokit_core::object::{ObjectId, ThreadId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    let caller_thread = ThreadId::current();
    let (tx, rx) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let finished_clone = Arc::clone(&finished);

    if runs_on_worker {
        let task = move || {
            let result = work();
            let _ = tx.send(result.clone());
            post_event_to_thread(
                caller_thread,
                ObjectId(0),
                Event::new(EventKind::MetaCall(Box::new(move |_| on_done(result)))),
            );
            finished_clone.store(true, Ordering::Release);
        };
        let _ = std::thread::Builder::new()
            .name("ferrokit-dialog".into())
            .spawn(task);
    } else {
        post_event_to_thread(
            caller_thread,
            ObjectId(0),
            Event::new(EventKind::MetaCall(Box::new(move |_| {
                let result = work();
                let _ = tx.send(result.clone());
                finished_clone.store(true, Ordering::Release);
                on_done(result);
            }))),
        );
    }

    DialogHandle { finished, rx }
}

/// Non-blocking wrapper around any [`PlatformDialogs`] backend.
///
/// Backends where [`PlatformDialogs::runs_on_worker_thread`] is `true` run on a worker thread and
/// post results back to the calling thread's event loop. Main-thread-only backends (such as macOS
/// AppKit) defer execution to the calling thread's event loop so the current handler returns
/// immediately.
#[derive(Clone)]
pub struct AsyncDialogs {
    backend: std::sync::Arc<dyn PlatformDialogs>,
}

impl AsyncDialogs {
    pub fn new(backend: std::sync::Arc<dyn PlatformDialogs>) -> Self {
        Self { backend }
    }

    pub fn message_box(
        &self,
        title: impl Into<String>,
        message: impl Into<String>,
        icon: DialogIcon,
        buttons: MessageBoxButtons,
        on_done: impl FnOnce(MessageBoxResult) + Send + 'static,
    ) -> DialogHandle<MessageBoxResult> {
        let backend = std::sync::Arc::clone(&self.backend);
        let title = title.into();
        let message = message.into();
        dispatch_dialog_async(
            backend.runs_on_worker_thread(),
            move || backend.message_box(&title, &message, icon, buttons),
            on_done,
        )
    }

    pub fn open_file(
        &self,
        title: impl Into<String>,
        initial_path: impl Into<String>,
        filter: Option<FileFilter>,
        on_done: impl FnOnce(Option<PathBuf>) + Send + 'static,
    ) -> DialogHandle<Option<PathBuf>> {
        let backend = std::sync::Arc::clone(&self.backend);
        let title = title.into();
        let initial_path = initial_path.into();
        dispatch_dialog_async(
            backend.runs_on_worker_thread(),
            move || backend.open_file(&title, &initial_path, filter.as_ref()),
            on_done,
        )
    }

    pub fn open_files(
        &self,
        title: impl Into<String>,
        initial_path: impl Into<String>,
        filter: Option<FileFilter>,
        on_done: impl FnOnce(Option<Vec<PathBuf>>) + Send + 'static,
    ) -> DialogHandle<Option<Vec<PathBuf>>> {
        let backend = std::sync::Arc::clone(&self.backend);
        let title = title.into();
        let initial_path = initial_path.into();
        dispatch_dialog_async(
            backend.runs_on_worker_thread(),
            move || backend.open_files(&title, &initial_path, filter.as_ref()),
            on_done,
        )
    }

    pub fn save_file(
        &self,
        title: impl Into<String>,
        initial_path: impl Into<String>,
        filter: Option<FileFilter>,
        on_done: impl FnOnce(Option<PathBuf>) + Send + 'static,
    ) -> DialogHandle<Option<PathBuf>> {
        let backend = std::sync::Arc::clone(&self.backend);
        let title = title.into();
        let initial_path = initial_path.into();
        dispatch_dialog_async(
            backend.runs_on_worker_thread(),
            move || backend.save_file(&title, &initial_path, filter.as_ref()),
            on_done,
        )
    }

    pub fn select_folder(
        &self,
        title: impl Into<String>,
        initial_path: impl Into<String>,
        on_done: impl FnOnce(Option<PathBuf>) + Send + 'static,
    ) -> DialogHandle<Option<PathBuf>> {
        let backend = std::sync::Arc::clone(&self.backend);
        let title = title.into();
        let initial_path = initial_path.into();
        dispatch_dialog_async(
            backend.runs_on_worker_thread(),
            move || backend.select_folder(&title, &initial_path),
            on_done,
        )
    }
}

/// Non-blocking [`PlatformDialogs::message_box`] using [`platform_dialogs`].
pub fn message_box_async(
    title: impl Into<String>,
    message: impl Into<String>,
    icon: DialogIcon,
    buttons: MessageBoxButtons,
    on_done: impl FnOnce(MessageBoxResult) + Send + 'static,
) -> DialogHandle<MessageBoxResult> {
    let backend = platform_dialogs();
    let title = title.into();
    let message = message.into();
    dispatch_dialog_async(
        backend.runs_on_worker_thread(),
        move || backend.message_box(&title, &message, icon, buttons),
        on_done,
    )
}

/// Non-blocking [`PlatformDialogs::open_file`] using [`platform_dialogs`].
pub fn open_file_async(
    title: impl Into<String>,
    initial_path: impl Into<String>,
    filter: Option<FileFilter>,
    on_done: impl FnOnce(Option<PathBuf>) + Send + 'static,
) -> DialogHandle<Option<PathBuf>> {
    let backend = platform_dialogs();
    let title = title.into();
    let initial_path = initial_path.into();
    dispatch_dialog_async(
        backend.runs_on_worker_thread(),
        move || backend.open_file(&title, &initial_path, filter.as_ref()),
        on_done,
    )
}

/// Non-blocking [`PlatformDialogs::open_files`] using [`platform_dialogs`].
pub fn open_files_async(
    title: impl Into<String>,
    initial_path: impl Into<String>,
    filter: Option<FileFilter>,
    on_done: impl FnOnce(Option<Vec<PathBuf>>) + Send + 'static,
) -> DialogHandle<Option<Vec<PathBuf>>> {
    let backend = platform_dialogs();
    let title = title.into();
    let initial_path = initial_path.into();
    dispatch_dialog_async(
        backend.runs_on_worker_thread(),
        move || backend.open_files(&title, &initial_path, filter.as_ref()),
        on_done,
    )
}

/// Non-blocking [`PlatformDialogs::save_file`] using [`platform_dialogs`].
pub fn save_file_async(
    title: impl Into<String>,
    initial_path: impl Into<String>,
    filter: Option<FileFilter>,
    on_done: impl FnOnce(Option<PathBuf>) + Send + 'static,
) -> DialogHandle<Option<PathBuf>> {
    let backend = platform_dialogs();
    let title = title.into();
    let initial_path = initial_path.into();
    dispatch_dialog_async(
        backend.runs_on_worker_thread(),
        move || backend.save_file(&title, &initial_path, filter.as_ref()),
        on_done,
    )
}

/// Non-blocking [`PlatformDialogs::select_folder`] using [`platform_dialogs`].
pub fn select_folder_async(
    title: impl Into<String>,
    initial_path: impl Into<String>,
    on_done: impl FnOnce(Option<PathBuf>) + Send + 'static,
) -> DialogHandle<Option<PathBuf>> {
    let backend = platform_dialogs();
    let title = title.into();
    let initial_path = initial_path.into();
    dispatch_dialog_async(
        backend.runs_on_worker_thread(),
        move || backend.select_folder(&title, &initial_path),
        on_done,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrokit_core::event_loop::EventLoop;
    use ferrokit_core::object::ThreadId;
    use ferrokit_core::poison::lock_unpoisoned;
    use std::sync::{Arc, Barrier, Mutex};

    struct BlockingWorkerDialogs {
        gate: Arc<Barrier>,
    }

    impl PlatformDialogs for BlockingWorkerDialogs {
        fn message_box(
            &self,
            _title: &str,
            _message: &str,
            _icon: DialogIcon,
            _buttons: MessageBoxButtons,
        ) -> MessageBoxResult {
            MessageBoxResult::Ok
        }

        fn open_file(
            &self,
            _title: &str,
            initial_path: &str,
            _filter: Option<&FileFilter>,
        ) -> Option<PathBuf> {
            self.gate.wait();
            Some(PathBuf::from(initial_path).join("picked.txt"))
        }

        fn open_files(
            &self,
            _title: &str,
            _initial_path: &str,
            _filter: Option<&FileFilter>,
        ) -> Option<Vec<PathBuf>> {
            None
        }

        fn save_file(
            &self,
            _title: &str,
            _initial_path: &str,
            _filter: Option<&FileFilter>,
        ) -> Option<PathBuf> {
            None
        }

        fn select_folder(&self, _title: &str, _initial_path: &str) -> Option<PathBuf> {
            None
        }
    }

    struct MainThreadDialogs {
        ran_on: Arc<Mutex<Option<ThreadId>>>,
    }

    impl PlatformDialogs for MainThreadDialogs {
        fn runs_on_worker_thread(&self) -> bool {
            false
        }

        fn message_box(
            &self,
            _title: &str,
            _message: &str,
            _icon: DialogIcon,
            _buttons: MessageBoxButtons,
        ) -> MessageBoxResult {
            *lock_unpoisoned(&self.ran_on) = Some(ThreadId::current());
            MessageBoxResult::Yes
        }

        fn open_file(
            &self,
            _title: &str,
            _initial_path: &str,
            _filter: Option<&FileFilter>,
        ) -> Option<PathBuf> {
            None
        }

        fn open_files(
            &self,
            _title: &str,
            _initial_path: &str,
            _filter: Option<&FileFilter>,
        ) -> Option<Vec<PathBuf>> {
            None
        }

        fn save_file(
            &self,
            _title: &str,
            _initial_path: &str,
            _filter: Option<&FileFilter>,
        ) -> Option<PathBuf> {
            None
        }

        fn select_folder(&self, _title: &str, _initial_path: &str) -> Option<PathBuf> {
            None
        }
    }

    #[test]
    fn worker_thread_dialog_does_not_block_caller_and_delivers_on_caller_thread() {
        let event_loop = EventLoop::new();
        let caller = ThreadId::current();
        let gate = Arc::new(Barrier::new(2));
        let dialogs = AsyncDialogs::new(Arc::new(BlockingWorkerDialogs {
            gate: Arc::clone(&gate),
        }));

        let delivered = Arc::new(Mutex::new(None));
        let delivered_cb = Arc::clone(&delivered);

        let handle = dialogs.open_file("Open", "base", None, move |picked| {
            *lock_unpoisoned(&delivered_cb) = Some((ThreadId::current(), picked));
        });
        assert!(
            !handle.is_finished(),
            "dialog call must return without blocking"
        );

        gate.wait();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(handle.is_finished());

        event_loop.send_posted_events();
        let (cb_thread, cb_path) = lock_unpoisoned(&delivered)
            .take()
            .expect("callback must run on caller thread");
        assert_eq!(cb_thread, caller);
        assert_eq!(cb_path, Some(PathBuf::from("base").join("picked.txt")));
        assert_eq!(
            handle.try_recv(),
            Some(Some(PathBuf::from("base").join("picked.txt")))
        );
    }

    #[test]
    fn main_thread_dialog_defers_execution_and_runs_on_caller_thread() {
        let event_loop = EventLoop::new();
        let caller = ThreadId::current();
        let ran_on = Arc::new(Mutex::new(None));
        let dialogs = AsyncDialogs::new(Arc::new(MainThreadDialogs {
            ran_on: Arc::clone(&ran_on),
        }));

        let cb_result: Arc<Mutex<Option<(ThreadId, MessageBoxResult)>>> =
            Arc::new(Mutex::new(None));
        let cb_clone = Arc::clone(&cb_result);
        let handle = dialogs.message_box(
            "Title",
            "Body",
            DialogIcon::Question,
            MessageBoxButtons::YesNo,
            move |res| {
                *lock_unpoisoned(&cb_clone) = Some((ThreadId::current(), res));
            },
        );
        assert!(lock_unpoisoned(&ran_on).is_none());
        assert!(!handle.is_finished());

        event_loop.send_posted_events();
        assert_eq!(*lock_unpoisoned(&ran_on), Some(caller));
        assert!(handle.is_finished());
        assert_eq!(handle.try_recv(), Some(MessageBoxResult::Yes));
        assert_eq!(
            *lock_unpoisoned(&cb_result),
            Some((caller, MessageBoxResult::Yes))
        );
    }
}
