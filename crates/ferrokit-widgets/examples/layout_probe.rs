// SPDX-License-Identifier: LGPL-3.0-only
//! Differential probe for the layout port: reads one layout per line from stdin, lays it out and
//! prints the rectangle of every widget item, to be compared with PySide6 by
//! a script. The line grammar and the layout
//! construction live in `tests/support/layout_probe.rs`, shared with the in-process oracle test.

#[path = "../tests/support/layout_probe.rs"]
mod layout_probe;

use std::io::BufRead;

fn main() {
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("stdin");
        println!(
            "{}",
            layout_probe::format_geoms(&layout_probe::lay_out_case(&line))
        );
    }
}
