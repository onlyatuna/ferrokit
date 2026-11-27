// SPDX-License-Identifier: LGPL-3.0-only
use super::*;
use crate::object::{
    ObjectData, QObject, ThreadContext, register_owned_qobject, register_qobject_ptr,
    unregister_qobject,
};
use std::ptr::NonNull;

#[test]
fn test_livelock_prevention() {
    struct LivelockWidget {
        data: ObjectData,
        queue: Arc<Mutex<EventQueue>>,
        call_count: usize,
    }

    impl LivelockWidget {
        fn new(id: ObjectId, queue: Arc<Mutex<EventQueue>>) -> Self {
            Self {
                data: ObjectData::new(id),
                queue,
                call_count: 0,
            }
        }
    }

    impl QObject for LivelockWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.call_count += 1;
            let mut q = self.queue.lock().unwrap();
            q.push_back(PostedEvent::new(
                self.data.id(),
                Event::new(EventKind::UpdateRequest),
                0,
            ));
            true
        }
    }

    let event_loop = EventLoop::new();
    let q_clone = Arc::clone(event_loop.queue());

    let mut widget = LivelockWidget::new(ObjectId::next(), q_clone);
    // SAFETY: per `register_qobject_ptr`'s contract: `widget` is a local that stays unmoved
    // until the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the
    // variable is only read afterwards, and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

    {
        let mut q = event_loop.queue().lock().unwrap();
        q.push_back(PostedEvent::new(
            widget.object_data().id(),
            Event::new(EventKind::UpdateRequest),
            0,
        ));
    }

    event_loop.send_posted_events();

    assert_eq!(widget.call_count, 1);
    assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

    event_loop.send_posted_events();
    assert_eq!(widget.call_count, 2);
    assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

    // SAFETY: both `send_posted_events` calls have returned, so no callback of `widget` runs;
    // `widget` is still alive and this is the registration thread.
    unsafe { unregister_qobject(widget.object_data().id()) };
}

#[test]
fn test_lock_release_reentrancy() {
    struct ReentrantWidget {
        data: ObjectData,
        queue: Arc<Mutex<EventQueue>>,
        received: bool,
    }

    impl ReentrantWidget {
        fn new(id: ObjectId, queue: Arc<Mutex<EventQueue>>) -> Self {
            Self {
                data: ObjectData::new(id),
                queue,
                received: false,
            }
        }
    }

    impl QObject for ReentrantWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.received = true;
            let mut q = self.queue.lock().unwrap();
            q.post_event(self.data.id(), Event::new(EventKind::Quit { exit_code: 0 }));
            true
        }
    }

    let event_loop = EventLoop::new();
    let q_clone = Arc::clone(event_loop.queue());

    let mut widget = ReentrantWidget::new(ObjectId::next(), q_clone);
    // SAFETY: per `register_qobject_ptr`'s contract: `widget` is a local that stays unmoved
    // until the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the
    // variable is only read afterwards, and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

    event_loop.post_event(
        widget.object_data().id(),
        Event::new(EventKind::UpdateRequest),
    );

    event_loop.send_posted_events();

    assert!(widget.received);
    assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

    // SAFETY: `send_posted_events` has returned, so no callback of `widget` runs; `widget` is
    // still alive and this is the registration thread.
    unsafe { unregister_qobject(widget.object_data().id()) };
}

#[test]
fn test_deferred_delete_loop_level() {
    struct DeleteWidget {
        data: ObjectData,
        deleted: bool,
    }

    impl DeleteWidget {
        fn new(id: ObjectId) -> Self {
            Self {
                data: ObjectData::new(id),
                deleted: false,
            }
        }
    }

    impl QObject for DeleteWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::DeferredDelete { .. }) {
                self.deleted = true;
                true
            } else {
                false
            }
        }
    }

    let event_loop = EventLoop::new();
    let mut widget = DeleteWidget::new(ObjectId::next());
    // SAFETY: per `register_qobject_ptr`'s contract: `widget` is a local that stays unmoved
    // until the end of the test, the pointer comes from `&raw mut`, the variable is only
    // read or used through `&self` afterwards, and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

    // delete_later stamps (loop 1, scope 0), promoted to scope 1.
    ThreadContext::set_current_loop_level(1);
    widget.delete_later();

    // Gate: stamp sum 2 is not greater than the current sum 2.
    ThreadContext::set_current_loop_level(2);
    event_loop.process_events(false);
    assert!(!widget.deleted);
    assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

    ThreadContext::set_current_loop_level(1);
    event_loop.process_events(false);
    assert!(widget.deleted);
    assert_eq!(event_loop.queue().lock().unwrap().len(), 0);
    ThreadContext::set_current_loop_level(0);

    // SAFETY: `process_events` has returned, so no callback of `widget` runs; the delivered
    // `DeferredDelete` already unregistered it (`destroy_deferred`), so this finds no record
    // and is a no-op; `widget` is still alive on the registration thread.
    unsafe { unregister_qobject(widget.object_data().id()) };
}

#[test]
fn test_process_events_pumping() {
    let event_loop = EventLoop::new();

    let handled = event_loop.process_events(false);
    assert!(!handled);

    let obj_id = ObjectId::next();
    event_loop.post_event(obj_id, Event::new(EventKind::LayoutRequest));
    let handled = event_loop.process_events(false);
    assert!(handled);
}

#[test]
fn test_exec_and_exit() {
    let event_loop = EventLoop::new();
    let handle = event_loop.handle();
    let receiver_id = ObjectId::next();

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        handle.post_quit(receiver_id, 42);
    });

    assert_eq!(event_loop.loop_level(), 0);

    let ret = event_loop.exec();

    assert_eq!(ret, 42);
    assert_eq!(event_loop.loop_level(), 0);
}

#[test]
fn test_exec_exit_method() {
    struct ExitWidget {
        data: ObjectData,
        call_count: usize,
        received_quit: bool,
    }

    impl QObject for ExitWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::Quit { .. }) {
                self.received_quit = true;
            }
            self.call_count += 1;
            true
        }
    }

    let event_loop = EventLoop::new();
    let mut widget = ExitWidget {
        data: ObjectData::new(ObjectId::next()),
        call_count: 0,
        received_quit: false,
    };
    // SAFETY: per `register_qobject_ptr`'s contract: `widget` is a local that stays unmoved
    // until the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the
    // variable is only read afterwards, and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

    event_loop.post_event(
        widget.object_data().id(),
        Event::new(EventKind::UpdateRequest),
    );
    event_loop.post_event(
        widget.object_data().id(),
        Event::new(EventKind::Quit { exit_code: 99 }),
    );

    let ret = event_loop.exec();
    assert_eq!(ret, 99);
    assert_eq!(widget.call_count, 2);
    assert!(widget.received_quit);
    assert_eq!(event_loop.loop_level(), 0);

    // SAFETY: `exec` has returned, so no callback of `widget` runs; `widget` is still alive
    // and this is the registration thread.
    unsafe { unregister_qobject(widget.object_data().id()) };
}

#[test]
fn exit_before_exec_is_noop() {
    let el = EventLoop::new();
    el.handle().exit(7);
    let h = el.handle();
    crate::timer::Timer::single_shot(0, move || h.exit(3));
    assert_eq!(el.exec(), 3);
}

#[test]
fn second_exec_is_fresh() {
    let el = EventLoop::new();
    let h1 = el.handle();
    crate::timer::Timer::single_shot(0, move || h1.exit(7));
    assert_eq!(el.exec(), 7);

    let h2 = el.handle();
    crate::timer::Timer::single_shot(0, move || h2.exit(0));
    assert_eq!(el.exec(), 0);
}

#[test]
fn test_basic_delivery() {
    struct MockObject {
        data: ObjectData,
        counter: usize,
    }

    impl MockObject {
        fn new(id: ObjectId) -> Self {
            Self {
                data: ObjectData::new(id),
                counter: 0,
            }
        }
    }

    impl QObject for MockObject {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::User(..)) {
                self.counter += 1;
                true
            } else {
                false
            }
        }
    }

    let event_loop = EventLoop::new();
    let mut mock = MockObject::new(ObjectId::next());
    // SAFETY: per `register_qobject_ptr`'s contract: `mock` is a local that stays unmoved until
    // the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the variable is
    // only read afterwards, and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut mock)) };

    event_loop.post_event(
        mock.object_data().id(),
        Event::new(EventKind::User(Box::new(42u32))),
    );

    let processed = event_loop.process_events(false);

    assert!(processed);
    assert_eq!(mock.counter, 1);
    assert_eq!(event_loop.queue().lock().unwrap().len(), 0);

    // SAFETY: `process_events` has returned, so no callback of `mock` runs; `mock` is still
    // alive and this is the registration thread.
    unsafe { unregister_qobject(mock.object_data().id()) };
}

#[test]
fn test_live_lock_prevention() {
    struct RelayObject {
        data: ObjectData,
        handle: EventLoopHandle,
        call_count: usize,
    }

    impl RelayObject {
        fn new(id: ObjectId, handle: EventLoopHandle) -> Self {
            Self {
                data: ObjectData::new(id),
                handle,
                call_count: 0,
            }
        }
    }

    impl QObject for RelayObject {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.call_count += 1;
            self.handle
                .post_event(self.data.id(), Event::new(EventKind::UpdateRequest));
            true
        }
    }

    let event_loop = EventLoop::new();
    let handle = event_loop.handle();

    let mut relay = RelayObject::new(ObjectId::next(), handle);
    // SAFETY: per `register_qobject_ptr`'s contract: `relay` is a local that stays unmoved
    // until the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the
    // variable is only read afterwards, and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut relay)) };

    event_loop.post_event(
        relay.object_data().id(),
        Event::new(EventKind::UpdateRequest),
    );

    let processed = event_loop.process_events(false);

    assert!(processed);
    assert_eq!(relay.call_count, 1);
    assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

    // SAFETY: `process_events` has returned, so no callback of `relay` runs; `relay` is still
    // alive and this is the registration thread.
    unsafe { unregister_qobject(relay.object_data().id()) };
}

#[test]
fn test_event_compression() {
    struct PaintWidget {
        data: ObjectData,
        paint_count: usize,
    }

    impl PaintWidget {
        fn new(id: ObjectId) -> Self {
            Self {
                data: ObjectData::new(id),
                paint_count: 0,
            }
        }
    }

    impl QObject for PaintWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::UpdateRequest) {
                self.paint_count += 1;
                true
            } else {
                false
            }
        }
    }

    let event_loop = EventLoop::new();
    let mut widget = PaintWidget::new(ObjectId::next());
    // SAFETY: per `register_qobject_ptr`'s contract: `widget` is a local that stays unmoved
    // until the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the
    // variable is only read afterwards, and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

    let id = widget.object_data().id();

    for _ in 0..10 {
        event_loop.post_event(id, Event::new(EventKind::UpdateRequest));
    }

    assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

    let processed = event_loop.process_events(false);
    assert!(processed);
    assert_eq!(widget.paint_count, 1);
    assert_eq!(event_loop.queue().lock().unwrap().len(), 0);

    // SAFETY: `process_events` has returned, so no callback of `widget` runs; `widget` is
    // still alive and this is the registration thread.
    unsafe { unregister_qobject(id) };
}

#[test]
fn test_cross_thread_wakeup() {
    struct WakeupWidget {
        data: ObjectData,
        received_event: bool,
    }

    impl WakeupWidget {
        fn new(id: ObjectId) -> Self {
            Self {
                data: ObjectData::new(id),
                received_event: false,
            }
        }
    }

    impl QObject for WakeupWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::User(..)) {
                self.received_event = true;
                true
            } else {
                false
            }
        }
    }

    let event_loop = EventLoop::new();
    let handle = event_loop.handle();
    let mut widget = WakeupWidget::new(ObjectId::next());
    // SAFETY: per `register_qobject_ptr`'s contract: `widget` is a local that stays unmoved
    // until the `unregister_qobject` at the end, the pointer comes from `&raw mut`, and no
    // callback runs yet. The spawned thread only posts through `handle`; callbacks are
    // dispatched by `exec` on this thread, the registration thread.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut widget)) };

    let id = widget.object_data().id();

    let start = std::time::Instant::now();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        handle.post_event(id, Event::new(EventKind::User(Box::new("wakeup"))));
        handle.post_quit(id, 42);
    });

    let ret = event_loop.exec();
    let elapsed = start.elapsed();

    assert_eq!(ret, 42);
    assert!(widget.received_event);
    assert!(elapsed >= Duration::from_millis(15));
    assert_eq!(event_loop.loop_level(), 0);

    // SAFETY: `exec` has returned, so no callback of `widget` runs; `widget` is still alive
    // and this is the registration thread.
    unsafe { unregister_qobject(id) };
}

#[test]
fn test_notify_helper_pipeline_and_safe_removal() {
    use crate::object::{ObjectData, register_qobject_ptr, unregister_qobject};

    struct TraceFilter {
        data: ObjectData,
        trace: Arc<Mutex<Vec<&'static str>>>,
        label: &'static str,
        intercept: bool,
        remove_target: Option<ObjectId>,
    }

    impl TraceFilter {
        fn new(
            label: &'static str,
            intercept: bool,
            remove_target: Option<ObjectId>,
            trace: Arc<Mutex<Vec<&'static str>>>,
        ) -> Self {
            Self {
                data: ObjectData::new(ObjectId::next()),
                trace,
                label,
                intercept,
                remove_target,
            }
        }
    }

    impl QObject for TraceFilter {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
            self.trace.lock().unwrap().push(self.label);
            if let Some(target) = self.remove_target {
                crate::object::with_object_mut(target, |obj| {
                    obj.object_data_mut().remove_event_filter(self.data.id());
                });
                self.remove_target = None;
            }
            self.intercept
        }
    }

    struct TargetWidget {
        data: ObjectData,
        trace: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TargetWidget {
        fn new(trace: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                data: ObjectData::new(ObjectId::next()),
                trace,
            }
        }
    }

    impl QObject for TargetWidget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            self.trace.lock().unwrap().push("target_event");
            true
        }
    }

    clear_application_event_filters();
    let trace = Arc::new(Mutex::new(Vec::new()));

    let mut app_filter = TraceFilter::new("app_filter", false, None, Arc::clone(&trace));
    let app_filter_id = app_filter.data.id();
    let mut target = TargetWidget::new(Arc::clone(&trace));
    let target_id = target.data.id();
    let mut obj_filter = TraceFilter::new("obj_filter", false, Some(target_id), Arc::clone(&trace));
    let obj_filter_id = obj_filter.data.id();
    // Set up before registering: the registry holds a pointer made here, and a use of the
    // variable after that would invalidate it.
    target.object_data_mut().install_event_filter(obj_filter_id);
    let app_filter_ptr = &raw mut app_filter;
    // SAFETY: per `register_qobject_ptr`'s contract: the three locals stay on this thread,
    // unmoved, until unregistered below; the pointers come from `&raw mut`, the variables are
    // not written to after this (only `app_filter_ptr` is), and no callback runs yet.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(app_filter_ptr));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut obj_filter));
    }
    install_application_event_filter(app_filter_id);

    let mut event1 = Event::new(EventKind::UpdateRequest);
    let handled = send_event(target_id, &mut event1);
    assert!(handled);
    assert_eq!(
        *trace.lock().unwrap(),
        vec!["app_filter", "obj_filter", "target_event"]
    );

    trace.lock().unwrap().clear();
    let mut event2 = Event::new(EventKind::UpdateRequest);
    let handled2 = send_event(target_id, &mut event2);
    assert!(handled2);
    assert_eq!(*trace.lock().unwrap(), vec!["app_filter", "target_event"]);

    // SAFETY: `app_filter_ptr` is the `&raw mut` pointer that was registered for `app_filter`
    // (a live local), so writing through it keeps the registry's pointer valid; no callback of
    // the filter runs because the previous `send_event` has returned.
    unsafe { (*app_filter_ptr).intercept = true };
    trace.lock().unwrap().clear();
    let mut event3 = Event::new(EventKind::UpdateRequest);
    let handled3 = send_event(target_id, &mut event3);
    assert!(handled3);
    assert_eq!(*trace.lock().unwrap(), vec!["app_filter"]);

    clear_application_event_filters();
    // SAFETY: the last `send_event` has returned, so no callback runs; the three locals are
    // still alive and this is the registration thread.
    unsafe {
        unregister_qobject(app_filter_id);
        unregister_qobject(obj_filter_id);
        unregister_qobject(target_id);
    }
}

type Trace = Arc<Mutex<Vec<&'static str>>>;
/// Runs inside `ScriptedFilter::event_filter`; receives (watched object, this filter).
type FilterAction = Box<dyn FnMut(ObjectId, ObjectId)>;

struct ScriptedFilter {
    data: ObjectData,
    label: &'static str,
    trace: Trace,
    on_event: Option<FilterAction>,
}

impl ScriptedFilter {
    fn new(label: &'static str, trace: Trace) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            label,
            trace,
            on_event: None,
        }
    }
}

impl QObject for ScriptedFilter {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
    fn event_filter(&mut self, watched: ObjectId, _event: &mut Event) -> bool {
        self.trace.lock().unwrap().push(self.label);
        let me = self.data.id();
        if let Some(action) = &mut self.on_event {
            action(watched, me);
        }
        false
    }
}

struct TraceTarget {
    data: ObjectData,
    trace: Trace,
}

impl TraceTarget {
    fn new(trace: Trace) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            trace,
        }
    }
}

impl QObject for TraceTarget {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        self.trace.lock().unwrap().push("target");
        true
    }
}

#[test]
fn filter_removed_by_earlier_filter_is_not_called() {
    // Qt: object filters are walked by live index; a removed slot is null and skipped
    let trace: Trace = Arc::new(Mutex::new(Vec::new()));
    let mut target = TraceTarget::new(Arc::clone(&trace));
    let mut earlier = ScriptedFilter::new("earlier", Arc::clone(&trace));
    let mut later = ScriptedFilter::new("later", Arc::clone(&trace));
    let target_id = target.data.id();
    let earlier_id = earlier.data.id();
    let later_id = later.data.id();
    // Set up before registering: a write through the variable after that would invalidate
    // the pointer the registry holds.
    target.object_data_mut().install_event_filter(later_id);
    target.object_data_mut().install_event_filter(earlier_id);
    earlier.on_event = Some(Box::new(move |watched: ObjectId, _: ObjectId| {
        crate::object::with_object_mut(watched, |obj| {
            obj.object_data_mut().remove_event_filter(later_id);
        });
    }));
    // SAFETY: per `register_qobject_ptr`'s contract: the three locals stay unmoved until
    // unregistered below, pointers come from `&raw mut`, the variables are not used again
    // (the `install_event_filter` calls ran before this), and no callback runs yet.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut earlier));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut later));
    }

    let mut event = Event::new(EventKind::UpdateRequest);
    assert!(send_event(target_id, &mut event));
    assert_eq!(*trace.lock().unwrap(), vec!["earlier", "target"]);

    // SAFETY: `send_event` has returned, so no callback runs; the three locals are still alive
    // and this is the registration thread.
    unsafe {
        unregister_qobject(target_id);
        unregister_qobject(earlier_id);
        unregister_qobject(later_id);
    }
}

#[test]
fn app_filter_removed_by_earlier_filter_is_not_called() {
    // Qt: application filters use the same live index walk.
    clear_application_event_filters();
    let trace: Trace = Arc::new(Mutex::new(Vec::new()));
    let mut target = TraceTarget::new(Arc::clone(&trace));
    let mut earlier = ScriptedFilter::new("earlier", Arc::clone(&trace));
    let mut later = ScriptedFilter::new("later", Arc::clone(&trace));
    let target_id = target.data.id();
    let earlier_id = earlier.data.id();
    let later_id = later.data.id();
    // Set up before registering: a write through the variable after that would invalidate
    // the pointer the registry holds.
    earlier.on_event = Some(Box::new(move |_: ObjectId, _: ObjectId| {
        remove_application_event_filter(later_id);
    }));
    // SAFETY: per `register_qobject_ptr`'s contract: the three locals stay unmoved until
    // unregistered below, pointers come from `&raw mut`, the variables are not used again, and
    // no callback runs yet (nothing is dispatched before `send_event`).
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut earlier));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut later));
    }
    install_application_event_filter(later_id);
    install_application_event_filter(earlier_id);

    let mut event = Event::new(EventKind::UpdateRequest);
    assert!(send_event(target_id, &mut event));
    assert_eq!(*trace.lock().unwrap(), vec!["earlier", "target"]);

    clear_application_event_filters();
    // SAFETY: `send_event` has returned, so no callback runs; the three locals are still alive
    // and this is the registration thread.
    unsafe {
        unregister_qobject(target_id);
        unregister_qobject(earlier_id);
        unregister_qobject(later_id);
    }
}

#[test]
fn filter_installed_during_dispatch_follows_qt_index_walk() {
    // Qt: installEventFilter prepends. The walk is already past index 0
    // when the callback runs, so the new filter is NOT called for this event. The prepend shifts
    // the running filter to index 1, and the `++i` step calls it once more for the same event
    // . The next event sees the new filter first.
    let trace: Trace = Arc::new(Mutex::new(Vec::new()));
    let mut target = TraceTarget::new(Arc::clone(&trace));
    let mut first = ScriptedFilter::new("first", Arc::clone(&trace));
    let mut added = ScriptedFilter::new("added", Arc::clone(&trace));
    let target_id = target.data.id();
    let first_id = first.data.id();
    let added_id = added.data.id();
    // Set up before registering: a write through the variable after that would invalidate
    // the pointer the registry holds.
    target.object_data_mut().install_event_filter(first_id);
    let mut installed = false;
    first.on_event = Some(Box::new(move |watched: ObjectId, _: ObjectId| {
        if !installed {
            installed = true;
            crate::object::with_object_mut(watched, |obj| {
                obj.object_data_mut().install_event_filter(added_id);
            });
        }
    }));
    // SAFETY: per `register_qobject_ptr`'s contract: the three locals stay unmoved until
    // unregistered below, pointers come from `&raw mut`, the variables are not used again
    // (`install_event_filter` ran before this), and no callback runs yet.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut first));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut added));
    }

    let mut event = Event::new(EventKind::UpdateRequest);
    assert!(send_event(target_id, &mut event));
    assert_eq!(*trace.lock().unwrap(), vec!["first", "first", "target"]);

    trace.lock().unwrap().clear();
    let mut next_event = Event::new(EventKind::UpdateRequest);
    assert!(send_event(target_id, &mut next_event));
    assert_eq!(*trace.lock().unwrap(), vec!["added", "first", "target"]);

    // SAFETY: both `send_event` calls have returned, so no callback runs; the three locals are
    // still alive and this is the registration thread.
    unsafe {
        unregister_qobject(target_id);
        unregister_qobject(first_id);
        unregister_qobject(added_id);
    }
}

#[test]
fn app_filter_installed_during_dispatch_follows_qt_index_walk() {
    // Qt: same prepend + index walk as the object chain, on the application list
    clear_application_event_filters();
    let trace: Trace = Arc::new(Mutex::new(Vec::new()));
    let mut target = TraceTarget::new(Arc::clone(&trace));
    let mut first = ScriptedFilter::new("first", Arc::clone(&trace));
    let mut added = ScriptedFilter::new("added", Arc::clone(&trace));
    let target_id = target.data.id();
    let first_id = first.data.id();
    let added_id = added.data.id();
    // Set up before registering: a write through the variable after that would invalidate
    // the pointer the registry holds.
    let mut installed = false;
    first.on_event = Some(Box::new(move |_: ObjectId, _: ObjectId| {
        if !installed {
            installed = true;
            install_application_event_filter(added_id);
        }
    }));
    // SAFETY: per `register_qobject_ptr`'s contract: the three locals stay unmoved until
    // unregistered below, pointers come from `&raw mut`, the variables are not used again, and
    // no callback runs yet (nothing is dispatched before `send_event`).
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut first));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut added));
    }
    install_application_event_filter(first_id);

    let mut event = Event::new(EventKind::UpdateRequest);
    assert!(send_event(target_id, &mut event));
    assert_eq!(*trace.lock().unwrap(), vec!["first", "first", "target"]);

    trace.lock().unwrap().clear();
    let mut next_event = Event::new(EventKind::UpdateRequest);
    assert!(send_event(target_id, &mut next_event));
    assert_eq!(*trace.lock().unwrap(), vec!["added", "first", "target"]);

    clear_application_event_filters();
    // SAFETY: both `send_event` calls have returned, so no callback runs; the three locals are
    // still alive and this is the registration thread.
    unsafe {
        unregister_qobject(target_id);
        unregister_qobject(first_id);
        unregister_qobject(added_id);
    }
}

#[test]
fn test_post_event_short_circuit_and_compression() {
    struct MockTarget {
        data: ObjectData,
    }
    impl MockTarget {
        fn new(id: ObjectId) -> Self {
            Self {
                data: ObjectData::new(id),
            }
        }
    }
    impl QObject for MockTarget {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            true
        }
    }

    let event_loop = EventLoop::new();
    let mut target = MockTarget::new(ObjectId::next());
    // SAFETY: per `register_qobject_ptr`'s contract: `target` is a local that stays unmoved
    // until the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the variable
    // is only read afterwards (`object_data().id()`) and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut target)) };

    let id = target.object_data().id();

    event_loop.post_event(id, Event::new(EventKind::UpdateRequest));
    assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

    event_loop.post_event(id, Event::new(EventKind::UpdateRequest));
    assert_eq!(event_loop.queue().lock().unwrap().len(), 1);

    event_loop.post_event(id, Event::new(EventKind::Timer { timer_id: 10 }));
    assert_eq!(event_loop.queue().lock().unwrap().len(), 2);

    event_loop.post_event(id, Event::new(EventKind::Timer { timer_id: 10 }));

    assert_eq!(event_loop.queue().lock().unwrap().len(), 2);

    // SAFETY: nothing was dispatched, so no callback of `target` runs; `target` is still alive
    // and this is the registration thread.
    unsafe { unregister_qobject(id) };
}

#[test]
fn test_insertion_offset_priority_ordering() {
    let execution_order = Arc::new(Mutex::new(Vec::new()));

    struct PriorityRelay {
        data: ObjectData,
        order: Arc<Mutex<Vec<&'static str>>>,
        loop_handle: EventLoopHandle,
    }
    impl QObject for PriorityRelay {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if let EventKind::User(any_data) = &event.kind {
                let label = *any_data.downcast_ref::<&'static str>().unwrap();
                self.order.lock().unwrap().push(label);

                if label == "initial_event_1" {
                    self.loop_handle.post_event_with_priority(
                        self.data.id(),
                        Event::new(EventKind::User(Box::new("high_priority_event"))),
                        100,
                    );
                }
                true
            } else {
                false
            }
        }
    }

    let event_loop = EventLoop::new();
    let mut relay = PriorityRelay {
        data: ObjectData::new(ObjectId::next()),
        order: Arc::clone(&execution_order),
        loop_handle: event_loop.handle(),
    };
    // SAFETY: per `register_qobject_ptr`'s contract: `relay` is a local that stays unmoved until
    // the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the variable is
    // only read afterwards (`data.id()`) and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut relay)) };
    let id = relay.data.id();

    event_loop.post_event_with_priority(
        id,
        Event::new(EventKind::User(Box::new("initial_event_1"))),
        0,
    );
    event_loop.post_event_with_priority(
        id,
        Event::new(EventKind::User(Box::new("initial_event_2"))),
        0,
    );

    let delivered = event_loop.send_posted_events();
    assert_eq!(delivered, 2);
    assert_eq!(
        *execution_order.lock().unwrap(),
        vec!["initial_event_1", "initial_event_2"],
    );

    let delivered2 = event_loop.send_posted_events();
    assert_eq!(delivered2, 1);
    assert_eq!(
        *execution_order.lock().unwrap(),
        vec!["initial_event_1", "initial_event_2", "high_priority_event"]
    );

    // SAFETY: both `send_posted_events` calls have returned, so no callback of `relay` runs;
    // `relay` is still alive and this is the registration thread.
    unsafe { unregister_qobject(id) };
}

/// the core application snapshots `insertionOffset` at the start
/// of a pass, so events posted from a handler wait for the next
/// pass, and events posted between passes are ordered by priority among the remaining entries.
/// `CleanUp` shifts `insertionOffset` down by the number of erased entries.
#[test]
fn test_posted_during_pump_waits_for_next_pass_and_orders_by_priority() {
    let execution_order = Arc::new(Mutex::new(Vec::new()));

    struct NestedRelay {
        data: ObjectData,
        order: Arc<Mutex<Vec<&'static str>>>,
        loop_handle: EventLoopHandle,
    }
    impl QObject for NestedRelay {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if let EventKind::User(any_data) = &event.kind {
                let label = *any_data.downcast_ref::<&'static str>().unwrap();
                self.order.lock().unwrap().push(label);
                if label == "B" {
                    self.loop_handle.post_event_with_priority(
                        self.data.id(),
                        Event::new(EventKind::User(Box::new("X"))),
                        0,
                    );
                    self.loop_handle.post_event_with_priority(
                        self.data.id(),
                        Event::new(EventKind::User(Box::new("Y"))),
                        100,
                    );
                }
                true
            } else {
                false
            }
        }
    }

    let event_loop = EventLoop::new();
    let mut relay = NestedRelay {
        data: ObjectData::new(ObjectId::next()),
        order: Arc::clone(&execution_order),
        loop_handle: event_loop.handle(),
    };
    // SAFETY: per `register_qobject_ptr`'s contract: `relay` is a local that stays unmoved until
    // the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the variable is
    // only read afterwards (`data.id()`) and no callback runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut relay)) };
    let id = relay.data.id();

    event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("A"))), 0);
    event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("B"))), 0);

    // Pass 1 delivers A and B; X and Y were posted during it and must not run yet.
    assert_eq!(event_loop.send_posted_events(), 2);
    assert_eq!(*execution_order.lock().unwrap(), vec!["A", "B"]);

    // Pass 2 delivers the nested posts, higher priority first.
    assert_eq!(event_loop.send_posted_events(), 2);
    assert_eq!(*execution_order.lock().unwrap(), vec!["A", "B", "Y", "X"]);

    // Posts made between passes: priority decides among them, insertion order breaks ties.
    event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("L"))), 0);
    event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("M"))), 0);
    event_loop.post_event_with_priority(id, Event::new(EventKind::User(Box::new("H"))), 100);
    assert_eq!(event_loop.send_posted_events(), 3);
    assert_eq!(
        *execution_order.lock().unwrap(),
        vec!["A", "B", "Y", "X", "H", "L", "M"]
    );

    // SAFETY: the last `send_posted_events` has returned, so no callback of `relay` runs;
    // `relay` is still alive and this is the registration thread.
    unsafe { unregister_qobject(id) };
}
#[test]
fn test_generic_event_dispatcher_and_event_loop_abstraction() {
    use super::super::dispatcher::{EventDispatcher, GenericEventDispatcher};

    let generic_dispatcher = GenericEventDispatcher::new();
    let handle = generic_dispatcher.clone_handle();

    let handle_clone = Arc::clone(&handle);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        handle_clone.wake_up();
    });

    let res = generic_dispatcher.process_events(true, Some(Duration::from_millis(500)));
    assert_eq!(res, DispatchResult::Awoken);

    let el = EventLoop::new();
    let el_handle = el.handle();
    el_handle.wake_up();
}

fn meta_call<F: FnOnce() + Send + 'static>(f: F) -> Event {
    Event::new(EventKind::MetaCall(Box::new(move |_| f())))
}

fn post_current(event: Event) {
    post_event_to_thread(ThreadId::current(), ObjectId(0), event);
}

#[test]
fn test_normal_pump_delivers_posted_events() {
    let el = EventLoop::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    for name in ["a", "b"] {
        let log = Arc::clone(&log);
        post_current(meta_call(move || log.lock().unwrap().push(name)));
    }
    assert_eq!(el.send_posted_events(), 2);
    assert_eq!(*log.lock().unwrap(), vec!["a", "b"]);
    assert!(el.queue().lock().unwrap().events.is_empty());
}

#[test]
fn test_modal_pump_keeps_turn_boundary() {
    let el = EventLoop::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let log_a = Arc::clone(&log);
        let log_c = Arc::clone(&log);
        post_current(meta_call(move || {
            log_a.lock().unwrap().push("a");
            post_current(meta_call(move || log_c.lock().unwrap().push("c")));
        }));
    }
    {
        let log_b = Arc::clone(&log);
        post_current(meta_call(move || log_b.lock().unwrap().push("b")));
    }

    assert_eq!(pump_posted_events_modal(), Some(2));
    assert_eq!(*log.lock().unwrap(), vec!["a", "b"]);
    assert_eq!(
        el.queue().lock().unwrap().events.len(),
        1,
        "C waits for next turn"
    );

    assert_eq!(pump_posted_events_modal(), Some(1));
    assert_eq!(*log.lock().unwrap(), vec!["a", "b", "c"]);
}

#[test]
fn test_modal_pump_without_event_loop_is_noop() {
    assert_eq!(pump_posted_events_modal(), None);
}

/// A native modal loop opened from a posted callback (`Menu::exec_popup` from
/// `Timer::single_shot(0)`) is a nested event loop: Qt delivers posted events from it
/// before the callback returns (`QMenuPrivate::exec` runs a an event loop,;
/// the core application re-enters).
#[test]
fn test_modal_pump_inside_a_posted_callback_delivers_events_before_it_returns() {
    let el = EventLoop::new();
    let nested = Arc::new(Mutex::new(None));
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let nested = Arc::clone(&nested);
        let log = Arc::clone(&log);
        post_current(meta_call(move || {
            let log_late = Arc::clone(&log);
            post_current(meta_call(move || log_late.lock().unwrap().push("late")));
            *nested.lock().unwrap() = Some(pump_posted_events_modal());
            log.lock().unwrap().push("outer returns");
        }));
    }

    assert_eq!(el.send_posted_events(), 1);
    assert_eq!(*nested.lock().unwrap(), Some(Some(1)));
    assert_eq!(*log.lock().unwrap(), vec!["late", "outer returns"]);
    assert!(el.queue().lock().unwrap().events.is_empty());
}

#[test]
fn test_modal_pump_inside_a_modal_pump_delivers_events_before_it_returns() {
    let el = EventLoop::new();
    let nested = Arc::new(Mutex::new(None));
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let nested = Arc::clone(&nested);
        let log = Arc::clone(&log);
        post_current(meta_call(move || {
            let log_late = Arc::clone(&log);
            post_current(meta_call(move || log_late.lock().unwrap().push("late")));
            *nested.lock().unwrap() = Some(pump_posted_events_modal());
            log.lock().unwrap().push("outer returns");
        }));
    }

    assert_eq!(pump_posted_events_modal(), Some(1));
    assert_eq!(*nested.lock().unwrap(), Some(Some(1)));
    assert_eq!(*log.lock().unwrap(), vec!["late", "outer returns"]);
    assert!(el.queue().lock().unwrap().events.is_empty());
    assert!(!POSTED_PUMP_ACTIVE.with(|f| f.get()));
}

/// The nested pump takes every event queued when it starts (Qt resets `insertionOffset` to
/// the list size) and delivers each once; the outer pump resumes
/// from the shared `start_offset`, so it neither repeats them nor runs events
/// posted during the nested pump, which wait for the next turn.
#[test]
fn test_a_nested_pump_delivers_each_event_once_and_leaves_its_own_posts_for_the_next_turn() {
    let el = EventLoop::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let log = Arc::clone(&log);
        post_current(meta_call(move || {
            let _ = pump_posted_events_modal();
            log.lock().unwrap().push("outer returns");
        }));
    }
    {
        let log_b = Arc::clone(&log);
        let log_d = Arc::clone(&log);
        post_current(meta_call(move || {
            log_b.lock().unwrap().push("b");
            post_current(meta_call(move || log_d.lock().unwrap().push("d")));
        }));
    }
    {
        let log = Arc::clone(&log);
        post_current(meta_call(move || log.lock().unwrap().push("c")));
    }

    el.send_posted_events();
    assert_eq!(*log.lock().unwrap(), vec!["b", "c", "outer returns"]);
    assert_eq!(
        el.queue().lock().unwrap().events.len(),
        1,
        "d waits for the next turn"
    );

    el.send_posted_events();
    assert_eq!(*log.lock().unwrap(), vec!["b", "c", "outer returns", "d"]);
    assert!(el.queue().lock().unwrap().events.is_empty());
}

/// An event filter that consumes the `DeferredDelete` keeps the object alive: Qt deletes in
/// `QObject::event`, which a consuming filter never lets run.
#[test]
fn test_a_filter_consuming_deferred_delete_keeps_the_object_alive() {
    struct Consumer(ObjectData, Arc<AtomicBool>);
    impl QObject for Consumer {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.0)
        }
        fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
            self.1.store(true, Ordering::SeqCst);
            true
        }
    }
    struct Target(ObjectData);
    impl QObject for Target {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.0)
        }
    }

    let el = EventLoop::new();
    let consumed = Arc::new(AtomicBool::new(false));
    let mut filter = Consumer(ObjectData::new(ObjectId::next()), Arc::clone(&consumed));
    let filter_id = filter.0.id();
    let mut target = Target(ObjectData::new(ObjectId::next()));
    let target_id = target.0.id();
    target.0.install_event_filter(filter_id);
    // SAFETY: per `register_qobject_ptr`'s contract: both are locals that stay unmoved until
    // unregistered below, pointers come from `&raw mut`, `install_event_filter` ran before this
    // (no write through the variables afterwards) and no callback runs yet.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut filter));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut target));
    }
    ThreadContext::set_current_loop_level(1);
    target.0.delete_later();
    el.process_events(false);
    ThreadContext::set_current_loop_level(0);
    assert!(
        consumed.load(Ordering::SeqCst),
        "the DeferredDelete never reached the filter"
    );
    assert!(
        crate::object::qobject::registration_thread_of(target_id).is_some(),
        "a consumed DeferredDelete must not destroy the object"
    );
    // SAFETY: `process_events` has returned, so no callback runs; this is the registration
    // thread and `filter` and `target` are still alive locals.
    unsafe {
        crate::object::unregister_qobject(target_id);
        crate::object::unregister_qobject(filter_id);
    }
}

/// A nested `DeferredDelete` that a filter consumes for another object does not keep the
/// outer object alive.
#[test]
fn test_a_nested_filtered_deferred_delete_does_not_save_the_outer_object() {
    struct Consumer(ObjectData, Arc<AtomicBool>);
    impl QObject for Consumer {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.0)
        }
        fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
            self.1.store(true, Ordering::SeqCst);
            true
        }
    }
    struct Nester(ObjectData, ObjectId);
    impl QObject for Nester {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.0)
        }
        fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::DeferredDelete { .. }) {
                let mut nested = Event::new(EventKind::DeferredDelete {
                    loop_level: 0,
                    scope_level: 0,
                });
                send_event(self.1, &mut nested);
            }
            false
        }
    }
    struct Plain(ObjectData);
    impl QObject for Plain {
        fn object_data(&self) -> &ObjectData {
            &self.0
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.0)
        }
    }

    let el = EventLoop::new();
    let consumed = Arc::new(AtomicBool::new(false));
    let mut filter = Consumer(ObjectData::new(ObjectId::next()), Arc::clone(&consumed));
    let filter_id = filter.0.id();
    let mut shielded = Plain(ObjectData::new(ObjectId::next()));
    let shielded_id = shielded.0.id();
    shielded.0.install_event_filter(filter_id);
    let mut nester = Nester(ObjectData::new(ObjectId::next()), shielded_id);
    let nester_id = nester.0.id();
    let mut outer = Plain(ObjectData::new(ObjectId::next()));
    let outer_id = outer.0.id();
    outer.0.install_event_filter(nester_id);
    // SAFETY: per `register_qobject_ptr`'s contract: all four are locals of this test that stay
    // unmoved until unregistered below, pointers come from `&raw mut`, they are only read
    // through the variables afterwards (`install_event_filter` ran before this), and no
    // callback runs yet.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut filter));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut shielded));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut nester));
        register_qobject_ptr(NonNull::new_unchecked(&raw mut outer));
    }
    ThreadContext::set_current_loop_level(1);
    outer.0.delete_later();
    el.process_events(false);
    ThreadContext::set_current_loop_level(0);
    assert!(
        consumed.load(Ordering::SeqCst),
        "the nested DeferredDelete never reached the filter"
    );
    assert!(
        crate::object::qobject::registration_thread_of(outer_id).is_none(),
        "the outer object must be destroyed"
    );
    // SAFETY: the `send_posted_events` pump has returned, so no callback runs; this is the
    // registration thread and the locals are still alive. `outer` was already unregistered by
    // its delivered `DeferredDelete`; unregistering an id twice is a no-op on the registry.
    unsafe {
        crate::object::unregister_qobject(shielded_id);
        crate::object::unregister_qobject(nester_id);
        crate::object::unregister_qobject(filter_id);
    }
}

/// An object deleted later from the outer loop survives a nested loop and is deleted once
/// control is back in the loop that requested it.
#[test]
fn test_a_deferred_delete_from_the_outer_loop_waits_out_a_nested_pump() {
    struct Deletable {
        data: ObjectData,
        deleted: Arc<AtomicBool>,
    }
    impl QObject for Deletable {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            if matches!(event.kind, EventKind::DeferredDelete { .. }) {
                self.deleted.store(true, Ordering::SeqCst);
                true
            } else {
                false
            }
        }
    }

    let el = EventLoop::new();
    ThreadContext::set_current_loop_level(1);
    let deleted = Arc::new(AtomicBool::new(false));
    let mut object = Deletable {
        data: ObjectData::new(ObjectId::next()),
        deleted: Arc::clone(&deleted),
    };
    // SAFETY: per `register_qobject_ptr`'s contract: `object` is a local that stays unmoved
    // until the test ends, the pointer comes from `&raw mut`, it is only read through the
    // variable afterwards, and no callback runs yet. The delivered `DeferredDelete` unregisters
    // it (`destroy_deferred`), which is memory-neutral.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut object)) };
    let object_addr = &object as *const Deletable as usize;

    let deleted_during_nested = Arc::new(Mutex::new(None));
    {
        let deleted = Arc::clone(&deleted);
        let deleted_during_nested = Arc::clone(&deleted_during_nested);
        post_current(meta_call(move || {
            // SAFETY: `object` is a local of the test that outlives every pump below and is not
            // moved; the pump unregisters it but never frees it. The closure is a posted
            // meta-call, not a callback of `object`, and only calls `delete_later(&self)`.
            let object = unsafe { &*(object_addr as *const Deletable) };
            // Requested inside a handler: stamped at loop level 1, scope level 1.
            object.delete_later();
            let _ = pump_posted_events_modal();
            *deleted_during_nested.lock().unwrap() = Some(deleted.load(Ordering::SeqCst));
        }));
    }

    el.send_posted_events();
    assert_eq!(*deleted_during_nested.lock().unwrap(), Some(false));
    el.send_posted_events();
    assert!(deleted.load(Ordering::SeqCst));
    assert!(el.queue().lock().unwrap().events.is_empty());
    ThreadContext::set_current_loop_level(0);
}

/// A handler that calls `delete_later` and then runs a nested non-modal pump keeps the target
/// alive for that pump. `notify_helper` holds scope level 1 for the whole handler
/// so the DeferredDelete stamped (1) is not deliverable at
/// (1). The outer loop delivers it once the handler returns.
#[test]
fn test_nested_pump_inside_a_handler_keeps_its_delete_later_target_alive() {
    struct Victim {
        data: ObjectData,
    }
    impl QObject for Victim {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, _event: &mut Event) -> bool {
            false
        }
    }

    struct Driver {
        data: ObjectData,
        victim_data: usize,
        victim_id: ObjectId,
        survived_nested: Arc<Mutex<Option<bool>>>,
        deleted_by_outer: Arc<Mutex<Option<bool>>>,
    }
    impl QObject for Driver {
        fn object_data(&self) -> &ObjectData {
            &self.data
        }
        fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
            crate::object::ObjectDataMut::new(&mut self.data)
        }
        fn event(&mut self, event: &mut Event) -> bool {
            match event.kind {
                EventKind::MouseButtonPress { .. } => {
                    // SAFETY: `victim_data` is the address of the `ObjectData` inside the
                    // victim's heap allocation (`OwnedObject`), taken in the test body. The
                    // `victim` owner is a local that outlives `exec`, so the memory stays valid,
                    // and `destroy_deferred` never frees an object whose owner it cannot
                    // reach, only unregisters it. `delete_later` takes `&ObjectData`, and no
                    // callback of the victim runs during this driver callback.
                    let victim = unsafe { &*(self.victim_data as *const ObjectData) };
                    victim.delete_later();
                    let handle =
                        get_thread_event_sender(ThreadId::current()).expect("loop registered");
                    let _ = pump_posted_events(&handle.queue, false);
                    *self.survived_nested.lock().unwrap() =
                        Some(crate::object::query_object_thread(self.victim_id).is_some());
                    post_event_to_thread(
                        ThreadId::current(),
                        self.data.id(),
                        Event::new_spontaneous(EventKind::MouseButtonRelease {
                            x: 0,
                            y: 0,
                            button: 1,
                        }),
                    );
                    true
                }
                EventKind::MouseButtonRelease { .. } => {
                    *self.deleted_by_outer.lock().unwrap() =
                        Some(crate::object::query_object_thread(self.victim_id).is_none());
                    if let Some(handle) = get_thread_event_sender(ThreadId::current()) {
                        handle.quit();
                    }
                    true
                }
                _ => false,
            }
        }
    }

    let survived_nested = Arc::new(Mutex::new(None));
    let deleted_by_outer = Arc::new(Mutex::new(None));
    let el = EventLoop::new();
    // SAFETY: per `register_owned_qobject`'s contract: no callback of the new object runs
    // (nothing is dispatched before `exec`), and the owner is used and dropped only on this
    // test's thread, after `exec` has returned.
    let victim = unsafe {
        register_owned_qobject(Victim {
            data: ObjectData::new(ObjectId::next()),
        })
    };
    let victim_id = crate::object::OwnedObject::object_id(&victim);
    // SAFETY: same as for `victim`: `driver` is used only on this thread and dropped at the end
    // of the test, and `victim.with` above runs between callbacks (nothing is dispatched yet).
    let driver = unsafe {
        register_owned_qobject(Driver {
            data: ObjectData::new(ObjectId::next()),
            victim_data: victim
                .with(|v| &v.data as *const ObjectData as usize)
                .unwrap(),
            victim_id,
            survived_nested: Arc::clone(&survived_nested),
            deleted_by_outer: Arc::clone(&deleted_by_outer),
        })
    };
    let driver_id = crate::object::OwnedObject::object_id(&driver);
    post_event_to_thread(
        ThreadId::current(),
        driver_id,
        Event::new_spontaneous(EventKind::MouseButtonPress {
            x: 0,
            y: 0,
            button: 1,
        }),
    );

    assert_eq!(el.exec(), 0);
    assert_eq!(
        *survived_nested.lock().unwrap(),
        Some(true),
        "delete_later target was deleted by a nested pump inside its requesting handler"
    );
    assert_eq!(
        *deleted_by_outer.lock().unwrap(),
        Some(true),
        "delete_later target was not delivered by the outer loop after the handler returned"
    );
    // SAFETY: `exec` has returned, so no callback of the driver runs, and this is the
    // registration thread; the `driver` owner is still alive here, so its memory stays valid.
    unsafe { unregister_qobject(driver_id) };
}

#[test]
fn test_modal_pump_runs_sentinel_metacall() {
    let _el = EventLoop::new();
    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = Arc::clone(&ran);
    post_current(meta_call(move || ran_clone.store(true, Ordering::SeqCst)));
    assert_eq!(pump_posted_events_modal(), Some(1));
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn test_posts_made_during_a_nested_pump_wait_for_the_next_turn_in_priority_order() {
    let el = EventLoop::new();
    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let log_outer = Arc::clone(&log);
    post_current(meta_call(move || {
        let _ = pump_posted_events_modal();
        log_outer.lock().unwrap().push("outer returns");
    }));
    let log_x = Arc::clone(&log);
    post_current(meta_call(move || {
        log_x.lock().unwrap().push("x");
        for (priority, name) in [(0, "p0"), (5, "p5")] {
            let log_p = Arc::clone(&log_x);
            post_event_to_thread_with_priority(
                ThreadId::current(),
                ObjectId(0),
                meta_call(move || log_p.lock().unwrap().push(name)),
                priority,
            );
        }
    }));
    let log_y = Arc::clone(&log);
    post_current(meta_call(move || log_y.lock().unwrap().push("y")));

    // `x` and `y` run inside the nested pump, before the outer callback returns.
    assert_eq!(el.send_posted_events(), 1);
    assert_eq!(*log.lock().unwrap(), vec!["x", "y", "outer returns"]);

    // Posts made during the nested turn wait for the next turn, highest priority first.
    assert_eq!(el.send_posted_events(), 2);
    assert_eq!(
        *log.lock().unwrap(),
        vec!["x", "y", "outer returns", "p5", "p0"]
    );
}

#[test]
fn test_a_priority_post_between_turns_overtakes_a_lower_priority_leftover() {
    let el = EventLoop::new();
    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let log_outer = Arc::clone(&log);
    post_current(meta_call(move || {
        // Posted during the turn, so it stays queued for the next turn at priority 0.
        let log_x = Arc::clone(&log_outer);
        post_event_to_thread_with_priority(
            ThreadId::current(),
            ObjectId(0),
            meta_call(move || log_x.lock().unwrap().push("x")),
            0,
        );
    }));
    assert_eq!(el.send_posted_events(), 1);

    let log_y = Arc::clone(&log);
    post_event_to_thread_with_priority(
        ThreadId::current(),
        ObjectId(0),
        meta_call(move || log_y.lock().unwrap().push("y")),
        5,
    );
    assert_eq!(el.send_posted_events(), 2);
    assert_eq!(*log.lock().unwrap(), vec!["y", "x"]);
}

struct UnregisteredProbe {
    data: ObjectData,
}

impl QObject for UnregisteredProbe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
}

fn queued_for_current_thread() -> usize {
    get_thread_event_sender(ThreadId::current())
        .map(|handle| handle.queue.lock().unwrap().events.len())
        .unwrap_or(0)
}

#[test]
fn test_purge_keeps_the_current_turn_boundary() {
    let el = EventLoop::new();
    let a = ObjectId::next();
    let b = ObjectId::next();
    for receiver in [a, b, a, b] {
        post_event_to_thread(ThreadId::current(), receiver, meta_call(|| {}));
    }
    {
        let mut q = el.queue().lock().unwrap();
        q.insertion_offset = q.events.len();
    }
    // SAFETY: `a` is a fresh id that was never registered, so `unregister_qobject` finds no
    // registry record and no object memory is involved (it is memory-neutral per its
    // documentation); it only purges the posted events queued for `a` on this thread.
    unsafe { unregister_qobject(a) };
    let q = el.queue().lock().unwrap();
    assert_eq!(q.events.len(), 2);
    assert_eq!(
        q.insertion_offset, 2,
        "offset must count only surviving current-turn events"
    );
}

#[test]
fn test_metacall_to_receiver_unregistered_before_delivery_is_dropped() {
    let el = EventLoop::new();
    let mut probe = UnregisteredProbe {
        data: ObjectData::new(ObjectId::next()),
    };
    let id = probe.data.id();
    // SAFETY: per `register_qobject_ptr`'s contract: `probe` is a local that stays unmoved until
    // the `unregister_qobject` below, the pointer comes from `&raw mut`, the variable is only
    // read (`data.id()` above) and no callback of it runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };
    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = Arc::clone(&ran);
    post_event_to_thread(
        ThreadId::current(),
        id,
        meta_call(move || ran_clone.store(true, Ordering::SeqCst)),
    );
    // SAFETY: only a posted call is queued, nothing was dispatched, so no callback of `probe`
    // runs; `probe` is still alive and this is the registration thread.
    unsafe { unregister_qobject(id) };
    assert_eq!(el.send_posted_events(), 0);
    assert!(
        !ran.load(Ordering::SeqCst),
        "call to a destroyed receiver must not run"
    );
}

#[test]
fn test_event_posted_to_unregistered_receiver_is_removed_from_queue() {
    let _el = EventLoop::new();
    let mut probe = UnregisteredProbe {
        data: ObjectData::new(ObjectId::next()),
    };
    let id = probe.data.id();
    // SAFETY: per `register_qobject_ptr`'s contract: `probe` is a local that stays unmoved until
    // the `unregister_qobject` below, the pointer comes from `&raw mut`, the variable is only
    // read (`data.id()` above) and no callback of it runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };
    post_event_to_thread(ThreadId::current(), id, meta_call(|| {}));
    assert_eq!(queued_for_current_thread(), 1);
    // SAFETY: nothing was dispatched, so no callback of `probe` runs; `probe` is still alive and
    // this is the registration thread.
    unsafe { unregister_qobject(id) };
    assert_eq!(queued_for_current_thread(), 0);
}

#[test]
fn test_object_id_zero_metacall_still_runs() {
    let el = EventLoop::new();
    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = Arc::clone(&ran);
    post_event_to_thread(
        ThreadId::current(),
        ObjectId(0),
        meta_call(move || ran_clone.store(true, Ordering::SeqCst)),
    );
    assert_eq!(el.send_posted_events(), 1);
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn metacall_to_registered_borrowed_receiver_is_deferred_not_run_on_stub() {
    let _watchdog = Watchdog::arm(Duration::from_secs(10));
    let el = EventLoop::new();
    let mut probe = UnregisteredProbe {
        data: ObjectData::new(ObjectId::next()),
    };
    let id = probe.data.id();
    // SAFETY: per `register_qobject_ptr`'s contract: `probe` is a local of this test that stays
    // unmoved until the `unregister_qobject` at the end, the pointer comes from `&raw mut`, the
    // variable is only read (`data.id()` above) and no callback of it runs yet.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };
    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = Arc::clone(&ran);
    post_event_to_thread(
        ThreadId::current(),
        id,
        meta_call(move || ran_clone.store(true, Ordering::SeqCst)),
    );
    // A callback on the receiver holds its borrow while the pump runs (re-entrant case).
    let during = crate::object::with_object_mut(id, |_| el.send_posted_events());
    assert_eq!(
        during,
        Some(0),
        "posted call must not be delivered while its receiver is borrowed"
    );
    assert!(
        !ran.load(Ordering::SeqCst),
        "call must not run on the NullObj stub while its receiver is borrowed"
    );
    assert_eq!(
        el.send_posted_events(),
        1,
        "call must be delivered once the borrow ends"
    );
    assert!(ran.load(Ordering::SeqCst));
    // SAFETY: the `with_object_mut` closure and both `send_posted_events` calls have returned,
    // so no callback of `probe` runs; `probe` is a live local on the registration thread.
    unsafe { unregister_qobject(id) };
}

/// Aborts the test process when a body hangs, so a regression fails instead of hanging CI.
/// Disarmed on drop, so a passing test leaves no live thread behind.
struct Watchdog {
    done: Arc<AtomicBool>,
}

impl Watchdog {
    fn arm(limit: Duration) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + limit;
            while std::time::Instant::now() < deadline {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // Written to the raw stderr so libtest output capture cannot swallow it on exit.
            let _ = std::io::Write::write_all(
                &mut std::io::stderr(),
                format!("watchdog: test body exceeded {limit:?}\n").as_bytes(),
            );
            std::process::exit(101);
        });
        Self { done }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
    }
}

/// An exit that was requested before the loop ran returns its code at once and delivers no
/// events (the thread event loop).
#[test]
fn exec_after_a_pending_exit_returns_its_code_without_delivering_events() {
    let _watchdog = Watchdog::arm(Duration::from_secs(10));
    let event_loop = EventLoop::new();
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ran_in_call = Arc::clone(&ran);
    event_loop.sender().post_event(
        ObjectId(0),
        Event::new(EventKind::MetaCall(Box::new(move |_| {
            ran_in_call.store(true, Ordering::SeqCst);
        }))),
    );
    assert_eq!(event_loop.exec_after(|| Some(9)), 9);
    assert!(
        !ran.load(Ordering::SeqCst),
        "an event was delivered after the exit"
    );
}

/// An inner loop that returns must not disturb the outer loop's timers or exit.
#[test]
fn inner_loop_exit_returns_only_the_inner_exec_and_the_outer_loop_keeps_running() {
    let _watchdog = Watchdog::arm(Duration::from_secs(10));
    let outer = EventLoop::new();
    let outer_handle = outer.handle();
    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let log_outer = Arc::clone(&log);
    crate::timer::Timer::single_shot(0, move || {
        // A nested loop entered from a posted callback of the outer loop.
        let inner = EventLoop::new();
        let inner_exit = inner.exit.clone();
        crate::timer::Timer::single_shot(0, move || inner_exit.request(5));
        let inner_code = inner.exec();
        log_outer.lock().unwrap().push(if inner_code == 5 {
            "inner-returned"
        } else {
            "inner-wrong-code"
        });

        drop(inner);

        // Once the inner loop is gone, the outer loop must still run timers and accept exit.
        let log_timer = Arc::clone(&log_outer);
        crate::timer::Timer::single_shot(0, move || {
            log_timer.lock().unwrap().push("outer-timer");
            outer_handle.exit(0);
        });
    });

    let outer_code = outer.exec();
    assert_eq!(outer_code, 0);
    assert_eq!(*log.lock().unwrap(), vec!["inner-returned", "outer-timer"]);
}

/// A cross-thread exit ends every loop running on the target thread
#[test]
fn cross_thread_handle_exit_ends_every_loop_running_on_that_thread() {
    let _watchdog = Watchdog::arm(Duration::from_secs(10));
    let outer = EventLoop::new();
    let handle = outer.handle();
    let (inner_running_tx, inner_running_rx) = std::sync::mpsc::channel::<()>();
    let log = Arc::new(Mutex::new(Vec::<String>::new()));

    // Another thread ends the loops only after the nested loop has started running.
    let exiter = std::thread::spawn(move || {
        inner_running_rx.recv().unwrap();
        handle.exit(3);
    });

    let log_outer = Arc::clone(&log);
    crate::timer::Timer::single_shot(0, move || {
        let inner = EventLoop::new();
        crate::timer::Timer::single_shot(0, move || {
            let _ = inner_running_tx.send(());
        });
        let inner_code = inner.exec();
        log_outer
            .lock()
            .unwrap()
            .push(format!("inner-{inner_code}"));
    });

    let outer_code = outer.exec();
    exiter.join().unwrap();
    assert_eq!(
        outer_code, 3,
        "the outer loop must end with the cross-thread code"
    );
    assert_eq!(*log.lock().unwrap(), vec!["inner-3".to_string()]);
}

/// A native modal loop that stands in for a an event loop asks whether an exit is under way
/// (the thread's `quit_now` flag): false before it, true from `exit` until the loop has returned,
/// false again after.
#[test]
fn thread_quit_now_is_set_by_exit_until_the_loops_have_returned() {
    let _watchdog = Watchdog::arm(Duration::from_secs(10));
    assert!(!thread_quit_now(), "no loop");
    let el = EventLoop::new();
    assert!(!thread_quit_now(), "a loop that is not running");
    let seen = Arc::new(Mutex::new(Vec::<bool>::new()));
    let log = Arc::clone(&seen);
    let handle = el.handle();
    crate::timer::Timer::single_shot(0, move || {
        log.lock().unwrap().push(thread_quit_now());
        handle.exit(5);
        log.lock().unwrap().push(thread_quit_now());
    });
    assert_eq!(el.exec(), 5);
    assert_eq!(*seen.lock().unwrap(), vec![false, true]);
    assert!(!thread_quit_now(), "cleared when the last loop returned");
}

/// A loop that registers on a thread already reported as ended (one that starts while the
/// thread's locals are destroyed) gets the posts made after it registered; the ones made
/// while the thread counted as ended were dropped.
#[test]
fn registering_a_loop_on_an_ended_thread_makes_it_receive_posts_again() {
    let delivered = Arc::new(parking_lot::Mutex::new(Vec::<&'static str>::new()));
    let event = |name: &'static str| {
        let log = Arc::clone(&delivered);
        Event::new(EventKind::MetaCall(Box::new(move |_| {
            log.lock().push(name)
        })))
    };
    let el = EventLoop::new();
    let thread = ThreadId::current();
    mark_thread_ended(thread);
    post_event_to_thread(thread, ObjectId(0), event("while-ended"));
    register_thread_event_loop(thread, el.handle());
    post_event_to_thread(thread, ObjectId(0), event("after-register"));
    el.process_events(false);
    assert_eq!(*delivered.lock(), vec!["after-register"]);
}
