// SPDX-License-Identifier: LGPL-3.0-only
//! Comprehensive integration tests for Rich Text, Formatting, Document, Layout, and Syntax Highlighting.

use ferrokit_gui::text::*;
use tiny_skia::Color;

#[test]
fn test_cursor_advances_by_document_growth_for_combining_marks() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("XY");
    let mut cursor = TextCursor::at_position(1);
    cursor.insert_text(&mut doc, "\u{0915}");
    cursor.insert_text(&mut doc, "\u{093F}"); // vowel sign merges into the previous cluster
    assert_eq!(doc.to_plain_text(), "X\u{0915}\u{093F}Y");
    assert_eq!(cursor.position(), 2);
}

#[test]
fn test_markdown_deeply_nested_links_do_not_overflow_the_stack() {
    // Miri interprets the quadratic rescans far slower; the depth cap is exercised either way.
    let n = if cfg!(miri) { 200 } else { 20_000 };
    let md = format!("{}a{}", "[".repeat(n), "](u)".repeat(n));
    let mut doc = TextDocument::new();
    doc.set_markdown(&md);
    assert!(doc.character_count() > 0);
}

#[test]
fn test_text_coordinates_and_graphemes() {
    // String with ASCII, CJK, and complex ZWJ emoji
    // "Hi! 🇨🇦 👨‍👩‍👧‍👦 世界"
    // Graphemes:
    // 0: "H"
    // 1: "i"
    // 2: "!"
    // 3: " "
    // 4: "🇨🇦" (Flag, 2 regional indicators)
    // 5: " "
    // 6: "👨‍👩‍👧‍👦" (Family emoji, 7 scalars joined with ZWJ)
    // 7: " "
    // 8: "世"
    // 9: "界"
    let text = "Hi! 🇨🇦 👨‍👩‍👧‍👦 世界";
    assert_eq!(grapheme_count(text), 10);

    // Grapheme 4 is the flag
    let pos_flag = TextPosition::from_grapheme(text, 4);
    assert_eq!(pos_flag.grapheme.0, 4);
    assert_eq!(&text[pos_flag.byte.0..pos_flag.byte.0 + 8], "🇨🇦");

    // Grapheme 6 is the family emoji
    let pos_family = TextPosition::from_grapheme(text, 6);
    assert_eq!(pos_family.grapheme.0, 6);
    assert_eq!(pos_family.next_grapheme(text).grapheme.0, 7);
    assert_eq!(pos_family.prev_grapheme(text).grapheme.0, 5);

    // Byte to grapheme roundtrip
    let pos_from_byte = TextPosition::from_byte(text, pos_family.byte.0);
    assert_eq!(pos_from_byte.grapheme.0, 6);

    // Directed TextRange
    let range = TextRange::from_graphemes(text, 4, 7); // "🇨🇦 👨‍👩‍👧‍👦"
    assert_eq!(range.grapheme_len(), 3);
    assert_eq!(range.slice_str(text), "🇨🇦 👨‍👩‍👧‍👦");
    assert!(!range.is_reversed());

    let rev_range = TextRange::new(range.position, range.anchor);
    assert!(rev_range.is_reversed());
    assert_eq!(rev_range.slice_str(text), "🇨🇦 👨‍👩‍👧‍👦");
}

#[test]
fn text_positions_use_grapheme_boundaries_for_unicode_conversions() {
    // "é e\u{301}😀Z": é(2B,1u16) + ' '(1B,1u16) + e+combining(3B,2u16) + 😀(4B,2u16) + Z(1B,1u16)
    // Grapheme:   0→byte 0  1→byte 2  2→byte 3  3→byte 6  4→byte 10  end→byte 11
    let text = "é e\u{301}😀Z";

    // from_grapheme: each grapheme starts at the correct byte offset.
    assert_eq!(
        TextPosition::from_grapheme(text, 1),
        TextPosition {
            grapheme: GraphemeIndex(1),
            byte: ByteOffset(2),
            utf16: Utf16Offset(1),
        }
    );
    assert_eq!(TextPosition::from_grapheme(text, 2).byte.0, 3); // e+combining starts at byte 3
    assert_eq!(TextPosition::from_grapheme(text, 3).byte.0, 6); // 😀 starts at byte 6

    // Byte offsets inside a scalar or a multi-scalar grapheme snap to that grapheme's start.
    assert_eq!(TextPosition::from_byte(text, 1).grapheme.0, 0); // byte 1 inside é → grapheme 0
    assert_eq!(TextPosition::from_byte(text, 4).grapheme.0, 2); // byte 4 inside e+combining → grapheme 2
    assert_eq!(
        TextPosition::from_byte(text, text.len() + 1),
        TextPosition::end_of(text)
    );

    // UTF-16: astral scalars use two code units; offsets inside a surrogate pair snap to grapheme start.
    assert_eq!(TextPosition::from_utf16(text, 3).byte.0, 3); // utf16=3 = combining accent → snaps to e+combining start byte 3
    assert_eq!(TextPosition::from_utf16(text, 4).byte.0, 6); // utf16=4 = start of 😀 surrogate → byte 6
    assert_eq!(TextPosition::from_utf16(text, 5).byte.0, 6); // utf16=5 = inside 😀 surrogate → snaps to byte 6
    assert_eq!(utf16_to_byte(text, usize::MAX), text.len());
    assert_eq!(byte_to_utf16(text, 9), 4); // byte 9 is inside 😀 (bytes 6-9) → utf16 offset 4 (start of surrogate pair)
}

#[test]
fn text_range_slicing_clamps_invalid_and_out_of_range_boundaries() {
    let range = TextRange::new(
        TextPosition {
            grapheme: GraphemeIndex(0),
            byte: ByteOffset(1), // Inside the first scalar.
            utf16: Utf16Offset(0),
        },
        TextPosition {
            grapheme: GraphemeIndex(1),
            byte: ByteOffset(usize::MAX),
            utf16: Utf16Offset(1),
        },
    );
    assert_eq!(range.slice_str("éx"), "éx");
    assert_eq!(range.slice_str("é"), "é");
}

#[test]
fn word_navigation_normalizes_malformed_unicode_positions() {
    // "é cat": é(2B) + ' '(1B) + c(1B) + a(1B) + t(1B) = 6 bytes, 5 graphemes.
    let text = "é cat";
    let inside_scalar = TextPosition {
        grapheme: GraphemeIndex(usize::MAX),
        byte: ByteOffset(1), // byte 1 is inside é (bytes 0-1)
        utf16: Utf16Offset(usize::MAX),
    };
    let past_end = TextPosition {
        grapheme: GraphemeIndex(0),
        byte: ByteOffset(usize::MAX), // clamps to text.len()=6
        utf16: Utf16Offset(0),
    };

    // inside_scalar.prev_word: from_byte snaps byte 1 to grapheme 0 (é, byte 0) → at start → returns start.
    let previous = inside_scalar.prev_word(text);
    assert_eq!(previous.byte.0, 0);
    // next_word from inside é snaps to byte 0 (start of é), then advances past é.
    assert!(text.is_char_boundary(inside_scalar.next_word(text).byte.0));
    // past_end: prev_word clamps to end (byte 6), last word boundary before 6 is at 3 (start of "cat").
    assert_eq!(past_end.prev_word(text).byte.0, 3);
    // past_end: next_word clamps to end → is_at_end → returns end_of.
    assert_eq!(past_end.next_word(text), TextPosition::end_of(text));
}

#[test]
fn test_text_formats_and_merging() {
    let mut char_fmt = TextCharFormat {
        font_underline: true,
        underline_style: UnderlineStyle::SingleUnderline,
        foreground: Some(Color::from_rgba8(200, 50, 50, 255)),
        ..Default::default()
    };

    let override_fmt = TextCharFormat {
        font_italic: Some(true),
        font_weight: Some(FontWeight::Bold),
        ..Default::default()
    };

    char_fmt.merge(&override_fmt);
    assert!(char_fmt.font_underline);
    assert_eq!(char_fmt.font_italic, Some(true));
    assert_eq!(char_fmt.font_weight, Some(FontWeight::Bold));
    assert!(char_fmt.foreground.is_some());

    // Font resolution
    let base_font = Font::new("Segoe UI", 12.0);
    let derived_font = char_fmt.to_font(&base_font);
    assert_eq!(derived_font.family(), "Segoe UI");
    assert_eq!(derived_font.weight(), FontWeight::Bold);
    assert_eq!(derived_font.style(), FontStyle::Italic);

    // Block format
    let mut block_fmt = TextBlockFormat {
        alignment: ferrokit_gui::Alignment::H_CENTER,
        heading_level: 2,
        top_margin: 10.0,
        ..Default::default()
    };

    let merge_block = TextBlockFormat {
        left_margin: 20.0,
        ..Default::default()
    };
    block_fmt.merge(&merge_block);
    assert_eq!(block_fmt.alignment, ferrokit_gui::Alignment::H_CENTER);
    assert_eq!(block_fmt.heading_level, 2);
    assert_eq!(block_fmt.top_margin, 10.0);
    assert_eq!(block_fmt.left_margin, 20.0);
}

#[test]
fn test_text_layout_and_line_breaking() {
    let text = "Hello world! This is a multi-line wrapped text layout engine test.";
    let font = Font::new("Arial", 12.0);
    let mut layout = TextLayout::new(text, font);

    // Constrain width to 100 pixels to force wrapping
    layout.set_wrap_width(Some(100.0));
    layout.do_layout();

    assert!(layout.line_count() > 1, "Should break into multiple lines");

    let first_line = layout.line_at(0).expect("first line");
    assert!(first_line.width() > 0.0);
    assert!(first_line.height() > 0.0);
    assert_eq!(first_line.text_start(), 0);
    assert!(first_line.text_length() > 0);

    // Coordinate conversion
    let x_at_0 = first_line.cursor_to_x(0, Edge::Leading);
    let x_at_end = first_line.cursor_to_x(first_line.text_length(), Edge::Leading);
    assert!(x_at_end > x_at_0);

    let cursor_from_x =
        first_line.x_to_cursor(x_at_0 + 5.0, CursorPosition::CursorBetweenCharacters);
    assert!(cursor_from_x <= first_line.text_length());

    // Preedit area integration
    layout.set_preedit_area(5, " [COMPOSING] ");
    assert_eq!(layout.preedit_area_text(), " [COMPOSING] ");
    assert_eq!(layout.preedit_area_position(), 5);
}

#[test]
fn test_rich_text_document_model_and_cursor() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("First paragraph.\nSecond paragraph.\nThird paragraph.");
    assert_eq!(doc.block_count(), 3);
    assert_eq!(doc.block_at(0).unwrap().text(), "First paragraph.");
    assert_eq!(doc.block_at(1).unwrap().text(), "Second paragraph.");
    assert_eq!(doc.block_at(2).unwrap().text(), "Third paragraph.");

    // Cursor navigation
    let mut cursor = TextCursor::new();
    assert_eq!(cursor.position(), 0);

    // Move to end of first block
    cursor.move_position(&doc, MoveOperation::EndOfBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 16);

    // Move next block
    cursor.move_position(&doc, MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 17);

    // Selection
    cursor.move_position(&doc, MoveOperation::NextWord, MoveMode::KeepAnchor, 1);
    assert!(cursor.has_selection());
    let selected = cursor.selected_text(&doc);
    assert!(!selected.is_empty());

    // Insert text replaces selection
    cursor.insert_text(&mut doc, "REPLACED");
    assert!(!cursor.has_selection());
    assert!(doc.to_plain_text().contains("REPLACED"));
    assert!(doc.is_modified());
}

#[test]
fn test_document_html_and_markdown_serialization() {
    let mut doc = TextDocument::new();
    doc.set_html("<h1>Title</h1><p>This is <b>bold</b> and <i>italic</i> text.</p>");
    assert_eq!(doc.block_count(), 2);
    assert_eq!(doc.block_at(0).unwrap().block_format().heading_level, 1);

    let html_out = doc.to_html();
    assert!(html_out.contains("<h1>"));
    assert!(html_out.contains("<b>bold</b>"));
    assert!(html_out.contains("<i>italic</i>"));

    // Markdown test
    let mut md_doc = TextDocument::new();
    md_doc.set_markdown("## Subtitle\n**BoldText**");
    assert_eq!(md_doc.block_count(), 2);
    assert_eq!(md_doc.block_at(0).unwrap().block_format().heading_level, 2);
    let md_out = md_doc.to_markdown();
    assert!(md_out.contains("## Subtitle"));
    assert!(md_out.contains("**BoldText**"));
}

#[test]
fn test_syntax_highlighter() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("fn main() {\n    let x = 42;\n    // Comment\n}");

    let mut highlighter = SyntaxHighlighter::new();
    let kw_fmt = TextCharFormat::default().with_foreground(Color::from_rgba8(200, 0, 100, 255));
    highlighter.add_keywords(&["fn", "let"], kw_fmt);
    let comment_fmt = TextCharFormat::default().with_foreground(Color::from_rgba8(0, 128, 0, 255));
    highlighter.add_line_comment("//", comment_fmt);

    highlighter.rehighlight(&mut doc);

    // The highlight is an overlay (`QTextLayout::additionalFormats`): the document fragments keep
    // their user formats, and `effective_fragments` merges the overlay in.
    let block0 = doc.block_at(0).unwrap();
    assert!(block0.fragments().iter().all(|f| f.char_format().foreground.is_none()));
    assert!(block0
        .effective_fragments()
        .iter()
        .any(|f| f.text() == "fn" && f.char_format().foreground.is_some()));

    // Verify block 2 has comment color
    let block2 = doc.block_at(2).unwrap();
    assert!(block2
        .effective_fragments()
        .iter()
        .any(|f| f.char_format().foreground == Some(Color::from_rgba8(0, 128, 0, 255))));
}

#[test]
fn test_rehighlight_preserves_user_character_formatting() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("fn main");
    let user = TextCharFormat::default().with_foreground(Color::from_rgba8(1, 2, 3, 255));
    doc.merge_char_format(0, 7, &user);
    let before = doc.block_at(0).unwrap().fragments().to_vec();

    let mut highlighter = SyntaxHighlighter::new();
    highlighter.add_keyword(
        "fn",
        TextCharFormat::default().with_foreground(Color::from_rgba8(200, 0, 100, 255)),
    );
    highlighter.rehighlight(&mut doc);
    assert_eq!(doc.block_at(0).unwrap().fragments(), &before[..], "fragments untouched");
    let fn_color = |doc: &TextDocument| {
        doc.block_at(0)
            .unwrap()
            .effective_fragments()
            .iter()
            .find(|f| f.text() == "fn")
            .and_then(|f| f.char_format().foreground)
    };
    assert_eq!(fn_color(&doc), Some(Color::from_rgba8(200, 0, 100, 255)));

    // Dropping the rule and rehighlighting restores the user colour.
    SyntaxHighlighter::new().rehighlight(&mut doc);
    assert_eq!(doc.block_at(0).unwrap().effective_fragments(), before);
    assert_eq!(doc.block_at(0).unwrap().fragments(), &before[..]);
}

#[test]
fn test_highlight_overlay_survives_fragment_splitting() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("fn main");
    let mut highlighter = SyntaxHighlighter::new();
    let color = Color::from_rgba8(200, 0, 100, 255);
    highlighter.add_keyword("fn", TextCharFormat::default().with_foreground(color));
    highlighter.rehighlight(&mut doc);

    // A formatting-only edit splits the fragments but leaves the text, so the overlay stays.
    let user = TextCharFormat::default().with_background(Color::from_rgba8(9, 9, 9, 255));
    doc.merge_char_format(1, 4, &user);
    assert!(doc.block_at(0).unwrap().fragments().len() > 1);
    assert!(doc
        .block_at(0)
        .unwrap()
        .effective_fragments()
        .iter()
        .any(|f| f.char_format().foreground == Some(color)));
}

#[test]
fn test_exports_do_not_include_highlighter_overlay_like_qt() {
    // Qt: `QSyntaxHighlighter` only sets `QTextLayout::formats`, which `toHtml()` never reads.
    let mut doc = TextDocument::new();
    doc.set_plain_text("fn main");
    let bold = TextCharFormat::default().with_font_weight(FontWeight::Bold);
    doc.merge_char_format(0, 2, &bold);
    let user_html = doc.to_html();
    let user_markdown = doc.to_markdown();
    assert!(user_html.contains("<b>fn</b>"), "{user_html}");

    let mut highlighter = SyntaxHighlighter::new();
    highlighter.add_keyword(
        "fn",
        TextCharFormat::default().with_foreground(Color::from_rgba8(200, 0, 100, 255)),
    );
    highlighter.rehighlight(&mut doc);
    // The overlay is applied for display ...
    assert!(doc
        .block_at(0)
        .unwrap()
        .effective_fragments()
        .iter()
        .any(|f| f.char_format().foreground.is_some()));
    // ... but the exports are unchanged and keep the user's own bold.
    assert_eq!(doc.to_html(), user_html);
    assert_eq!(doc.to_markdown(), user_markdown);
    assert!(!doc.to_html().contains("c80064"));
}

#[test]
fn test_cursor_visual_line_navigation_in_wrapped_blocks() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("abc\nde");
    doc.set_text_width(1.0); // every grapheme wraps onto its own visual line
    fn step(doc: &TextDocument, from: usize, op: MoveOperation) -> usize {
        let mut c = TextCursor::at_position(from);
        c.move_position(doc, op, MoveMode::MoveAnchor, 1);
        c.position()
    }
    assert_eq!(step(&doc, 1, MoveOperation::StartOfLine), 1);
    assert_eq!(step(&doc, 1, MoveOperation::EndOfLine), 2);
    assert_eq!(step(&doc, 0, MoveOperation::Down), 1);
    assert_eq!(step(&doc, 1, MoveOperation::Down), 2);
    assert_eq!(step(&doc, 3, MoveOperation::Down), 4, "crosses into the next block's first line");
    assert_eq!(
        step(&doc, 4, MoveOperation::Up),
        2,
        "crosses back to the previous block's last line"
    );
    assert_eq!(step(&doc, 1, MoveOperation::Up), 0);

    // Without a text width every block is one line.
    doc.set_text_width(-1.0);
    assert_eq!(step(&doc, 1, MoveOperation::EndOfLine), 3);
    assert_eq!(step(&doc, 1, MoveOperation::StartOfLine), 0);
}

fn press(c: &mut TextCursor, doc: &TextDocument, op: MoveOperation) -> usize {
    c.move_position(doc, op, MoveMode::MoveAnchor, 1);
    c.position()
}

#[test]
fn test_cursor_sticky_column_without_wrapping() {
    let mut doc = TextDocument::new();
    // Blocks start at 0, 7, 10, 17.
    doc.set_plain_text("abcdef\nxy\nlonger\nabcdef");
    let mut c = TextCursor::at_position(5);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 9, "clamped into the short block");
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 15, "column 5 restored");
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 22);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 15);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 9);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 5, "back at the original column");

    // Cloned cursors carry the column along.
    press(&mut c, &doc, MoveOperation::Down);
    let mut copy = c.clone();
    assert_eq!(press(&mut copy, &doc, MoveOperation::Down), 15);

    // Any non-vertical move or edit forgets it: Down behaves like on a fresh cursor.
    for op in [
        MoveOperation::Left,
        MoveOperation::Right,
        MoveOperation::StartOfLine,
        MoveOperation::EndOfLine,
        MoveOperation::StartOfWord,
        MoveOperation::NextWord,
    ] {
        let mut c = TextCursor::at_position(5);
        press(&mut c, &doc, MoveOperation::Down);
        let at = press(&mut c, &doc, op);
        let mut fresh = TextCursor::at_position(at);
        assert_eq!(
            press(&mut c, &doc, MoveOperation::Down),
            press(&mut fresh, &doc, MoveOperation::Down),
            "{op:?}"
        );
    }
    let mut c = TextCursor::at_position(5);
    let mut doc2 = doc.clone();
    press(&mut c, &doc2, MoveOperation::Down);
    c.insert_text(&mut doc2, "z");
    assert_eq!(c.position(), 10);
    assert_eq!(press(&mut c, &doc2, MoveOperation::Down), 11 + 3, "edit reset the column");
}

#[test]
fn test_cursor_sticky_x_with_wrapping() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("abcdefgh\nab\nabcdefgh\nabcdefgh");
    doc.set_text_width(10_000.0);
    // Blocks start at 0, 9, 12, 21.
    let mut c = TextCursor::at_position(6);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 11, "short line clamps");
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 18, "x restored across blocks");
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 27, "repeated press keeps x");
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 18);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 11);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 6, "original x on the first line");

    // A failed Down at the last line / Up at the first keeps the x.
    let mut c = TextCursor::at_position(6);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 11);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 18);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 27);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 27, "last line: no move");
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 18);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 11);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 6);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 6, "first line: no move");
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 11, "x still sticky");
    let mut c = TextCursor::at_position(6);
    press(&mut c, &doc, MoveOperation::Down);
    press(&mut c, &doc, MoveOperation::Down);
    press(&mut c, &doc, MoveOperation::Down);
    press(&mut c, &doc, MoveOperation::Down);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 18, "x survived the failed Down");

    // Selecting with Shift+Down keeps the column too.
    let mut c = TextCursor::at_position(6);
    c.move_position(&doc, MoveOperation::Down, MoveMode::KeepAnchor, 2);
    assert_eq!((c.anchor(), c.position()), (6, 18));

    // Non-vertical moves and edits reset it.
    for op in [
        MoveOperation::Left,
        MoveOperation::Right,
        MoveOperation::StartOfLine,
        MoveOperation::EndOfLine,
    ] {
        let mut c = TextCursor::at_position(6);
        press(&mut c, &doc, MoveOperation::Down);
        let at = press(&mut c, &doc, op);
        let down = press(&mut c, &doc, MoveOperation::Down);
        let mut fresh = TextCursor::at_position(at);
        assert_eq!(down, press(&mut fresh, &doc, MoveOperation::Down), "{op:?}");
    }
    let mut c = TextCursor::at_position(6);
    let mut doc2 = doc.clone();
    press(&mut c, &doc2, MoveOperation::Down);
    c.insert_text(&mut doc2, "z");
    let mut fresh = TextCursor::at_position(c.position());
    assert_eq!(
        press(&mut c, &doc2, MoveOperation::Down),
        press(&mut fresh, &doc2, MoveOperation::Down)
    );
}

// Measured with QTextCursor.movePosition on "hello world\nsecond line here\nthird" with the
// selection 5..14: a count of 0 changes nothing and returns true, except Start, End, StartOfLine
// and EndOfLine, which move once whatever the count.
#[test]
fn test_cursor_move_position_zero_count_is_a_no_op_as_in_qt() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("hello world\nsecond line here\nthird");
    let selected = || {
        let mut c = TextCursor::at_position(5);
        c.set_position(14, MoveMode::KeepAnchor);
        c
    };
    for op in [
        MoveOperation::NoMove,
        MoveOperation::StartOfBlock,
        MoveOperation::EndOfBlock,
        MoveOperation::StartOfWord,
        MoveOperation::EndOfWord,
        MoveOperation::PreviousBlock,
        MoveOperation::NextBlock,
        MoveOperation::PreviousCharacter,
        MoveOperation::NextCharacter,
        MoveOperation::PreviousWord,
        MoveOperation::NextWord,
        MoveOperation::Left,
        MoveOperation::Right,
        MoveOperation::Up,
        MoveOperation::Down,
    ] {
        for mode in [MoveMode::MoveAnchor, MoveMode::KeepAnchor] {
            let mut c = selected();
            assert!(c.move_position(&doc, op, mode, 0), "{op:?} {mode:?}");
            assert_eq!((c.anchor(), c.position()), (5, 14), "{op:?} {mode:?}");
        }
    }
    for (op, to) in [
        (MoveOperation::Start, 0),
        (MoveOperation::End, 34),
        (MoveOperation::StartOfLine, 12),
        (MoveOperation::EndOfLine, 28),
    ] {
        let mut c = selected();
        assert!(c.move_position(&doc, op, MoveMode::MoveAnchor, 0), "{op:?}");
        assert_eq!((c.anchor(), c.position()), (to, to), "{op:?} moves once");
        let mut c = selected();
        assert!(c.move_position(&doc, op, MoveMode::KeepAnchor, 0), "{op:?}");
        assert_eq!((c.anchor(), c.position()), (5, to), "{op:?} keeps the anchor");
    }

    // A zero-count move does not reset the column that Up and Down keep.
    let mut doc = TextDocument::new();
    doc.set_plain_text("abcdef\nxy\nlonger\nabcdef");
    let mut c = TextCursor::at_position(5);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 9, "clamped to the short line");
    c.move_position(&doc, MoveOperation::Left, MoveMode::MoveAnchor, 0);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 5, "the column of 5 survived");
}

#[test]
fn test_qt_canonical_text_aliases() {
    let _l: QTextLayout = TextLayout::new("text", Font::new("Arial", 12.0));
    let _line: QTextLine = TextLine::default();
    let _doc: QTextDocument = TextDocument::new();
    let _cur: QTextCursor = TextCursor::new();
    let _b: QTextBlock = TextBlock::new(0, 0);
    let _f: QTextFragment = TextFragment::plain("txt");
    let _cf: QTextCharFormat = TextCharFormat::default();
    let _bf: QTextBlockFormat = TextBlockFormat::default();
    let _lf: QTextListFormat = TextListFormat::default();
    let _ff: QTextFrameFormat = TextFrameFormat::default();
    let _df: QTextDocumentFragment = TextDocumentFragment::new();
    let _hl: QSyntaxHighlighter = SyntaxHighlighter::new();
}

/// `Up` and `Down` keep the x coordinate in a document without a text width too, not the grapheme
/// column. Positions are `QTextCursor::movePosition` of PySide6 6.11.2 (document margin 0) with the
/// synthetic oracle font (16 px: "b" is 7.625 px, a space 4 px, "a" 7.5 px), so the same column is
/// a different x on the lines of `"bbbbbb\n      ab\nbbbbbbbb"`.
#[test]
fn test_cursor_up_down_keep_x_in_an_unwrapped_document_as_in_qt() {
    use ferrokit_gui::text::font::SharedFontData;
    ferrokit_gui::text::font_database::set_application_device_pixel_ratio(2.0);
    let font = Font::new("FerrokitOracleText", 16.0).with_font_data(SharedFontData::from_vec(
        include_bytes!("data/oracle_text.ttf").to_vec(),
    ));
    // (text, start, moves: true is Down, positions after each move)
    let cases: [(&str, usize, &[bool], &[usize]); 7] = [
        ("bbbbbb\n      ab\nbbbbbbbb", 4, &[true, true], &[14, 20]),
        ("bbbbbb\n      ab\nbbbbbbbb", 4, &[true, true, false, false], &[14, 20, 14, 4]),
        ("bbbbbb\n  ab\nbbbbbbbb", 5, &[true, true], &[11, 17]),
        ("      ab\nbbbbbb", 5, &[true], &[12]),
        ("bbbbbb\n      ab", 6, &[true], &[15]),
        ("bbbbbb\n      ab\nbb", 3, &[true, true, false, false], &[13, 18, 13, 3]),
        ("ab\n\nbbbbbb", 2, &[true, true], &[3, 6]),
    ];
    for (text, start, moves, expected) in cases {
        let mut doc = TextDocument::from_plain_text(text);
        doc.set_default_font(font.clone());
        let mut cursor = TextCursor::at_position(start);
        let mut got = Vec::new();
        for &down in moves {
            let op = if down { MoveOperation::Down } else { MoveOperation::Up };
            cursor.move_position(&doc, op, MoveMode::MoveAnchor, 1);
            got.push(cursor.position());
        }
        assert_eq!(got, expected, "{text:?} from {start}");
    }
}

