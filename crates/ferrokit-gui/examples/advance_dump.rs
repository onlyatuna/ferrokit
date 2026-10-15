// SPDX-License-Identifier: LGPL-3.0-only
//! Prints `horizontal_advance_exact` for tab-separated `family  px  weight  text` lines on stdin, for
//! comparison against `QFontMetricsF::horizontalAdvance`.
//!
//! Usage: advance_dump <device pixel ratio> < lines
use ferrokit_gui::text::font::{Font, FontWeight};
use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
use ferrokit_gui::text::font_metrics::FontMetrics;
use std::io::BufRead;

fn weight(w: u32) -> FontWeight {
    match w {
        ..=149 => FontWeight::Thin,
        150..=249 => FontWeight::ExtraLight,
        250..=349 => FontWeight::Light,
        350..=449 => FontWeight::Normal,
        450..=549 => FontWeight::Medium,
        550..=649 => FontWeight::SemiBold,
        650..=749 => FontWeight::Bold,
        750..=849 => FontWeight::ExtraBold,
        _ => FontWeight::Black,
    }
}

fn main() {
    set_application_device_pixel_ratio(std::env::args().nth(1).map_or(1.0, |a| a.parse().expect("device pixel ratio")));
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("stdin");
        let mut f = line.splitn(4, '\t');
        let (family, px, w, text) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap(), f.next().unwrap_or(""));
        let font = Font::new(family, px.parse().unwrap()).with_weight(weight(w.parse().unwrap()));
        let width = FontMetrics::from_font(&font).horizontal_advance_exact(text, &font);
        println!("{width:.4}");
    }
}
