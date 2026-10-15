// SPDX-License-Identifier: LGPL-3.0-only
//! `Qt::mightBeRichText` (qtextdocument.cpp:72-130), the detection `QLabel` uses for `AutoText`
//! and `QTipLabel` uses to decide word wrap (qtooltip.cpp:152).

use ferrokit_gui::text::might_be_rich_text;

#[test]
fn a_known_tag_makes_text_rich() {
    assert!(might_be_rich_text("<b>x</b>"));
    assert!(might_be_rich_text(
        "<html><body>502 Bad Gateway</body></html>"
    ));
    assert!(might_be_rich_text("<!DOCTYPE html>"));
    assert!(might_be_rich_text("<b>a</b>\nline two"));
}

#[test]
fn an_entity_makes_text_rich() {
    assert!(might_be_rich_text("a &lt; b"));
}

#[test]
fn plain_text_with_angle_brackets_is_not_rich() {
    assert!(!might_be_rich_text("value <5 and >3 here"));
    assert!(!might_be_rich_text("<unknown>x"));
    assert!(!might_be_rich_text("line one\nline two"));
    assert!(!might_be_rich_text(""));
}
