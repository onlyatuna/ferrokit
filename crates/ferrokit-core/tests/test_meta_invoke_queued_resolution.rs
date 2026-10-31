// SPDX-License-Identifier: LGPL-3.0-only
use std::any::Any;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::sync::Arc;
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
use ferrokit_core::signal::{ConnectionType, Signal};
use ferrokit_core::variant::Variant;
use parking_lot::Mutex;

/// Two overloads of `tag`: `tag(QString)` is declared first and `tag(int)` second. An `I64`
/// argument is accepted only by `tag(int)`.
struct TagWidget {
    data: ObjectData,
    ran: i32,
}

impl QObject for TagWidget {
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
        &TAG_META
    }
}

fn new_tag_widget(id: ObjectId) -> TagWidget {
    TagWidget {
        data: ObjectData::new(id),
        ran: 0,
    }
}

fn tag_string(obj: &mut dyn Any, _: &[Variant]) -> Result<Variant, InvokeError> {
    let w = obj
        .downcast_mut::<TagWidget>()
        .ok_or(InvokeError::TargetBorrowFailed)?;
    w.ran = 1;
    Ok(Variant::Invalid)
}

fn tag_int(obj: &mut dyn Any, _: &[Variant]) -> Result<Variant, InvokeError> {
    let w = obj
        .downcast_mut::<TagWidget>()
        .ok_or(InvokeError::TargetBorrowFailed)?;
    w.ran = 2;
    Ok(Variant::Invalid)
}

static TAG_METHODS: &[MetaMethod] = &[
    MetaMethod::new(
        "tag",
        "tag(QString)",
        "void",
        &["QString"],
        &["value"],
        MethodType::Slot,
        Access::Public,
        Some(tag_string),
    ),
    MetaMethod::new(
        "tag",
        "tag(int)",
        "void",
        &["int"],
        &["value"],
        MethodType::Slot,
        Access::Public,
        Some(tag_int),
    ),
];

static TAG_META: MetaObject = MetaObject::new(
    "TagWidget",
    Some(&QOBJECT_META_OBJECT),
    TAG_METHODS,
    &[],
    &[],
    &[],
);

#[derive(ferrokit_core::QObject)]
#[qobject(class_name = "QueuedSignalProbe")]
struct QueuedSignalProbe {
    data: ObjectData,
    #[signal]
    changed: Signal<String>,
}

/// What the registered widget `id` ran, read through the registry. The predicate given to
/// `pump_until` uses this, not a reference to the widget: a reference held while the loop runs
/// would still be live when a queued invoke writes through the registry's pointer.
fn ran_of(id: ObjectId) -> Option<i32> {
    with_object(id, |o| {
        o.as_qobject_any()
            .and_then(|any| any.downcast_ref::<TagWidget>())
            .map(|w| w.ran)
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
fn queued_name_only_invoke_runs_first_compatible_overload() {
    // Qt: invokeMethodImpl resolves the member before posting and takes the first declared
    // overload whose parameter types match (qmetaobject.cpp:1693-1708).
    let el = EventLoop::new();
    let id = ObjectId::next();
    let mut w = new_tag_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let posted = w.invoke_method_with_type("tag", &[Variant::I64(3)], ConnectionType::Queued);
    pump_until(&el, "queued tag", || ran_of(id).is_some_and(|ran| ran != 0));
    let ran = w.ran;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(posted, Ok(Variant::Invalid));
    assert_eq!(
        ran, 2,
        "queued name-only invoke must run tag(int), the first overload accepting an int"
    );
}

#[test]
fn queued_signature_invoke_rejects_wrong_argument_type_without_posting() {
    // Qt: invokeImpl checks argument types before queueing (qmetaobject.cpp:2774-2809).
    let el = EventLoop::new();
    let id = ObjectId::next();
    let mut w = new_tag_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let r = w.invoke_method_with_type("tag(QString)", &[Variant::I64(3)], ConnectionType::Queued);
    el.process_events(false);
    let ran = w.ran;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(
        r,
        Err(InvokeError::TypeMismatch {
            index: 0,
            expected: "QString",
        })
    );
    assert_eq!(ran, 0, "refused queued call must not run");
}

#[test]
fn queued_name_only_with_no_compatible_overload_is_method_not_found_without_posting() {
    // Qt: no candidate passes invokeImpl, so invokeMethodImpl reports not-found and posts nothing
    // (qmetaobject.cpp:1693-1712).
    let el = EventLoop::new();
    let id = ObjectId::next();
    let mut w = new_tag_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let r = w.invoke_method_with_type("tag", &[Variant::F64(1.5)], ConnectionType::Queued);
    el.process_events(false);
    let ran = w.ran;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(r, Err(InvokeError::MethodNotFound));
    assert_eq!(ran, 0, "unmatched queued call must not run");
}

#[test]
fn queued_signal_invoke_emits_on_owner_loop() {
    // Qt (observed with PySide6 6.11.2): a queued invoke of a signal
    // by name reaches its receivers only once the event loop runs.
    let el = EventLoop::new();
    let mut probe = QueuedSignalProbe {
        data: ObjectData::with_auto_id(),
        changed: Signal::new(),
    };
    let id = probe.data.id();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    probe.changed.connect(move |v| s.lock().push(v.clone()));

    // SAFETY: `probe` stays on this thread and alive until unregistered below.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut probe)) };
    let posted = probe.invoke_method_with_type(
        "changed(String)",
        &[Variant::String("q".into())],
        ConnectionType::Queued,
    );
    let before_loop = seen.lock().clone();
    pump_until(&el, "queued signal", || !seen.lock().is_empty());

    // SAFETY: `probe` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(posted, Ok(Variant::Invalid));
    assert!(
        before_loop.is_empty(),
        "queued signal emitted before the owner loop ran"
    );
    assert_eq!(seen.lock().as_slice(), &["q".to_string()]);
}

#[test]
fn cross_thread_queued_signature_rejects_wrong_argument_type_without_posting() {
    // Qt: invokeImpl checks argument types before queueing, so a cross-thread refusal posts nothing
    // (qmetaobject.cpp:2774-2809).
    let el = EventLoop::new();
    let id = ObjectId::next();
    let mut w = new_tag_widget(id);
    // SAFETY: `w` stays on this thread and alive until unregistered below; the worker only uses
    // the receiver id and the static MetaObject, never the object itself.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut w)) };

    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = invoke_method_on_thread(
            id,
            &TAG_META,
            "tag(QString)",
            &[Variant::I64(3)],
            ConnectionType::Queued,
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
            "watchdog: queued invoke not answered within 5s"
        );
        el.process_events(false);
        thread::sleep(Duration::from_millis(1));
    };
    worker.join().expect("worker thread must not panic");

    // Pump again so any call wrongly posted before the refusal would run now.
    el.process_events(false);
    let ran = w.ran;

    // SAFETY: `w` was registered above and is still alive.
    unsafe { unregister_qobject(id) };

    assert_eq!(
        result,
        Err(InvokeError::TypeMismatch {
            index: 0,
            expected: "QString",
        })
    );
    assert_eq!(ran, 0, "refused cross-thread call must not run");
}
