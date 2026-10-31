// SPDX-License-Identifier: LGPL-3.0-only
//! Process commit charge added by loading each application font (raw file bytes + the parsed glyph face).
use ferrokit_gui::text::font_database::with_global_font_database;

/// Windows: process commit charge. Elsewhere: resident set from /proc/self/statm (no commit equivalent).
#[cfg(windows)]
#[repr(C)]
struct Pmc {
    cb: u32,
    page_faults: u32,
    peak_ws: usize,
    ws: usize,
    qpp: usize,
    qp: usize,
    qpnp: usize,
    qnp: usize,
    pagefile: usize,
    peak_pagefile: usize,
}
#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn K32GetProcessMemoryInfo(p: isize, c: *mut Pmc, cb: u32) -> i32;
}
#[cfg(windows)]
fn commit_mb() -> f64 {
    let mut m: Pmc = unsafe { std::mem::zeroed() };
    m.cb = std::mem::size_of::<Pmc>() as u32;
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb) };
    m.pagefile as f64 / 1048576.0
}
#[cfg(not(windows))]
fn commit_mb() -> f64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: f64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    pages * 4096.0 / 1048576.0
}

fn main() {
    with_global_font_database(|_| {});
    let base = commit_mb();
    println!("baseline (empty FontDatabase): {base:.1} MB commit");
    for fam in [
        "Segoe UI",
        "Consolas",
        "Microsoft JhengHei",
        "Microsoft YaHei",
        "Segoe UI Symbol",
        "Segoe UI Emoji",
    ] {
        let before = commit_mb();
        let (ok, raw) = with_global_font_database(|db| {
            let ok = db.load_font(fam).is_some();
            (ok, db.total_raw_bytes())
        });
        let after = commit_mb();
        println!(
            "{fam:<20} loaded={ok:<5} commit +{:>6.1} MB   (cumulative raw file bytes held: {:.1} MB)",
            after - before,
            raw as f64 / 1048576.0
        );
    }
    println!("total added: {:.1} MB", commit_mb() - base);
}
