// SPDX-License-Identifier: LGPL-3.0-only
use std::any::Any;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::meta::{
    Access, InvokeError, MetaMethod, MetaObject, MethodType, QOBJECT_META_OBJECT,
};
use ferrokit_core::object::{
    invoke_method_on_thread, register_qobject_ptr, unregister_qobject, with_object, ObjectData,
    ObjectId, QObject,
};
use ferrokit_core::signal::ConnectionType;
use ferrokit_core::variant::Variant;

const ADD_WIDTH: &str = "addWidth(int)";

struct InvokeWidget {
    data: ObjectData,
    width: i32,
}

impl QObject for InvokeWidget {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }

    fn as_qobject_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn meta_object(&self) -> &'static MetaObject {
        &INVOKE_WIDGET_META_OBJECT
    }
}

fn add_width(obj: &mut dyn Any, args: &[Variant]) -> Result<Variant, InvokeError> {
    let widget = obj
        .downcast_mut::<InvokeWidget>()
        .ok_or(InvokeError::TargetBorrowFailed)?;
    let delta =
        args.first()
            .and_then(|v| v.to_value::<i32>())
            .ok_or(InvokeError::TypeMismatch {
                index: 0,
                expected: "i32",
            })?;
    widget.width += delta;
    Ok(Variant::from(widget.width))
}

static INVOKE_WIDGET_METHODS: &[MetaMethod] = &[MetaMethod::new(
    "addWidth",
    ADD_WIDTH,
    "int",
    &["int"],
    &["delta"],
    MethodType::Method,
    Access::Public,
    Some(add_width),
)];

static INVOKE_WIDGET_META_OBJECT: MetaObject = MetaObject::new(
    "InvokeWidget",
    Some(&QOBJECT_META_OBJECT),
    INVOKE_WIDGET_METHODS,
    &[],
    &[],
    &[],
);

fn new_widget(id: ObjectId) -> InvokeWidget {
    InvokeWidget {
        data: ObjectData::new(id),
        width: 0,
    }
}

/// The width of the registered widget `id`, read through the registry. The predicate given to
/// `pump_until` uses this, not a reference to the widget: a reference held while the loop runs
/// would still be live when a queued invoke writes through the registry's pointer.
fn width_of(id: ObjectId) -> Option<i32> {
    with_object(id, |o| {
        o.as_qobject_any()
            .and_then(|any| any.downcast_ref::<InvokeWidget>())
            .map(|w| w.width)
    })
    .flatten()
}

/// Pumps the loop on this thread until `done` holds; panics if it never does (watchdog).
fn pump_until(el: &EventLoop, what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "watchdog: {what} not delivered within 5s"
        );
        el.process_events(false);
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn direct_and_auto_on_owner_thread_invoke_immediately() {
    let id = ObjectId::next();
    let mut w = new_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let direct =
        w.invoke_method_with_type(ADD_WIDTH, &[Variant::from(5i32)], ConnectionType::Direct);
    let auto = w.invoke_method_with_type(ADD_WIDTH, &[Variant::from(2i32)], ConnectionType::Auto);
    let width = w.width;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(direct.map(|v| v.to_value::<i32>()), Ok(Some(5)));
    assert_eq!(auto.map(|v| v.to_value::<i32>()), Ok(Some(7)));
    assert_eq!(width, 7);
}

#[test]
fn queued_on_owner_thread_is_deferred_until_loop_runs() {
    let el = EventLoop::new();
    let id = ObjectId::next();
    let mut w = new_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let posted =
        w.invoke_method_with_type(ADD_WIDTH, &[Variant::from(3i32)], ConnectionType::Queued);
    let width_before_loop = w.width;
    pump_until(&el, "queued invoke", || width_of(id) == Some(3));
    let width_after = w.width;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(
        posted,
        Ok(Variant::Invalid),
        "queued invoke returns at once"
    );
    assert_eq!(
        width_before_loop, 0,
        "queued invoke must not run before the loop"
    );
    assert_eq!(width_after, 3);
}

#[test]
fn blocking_queued_on_owner_thread_is_refused_without_hanging() {
    let id = ObjectId::next();
    let mut w = new_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let result = w.invoke_method_with_type(
        ADD_WIDTH,
        &[Variant::from(9i32)],
        ConnectionType::BlockingQueued,
    );
    let width = w.width;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert!(
        matches!(result, Err(InvokeError::ExecutionFailed(_))),
        "same-thread BlockingQueued must be refused, got {result:?}"
    );
    assert_eq!(width, 0, "refused call must not run");
}

#[test]
fn queued_rejects_unknown_member_and_bad_arity_synchronously() {
    let el = EventLoop::new();
    let id = ObjectId::next();
    let mut w = new_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let unknown = w.invoke_method_with_type("nonExistent()", &[], ConnectionType::Queued);
    let bad_arity = w.invoke_method_with_type(ADD_WIDTH, &[], ConnectionType::Queued);
    // Pump once so anything wrongly posted would run before the assertions.
    el.process_events(false);
    let width = w.width;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(unknown, Err(InvokeError::MethodNotFound));
    assert_eq!(
        bad_arity,
        Err(InvokeError::ParameterCountMismatch {
            expected: 1,
            actual: 0
        })
    );
    assert_eq!(width, 0, "rejected calls must not be posted");
}

#[test]
fn queued_on_unregistered_object_is_target_borrow_failed() {
    let id = ObjectId::next();
    let mut w = new_widget(id);

    let result =
        w.invoke_method_with_type(ADD_WIDTH, &[Variant::from(1i32)], ConnectionType::Queued);

    assert_eq!(result, Err(InvokeError::TargetBorrowFailed));
}

#[test]
fn cross_thread_blocking_queued_returns_owner_thread_result() {
    let el = EventLoop::new();
    let id = ObjectId::next();
    let mut w = new_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below; the worker only uses
    // the receiver id and the static MetaObject, never the object itself.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = invoke_method_on_thread(
            id,
            &INVOKE_WIDGET_META_OBJECT,
            ADD_WIDTH,
            &[Variant::from(4i32)],
            ConnectionType::BlockingQueued,
        );
        let _ = tx.send(result);
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    let result = loop {
        if let Ok(result) = rx.try_recv() {
            break result;
        }
        assert!(
            Instant::now() < deadline,
            "watchdog: blocking invoke not answered within 5s"
        );
        el.process_events(false);
        thread::sleep(Duration::from_millis(1));
    };
    worker.join().expect("worker thread must not panic");
    let width = w.width;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(result.map(|v| v.to_value::<i32>()), Ok(Some(4)));
    assert_eq!(width, 4);
}

#[test]
fn cross_thread_queued_returns_at_once_and_applies_on_owner_loop() {
    let el = EventLoop::new();
    let id = ObjectId::next();
    let mut w = new_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below; the worker only uses
    // the receiver id and the static MetaObject, never the object itself.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = invoke_method_on_thread(
            id,
            &INVOKE_WIDGET_META_OBJECT,
            ADD_WIDTH,
            &[Variant::from(6i32)],
            ConnectionType::Queued,
        );
        let _ = tx.send(result);
    });

    let returned = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("watchdog: queued invoke must return without waiting for the owner loop");
    worker.join().expect("worker thread must not panic");
    let width_before_loop = w.width;
    pump_until(&el, "cross-thread queued invoke", || width_of(id) == Some(6));
    let width_after = w.width;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(returned, Ok(Variant::Invalid));
    assert_eq!(
        width_before_loop, 0,
        "queued invoke must wait for the owner loop"
    );
    assert_eq!(width_after, 6);
}
