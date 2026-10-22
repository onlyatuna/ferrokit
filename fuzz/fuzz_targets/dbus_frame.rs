#![no_main]
//! D-Bus frame decoding: `frame_len`/`from_bytes` on arbitrary bytes, a decoded message must
//! re-encode to something that decodes to the same message, and `DbusConnection::read_incoming`
//! must cope with the same bytes arriving in arbitrary-sized reads.

use ferrokit_platform::tray::dbus_connection::scripted::ScriptedDbusTransport;
use ferrokit_platform::tray::{DbusConnection, DbusMessage};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(msg) = DbusMessage::from_bytes(data) {
        if msg.signature.len() <= 255 {
            let wire = msg.to_bytes();
            let again = DbusMessage::from_bytes(&wire)
                .unwrap_or_else(|e| panic!("re-encoded message does not decode: {e}"));
            let mut expected = msg.clone();
            // `to_bytes` always writes little-endian headers; the body is carried over verbatim.
            expected.big_endian = false;
            assert_eq!(again, expected, "round trip changed the message");
        }
        let _ = msg.body_string();
    }

    // Same bytes through the connection's incremental decoder, in chunks chosen by the input.
    let chunk = usize::from(data.first().copied().unwrap_or(0) % 32) + 1;
    let (transport, bus) = ScriptedDbusTransport::new(":1.1");
    let Ok(mut conn) = DbusConnection::from_transport(Box::new(transport), "fuzz:", 1000) else {
        return;
    };
    bus.limit_read_size(chunk);
    bus.push_to_client(data);
    for _ in 0..4 {
        if conn.read_incoming().is_err() {
            break;
        }
    }
});
