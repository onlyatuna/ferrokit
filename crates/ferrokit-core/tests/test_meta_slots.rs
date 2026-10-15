// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::meta::InvokeError;
use ferrokit_core::object::{ObjectData, QObject};
use ferrokit_core::variant::Variant;

#[derive(ferrokit_core::QObject)]
#[qobject(class_name = "SlotProbe")]
struct SlotProbe {
    data: ObjectData,
    caption: String,
    calls: usize,
}

#[ferrokit_core::slots]
impl SlotProbe {
    #[slot]
    fn set_caption(&mut self, text: String) {
        self.caption = text;
        self.calls += 1;
    }

    #[slot]
    fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }

    #[slot]
    fn reset(&mut self) {
        self.calls = 0;
    }

    fn not_a_slot(&self) -> i32 {
        7
    }
}

fn new_probe() -> SlotProbe {
    SlotProbe {
        data: ObjectData::with_auto_id(),
        caption: String::new(),
        calls: 0,
    }
}

#[test]
fn slot_invoked_by_name_runs_method() {
    // Qt: a slot declared in the class is registered in its meta object and invokable by name
    // (observed with PySide6 6.11.2).
    let mut probe = new_probe();
    let r = probe.meta_object().invoke_method(
        &mut probe,
        "set_caption",
        &[Variant::String("hi".into())],
    );
    assert_eq!(r, Ok(Variant::Invalid));
    assert_eq!(probe.caption, "hi");
    assert_eq!(probe.calls, 1);
}

#[test]
fn slot_invoked_by_signature_runs_method() {
    let mut probe = new_probe();
    let r = probe.meta_object().invoke_method(
        &mut probe,
        "set_caption(String)",
        &[Variant::String("sig".into())],
    );
    assert_eq!(r, Ok(Variant::Invalid));
    assert_eq!(probe.caption, "sig");
}

#[test]
fn slot_return_value_is_converted_to_variant() {
    let mut probe = new_probe();
    let r = probe.meta_object().invoke_method(
        &mut probe,
        "add(i32,i32)",
        &[Variant::I64(2), Variant::I64(40)],
    );
    assert_eq!(r, Ok(Variant::from(42i32)));
}

#[test]
fn slot_without_return_value_is_invalid_variant() {
    let mut probe = new_probe();
    probe.calls = 5;
    let r = probe.meta_object().invoke_method(&mut probe, "reset", &[]);
    assert_eq!(r, Ok(Variant::Invalid));
    assert_eq!(probe.calls, 0);
}

#[test]
fn slot_signature_rejects_argument_of_wrong_type_without_running() {
    // Qt: invokeImpl checks argument types before calling the slot (qmetaobject.cpp:2774-2809).
    let mut probe = new_probe();
    let r =
        probe
            .meta_object()
            .invoke_method(&mut probe, "set_caption(String)", &[Variant::I64(5)]);
    assert_eq!(
        r,
        Err(InvokeError::TypeMismatch {
            index: 0,
            expected: "String",
        })
    );
    assert_eq!(probe.caption, "");
    assert_eq!(probe.calls, 0);
}

#[test]
fn slot_name_only_with_no_compatible_overload_is_method_not_found() {
    // Qt: invokeMethod returns false when no overload matches the argument types (qmetaobject.cpp:651-687).
    let mut probe = new_probe();
    let r = probe
        .meta_object()
        .invoke_method(&mut probe, "set_caption", &[Variant::I64(5)]);
    assert_eq!(r, Err(InvokeError::MethodNotFound));
    assert_eq!(probe.calls, 0);
}

#[test]
fn method_without_slot_marker_is_not_invokable() {
    let mut probe = new_probe();
    assert_eq!(probe.not_a_slot(), 7);
    let r = probe
        .meta_object()
        .invoke_method(&mut probe, "not_a_slot", &[]);
    assert_eq!(r, Err(InvokeError::MethodNotFound));
}
