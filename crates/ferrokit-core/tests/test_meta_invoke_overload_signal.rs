// SPDX-License-Identifier: LGPL-3.0-only
use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ferrokit_core::meta::{
    Access, InvokeError, MetaMethod, MetaObject, MethodType, QOBJECT_META_OBJECT,
};
use ferrokit_core::object::{ObjectData, QObject};
use ferrokit_core::signal::Signal;
use ferrokit_core::variant::Variant;
use parking_lot::Mutex;

#[derive(ferrokit_core::QObject)]
#[qobject(class_name = "InvokeProbe")]
struct InvokeProbe {
    data: ObjectData,
    #[signal]
    changed: Signal<String>,
}

impl InvokeProbe {
    fn new() -> Self {
        Self {
            data: ObjectData::with_auto_id(),
            changed: Signal::new(),
        }
    }
}

#[test]
fn derived_signal_invoke_by_name_emits_like_emit() {
    // Qt (observed with PySide6 6.11.2): invoking a signal by name
    // emits it to its receivers, the same as emitting it directly.
    let mut probe = InvokeProbe::new();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    probe.changed.connect(move |v| s.lock().push(v.clone()));

    let meta = probe.meta_object();
    let r = meta.invoke_method(&mut probe, "changed", &[Variant::String("x".into())]);
    assert_eq!(r, Ok(Variant::Invalid));
    assert_eq!(seen.lock().as_slice(), &["x".to_string()]);
}

#[test]
fn derived_signal_invoke_checks_argument_type_before_emitting() {
    // Qt: invokeImpl checks argument types before calling the target (qmetaobject.cpp:2774-2809).
    let mut probe = InvokeProbe::new();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    probe.changed.connect(move |v| s.lock().push(v.clone()));

    let meta = probe.meta_object();
    let r = meta.invoke_method(&mut probe, "changed(String)", &[Variant::I64(5)]);
    assert_eq!(
        r,
        Err(InvokeError::TypeMismatch {
            index: 0,
            expected: "String",
        })
    );
    assert!(
        seen.lock().is_empty(),
        "signal emitted despite type mismatch"
    );
}

static OVERLOAD_RAN: AtomicUsize = AtomicUsize::new(0);

fn run_int(_: &mut dyn Any, _: &[Variant]) -> Result<Variant, InvokeError> {
    OVERLOAD_RAN.store(1, Ordering::SeqCst);
    Ok(Variant::Invalid)
}

fn run_qlonglong(_: &mut dyn Any, _: &[Variant]) -> Result<Variant, InvokeError> {
    OVERLOAD_RAN.store(2, Ordering::SeqCst);
    Ok(Variant::Invalid)
}

static OVERLOAD_META: MetaObject = MetaObject::new(
    "OverloadProbe",
    Some(&QOBJECT_META_OBJECT),
    &[
        MetaMethod::new(
            "tag",
            "tag(int)",
            "void",
            &["int"],
            &["value"],
            MethodType::Slot,
            Access::Public,
            Some(run_int),
        ),
        MetaMethod::new(
            "tag",
            "tag(qlonglong)",
            "void",
            &["qlonglong"],
            &["value"],
            MethodType::Slot,
            Access::Public,
            Some(run_qlonglong),
        ),
    ],
    &[],
    &[],
    &[],
);

#[test]
fn name_only_invoke_picks_first_declared_compatible_overload() {
    // Qt: invokeMethodImpl scans methods in declaration order and runs the first whose parameter
    // types match; a FormalParameterMismatch moves on to the next candidate (qmetaobject.cpp:1693-1708,
    // qmetaobject_p.h:98-102).
    OVERLOAD_RAN.store(0, Ordering::SeqCst);
    let mut target = 0u8;
    let r = OVERLOAD_META.invoke_method(&mut target, "tag", &[Variant::I64(3)]);
    assert_eq!(r, Ok(Variant::Invalid));
    assert_eq!(
        OVERLOAD_RAN.load(Ordering::SeqCst),
        1,
        "name-only invoke did not pick the first declared compatible overload"
    );
}

#[test]
fn name_only_invoke_without_compatible_overload_is_method_not_found() {
    // Qt: no method matches the argument types, so invokeMethod fails (qmetaobject.cpp:651-687).
    OVERLOAD_RAN.store(0, Ordering::SeqCst);
    let mut target = 0u8;
    let r = OVERLOAD_META.invoke_method(&mut target, "tag", &[Variant::String("x".into())]);
    assert_eq!(r, Err(InvokeError::MethodNotFound));
    assert_eq!(
        OVERLOAD_RAN.load(Ordering::SeqCst),
        0,
        "overload ran despite no match"
    );
}

#[test]
fn signature_invoke_rejects_string_for_int_parameter() {
    // Qt: invokeImpl checks each argument type before calling the slot (qmetaobject.cpp:2774-2809).
    OVERLOAD_RAN.store(0, Ordering::SeqCst);
    let mut target = 0u8;
    let r = OVERLOAD_META.invoke_method(&mut target, "tag(int)", &[Variant::String("5".into())]);
    assert_eq!(
        r,
        Err(InvokeError::TypeMismatch {
            index: 0,
            expected: "int",
        })
    );
    assert_eq!(
        OVERLOAD_RAN.load(Ordering::SeqCst),
        0,
        "slot ran despite type mismatch"
    );
}
