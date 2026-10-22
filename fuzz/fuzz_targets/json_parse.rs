#![no_main]
//! JSON parser: arbitrary bytes must never panic; whatever parses must survive
//! parse -> serialize -> parse unchanged (compact and indented).

use ferrokit_core::json::{self, JsonArray, JsonObject, JsonValue};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(value) = json::from_slice(data) else {
        return;
    };
    // The writer emits `null` for numbers JSON cannot represent (`1e999` parses to infinity).
    let expected = representable(&value);
    for text in [json::to_string(&value), json::to_string_pretty(&value)] {
        let again = json::from_str(&text)
            .unwrap_or_else(|e| panic!("serialized JSON does not reparse ({e}): {text}"));
        assert!(expected == again, "round trip changed the value: {text}");
    }
});

fn representable(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Number(n) if !n.is_finite() => JsonValue::Null,
        JsonValue::Undefined => JsonValue::Null,
        JsonValue::Array(a) => {
            let mut out = JsonArray::new();
            for v in a.iter() {
                out.push(representable(v));
            }
            JsonValue::Array(out)
        }
        JsonValue::Object(o) => {
            let mut out = JsonObject::new();
            for (k, v) in o.iter() {
                out.insert(k.as_str(), representable(v));
            }
            JsonValue::Object(out)
        }
        other => other.clone(),
    }
}
