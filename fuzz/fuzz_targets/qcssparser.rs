#![no_main]
//! Qt style sheet parser: arbitrary text must never panic, hang or exhaust memory.

use ferrokit_gui::text::qcssparser::{parse_color, parse_length, QCssParser, QCssStyleSheet};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let sheet = QCssStyleSheet::parse(&text);
    let _ = sheet.is_empty();
    let _ = QCssParser::parse(&text);
    let _ = parse_length(&text);
    let _ = parse_color(&text);
});
