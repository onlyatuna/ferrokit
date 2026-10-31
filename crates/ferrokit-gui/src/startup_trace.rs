// SPDX-License-Identifier: LGPL-3.0-only
//! Opt-in startup timing (`FERROKIT_STARTUP_TRACE=1`, or a log file path).
//!
//! Every line is `epoch_ms +since_first_event_ms [thread] indent label  (duration)`. The epoch
//! column lets an external launcher align the trace with the process start time. Disabled by default:
//! the cost is one cached bool test per call site and no label is built.

use std::cell::Cell;
use std::fs::File;
use std::io::Write;
use std::sync::{LazyLock, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Sink {
    file: Option<File>,
    t0: Instant,
}

static SINK: LazyLock<Option<Mutex<Sink>>> = LazyLock::new(|| {
    let v = std::env::var("FERROKIT_STARTUP_TRACE")
        .ok()
        .filter(|v| !v.is_empty() && v != "0")?;
    let path = if v == "1" {
        std::env::temp_dir().join("ferrokit_startup_trace.log")
    } else {
        std::path::PathBuf::from(v)
    };
    let file = File::create(path).ok();
    Some(Mutex::new(Sink {
        file,
        t0: Instant::now(),
    }))
});

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// `true` when `FERROKIT_STARTUP_TRACE` is set to something other than `0`/empty.
pub fn enabled() -> bool {
    SINK.is_some()
}

fn write_line(label: &str, dur_ms: Option<f64>, depth: usize) {
    let Some(sink) = SINK.as_ref() else { return };
    let Ok(mut s) = sink.lock() else { return };
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let rel = s.t0.elapsed().as_secs_f64() * 1000.0;
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("?").to_owned();
    let indent = "  ".repeat(depth);
    let line = match dur_ms {
        Some(d) => format!("{epoch} +{rel:9.2} [{name}] {indent}{label}  ({d:.2} ms)\n"),
        None => format!("{epoch} +{rel:9.2} [{name}] {indent}{label}\n"),
    };
    if let Some(f) = s.file.as_mut() {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

/// Records an instant event.
pub fn mark(label: impl FnOnce() -> String) {
    if enabled() {
        write_line(&label(), None, DEPTH.with(Cell::get));
    }
}

/// Times the enclosing scope. Spans shorter than `min_ms` are not written (nested spans still are).
#[must_use = "the span is recorded when dropped"]
pub struct Span {
    start: Option<(Instant, String, f64)>,
}

pub fn span(label: impl FnOnce() -> String) -> Span {
    span_min(0.0, label)
}

pub fn span_min(min_ms: f64, label: impl FnOnce() -> String) -> Span {
    if !enabled() {
        return Span { start: None };
    }
    DEPTH.with(|d| d.set(d.get() + 1));
    Span {
        start: Some((Instant::now(), label(), min_ms)),
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some((t, label, min_ms)) = self.start.take() {
            let depth = DEPTH.with(|d| {
                d.set(d.get().saturating_sub(1));
                d.get()
            });
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ms >= min_ms {
                write_line(&label, Some(ms), depth);
            }
        }
    }
}
