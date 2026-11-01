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

/// Foreground colour the highlighter overlay (`QTextLayout::additionalFormats`) gives to the
/// graphemes `start..start + len` of block `block`.
fn overlay_foreground(doc: &TextDocument, block: usize, start: usize, len: usize) -> Option<Color> {
    doc.block_at(block)
        .unwrap()
        .additional_formats()
        .iter()
        .find(|r| r.start <= start && start + len <= r.start + r.length)
        .and_then(|r| r.format.foreground)
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
    // their user formats, and the overlay carries the highlight.
    let block0 = doc.block_at(0).unwrap();
    assert!(
        block0
            .fragments()
            .iter()
            .all(|f| f.char_format().foreground.is_none())
    );
    assert!(overlay_foreground(&doc, 0, 0, 2).is_some(), "fn");

    // Verify block 2 has comment color
    assert_eq!(
        overlay_foreground(&doc, 2, 4, 10),
        Some(Color::from_rgba8(0, 128, 0, 255))
    );
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
    assert_eq!(
        doc.block_at(0).unwrap().fragments(),
        &before[..],
        "fragments untouched"
    );
    let fn_color = |doc: &TextDocument| overlay_foreground(doc, 0, 0, 2);
    assert_eq!(fn_color(&doc), Some(Color::from_rgba8(200, 0, 100, 255)));

    // Dropping the rule and rehighlighting restores the user colour.
    SyntaxHighlighter::new().rehighlight(&mut doc);
    assert_eq!(fn_color(&doc), None);
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
    assert_eq!(overlay_foreground(&doc, 0, 0, 2), Some(color));
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
    assert!(overlay_foreground(&doc, 0, 0, 2).is_some());
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
    assert_eq!(
        step(&doc, 3, MoveOperation::Down),
        4,
        "crosses into the next block's first line"
    );
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
fn test_cursor_sticky_x_with_wrapping() {
    let mut doc = TextDocument::new();
    doc.set_plain_text("abcdefgh\nab\nabcdefgh\nabcdefgh");
    doc.set_text_width(10_000.0);
    // Blocks start at 0, 9, 12, 21.
    let mut c = TextCursor::at_position(6);
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Down),
        11,
        "short line clamps"
    );
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Down),
        18,
        "x restored across blocks"
    );
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Down),
        27,
        "repeated press keeps x"
    );
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 18);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 11);
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Up),
        6,
        "original x on the first line"
    );

    // A failed Down at the last line / Up at the first keeps the x.
    let mut c = TextCursor::at_position(6);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 11);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 18);
    assert_eq!(press(&mut c, &doc, MoveOperation::Down), 27);
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Down),
        27,
        "last line: no move"
    );
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 18);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 11);
    assert_eq!(press(&mut c, &doc, MoveOperation::Up), 6);
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Up),
        6,
        "first line: no move"
    );
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Down),
        11,
        "x still sticky"
    );
    let mut c = TextCursor::at_position(6);
    press(&mut c, &doc, MoveOperation::Down);
    press(&mut c, &doc, MoveOperation::Down);
    press(&mut c, &doc, MoveOperation::Down);
    press(&mut c, &doc, MoveOperation::Down);
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Up),
        18,
        "x survived the failed Down"
    );

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
        assert_eq!(
            (c.anchor(), c.position()),
            (5, to),
            "{op:?} keeps the anchor"
        );
    }
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

/// The document of the oracle font (16 px: "b" is 7.625 px, a space 4 px, "a" 7.5 px), which the
/// vertical-move tests use because the same column is a different x on different lines.
fn oracle_document(text: &str) -> TextDocument {
    use ferrokit_gui::text::font::SharedFontData;
    ferrokit_gui::text::font_database::set_application_device_pixel_ratio(2.0);
    let mut doc = TextDocument::from_plain_text(text);
    doc.set_default_font(Font::new("FerrokitOracleText", 16.0).with_font_data(
        SharedFontData::from_vec(include_bytes!("data/oracle_text.ttf").to_vec()),
    ));
    doc
}

/// U+2029 has no width in a `QTextLayout` and does not end its line, but a line may break
/// after it. Lines are `(textStart, textLength, naturalTextWidth, cursorToX of every
/// position)` measured with PySide6 6.11.2 and the oracle font.
#[test]
fn test_a_paragraph_separator_has_no_width_as_in_qt() {
    type Line = (usize, usize, f32, &'static [f32]);
    let cases: [(&str, Option<f32>, &[Line]); 6] = [
        (
            "ab\u{2029}cd",
            None,
            &[(0, 5, 30.75, &[0.0, 7.5, 15.125, 15.125, 22.875, 30.75])],
        ),
        (
            "ab\u{2029}",
            None,
            &[(0, 3, 15.125, &[0.0, 7.5, 15.125, 15.125])],
        ),
        ("\u{2029}", None, &[(0, 1, 0.0, &[0.0, 0.0])]),
        (
            "a\u{2029}\u{2029}b",
            None,
            &[(0, 4, 15.125, &[0.0, 7.5, 7.5, 7.5, 15.125])],
        ),
        (
            "aaa\u{2029}bbb",
            Some(40.0),
            &[
                (0, 4, 22.5, &[0.0, 7.5, 15.0, 22.5, 22.5]),
                (4, 3, 22.875, &[0.0, 7.625, 15.25, 22.875]),
            ],
        ),
        (
            "aaa\u{2029}bbb",
            Some(50.0),
            &[(
                0,
                7,
                45.375,
                &[0.0, 7.5, 15.0, 22.5, 22.5, 30.125, 37.75, 45.375],
            )],
        ),
    ];
    for (text, width, expected) in cases {
        let font = oracle_document("").default_font().clone();
        let mut layout = TextLayout::new(text, font);
        if let Some(w) = width {
            layout.set_wrap_mode(WrapMode::WrapAtWordBoundaryOrAnywhere);
            layout.set_wrap_width(Some(w));
        }
        layout.do_layout();
        let got: Vec<(usize, usize, f32, Vec<f32>)> = (0..layout.line_count())
            .map(|i| {
                let l = layout.line_at(i).unwrap();
                let xs = (l.text_start()..=l.text_start() + l.text_length())
                    .map(|p| l.cursor_to_x(p, Edge::Leading))
                    .collect();
                (l.text_start(), l.text_length(), l.natural_text_width(), xs)
            })
            .collect();
        let want: Vec<(usize, usize, f32, Vec<f32>)> = expected
            .iter()
            .map(|&(s, n, w, xs)| (s, n, w, xs.to_vec()))
            .collect();
        assert_eq!(got, want, "{text:?} at {width:?}");
    }
}

/// The lines of `text` laid out in the oracle font wrapped in `mode` at `width`, as
/// `(textStart, textLength, naturalTextWidth)`.
fn oracle_lines(text: &str, mode: WrapMode, width: f32) -> Vec<(usize, usize, f32)> {
    let font = oracle_document("").default_font().clone();
    let mut layout = TextLayout::new(text, font);
    layout.set_wrap_mode(mode);
    layout.set_wrap_width(Some(width));
    layout.do_layout();
    (0..layout.line_count())
        .map(|i| {
            let l = layout.line_at(i).unwrap();
            (l.text_start(), l.text_length(), l.natural_text_width())
        })
        .collect()
}

/// At a width narrower than a glyph the paragraph separator stays on the line before it and a
/// line of separators keeps the next word, in every wrap mode: Qt does not test it against the
/// width and does not count it as a word. `QTextLayout` of PySide6 6.11.2, oracle font.
#[test]
fn test_a_paragraph_separator_is_not_wrapped_off_as_in_qt() {
    use WrapMode::{WordWrap, WrapAnywhere, WrapAtWordBoundaryOrAnywhere};
    type Case = (&'static str, WrapMode, f32, &'static [(usize, usize, f32)]);
    let cases: [Case; 8] = [
        ("a\u{2029}", WrapAnywhere, 3.0, &[(0, 2, 7.5)]),
        (
            "a\u{2029}",
            WrapAtWordBoundaryOrAnywhere,
            0.0,
            &[(0, 2, 7.5)],
        ),
        ("\u{2029}\u{2029}a", WordWrap, 0.0, &[(0, 3, 7.5)]),
        ("\u{2029}a", WrapAnywhere, 5.0, &[(0, 2, 7.5)]),
        (
            "a\u{2029}b",
            WrapAtWordBoundaryOrAnywhere,
            3.0,
            &[(0, 2, 7.5), (2, 1, 7.625)],
        ),
        (
            "aa\u{2029}\u{2029}bb",
            WordWrap,
            5.0,
            &[(0, 4, 15.0), (4, 2, 15.25)],
        ),
        (
            "aa\u{2029}\u{2029}bb",
            WrapAnywhere,
            3.0,
            &[(0, 1, 7.5), (1, 3, 7.5), (4, 1, 7.625), (5, 1, 7.625)],
        ),
        (
            "aa\u{2029}\u{2029}bb",
            WrapAtWordBoundaryOrAnywhere,
            10.0,
            &[(0, 1, 7.5), (1, 3, 7.5), (4, 1, 7.625), (5, 1, 7.625)],
        ),
    ];
    for (text, mode, width, expected) in cases {
        assert_eq!(
            oracle_lines(text, mode, width),
            expected,
            "{text:?} {mode:?} at {width}"
        );
    }
}

/// A line with right to left text that a line separator ends: the separator has no advance, so
/// the bidi run holding it must not end at x 0. `cursorToX` of every position and `xToCursor` at
/// some x of the first line, `QTextLayout` of PySide6 6.11.2, oracle font.
#[test]
fn test_right_to_left_text_before_a_line_separator_as_in_qt() {
    type Case = (&'static str, &'static [f32], &'static [usize]);
    let probes = [
        -1.0, 0.0, 3.0, 7.0, 11.0, 15.0, 20.0, 26.0, 31.0, 40.0, 1000.0,
    ];
    let cases: [Case; 3] = [
        (
            "\u{5d0}\u{5d1}\u{2028}",
            &[17.25, 8.5, 0.0, 0.0],
            &[3, 3, 2, 1, 1, 0, 0, 0, 0, 0, 0],
        ),
        (
            "\u{5d0}\u{5d1} \u{5d2}\u{2028}cd",
            &[27.5, 18.75, 10.25, 6.25, 0.0, 0.0],
            &[5, 5, 4, 3, 2, 1, 1, 0, 0, 0, 0],
        ),
        (
            "ab \u{5d0}\u{5d1}\u{2028}cd",
            &[0.0, 7.5, 15.125, 19.125, 27.625, 36.375, 36.375],
            &[0, 0, 0, 1, 1, 2, 5, 4, 4, 5, 5],
        ),
    ];
    for (text, xs, hits) in cases {
        let font = oracle_document("").default_font().clone();
        let mut layout = TextLayout::new(text, font);
        layout.do_layout();
        let line = layout.line_at(0).unwrap();
        let got: Vec<f32> = (0..=line.text_length())
            .map(|i| line.cursor_to_x(i, Edge::Leading))
            .collect();
        assert_eq!(got, xs, "{text:?} cursorToX");
        let got: Vec<usize> = probes
            .iter()
            .map(|&x| line.x_to_cursor(x, CursorPosition::CursorBetweenCharacters))
            .collect();
        assert_eq!(got, hits, "{text:?} xToCursor");
    }
}

/// `Up` and `Down` keep the x coordinate in a document without a text width too, not the grapheme
/// column. Positions are `QTextCursor::movePosition` of PySide6 6.11.2 (document margin 0), so the
/// same column is a different x on the lines of `"bbbbbb\n      ab\nbbbbbbbb"`.
#[test]
fn test_cursor_up_down_keep_x_in_an_unwrapped_document_as_in_qt() {
    // (text, start, moves: true is Down, positions after each move)
    let cases: [(&str, usize, &[bool], &[usize]); 10] = [
        ("bbbbbb\n      ab\nbbbbbbbb", 4, &[true, true], &[14, 20]),
        (
            "bbbbbb\n      ab\nbbbbbbbb",
            4,
            &[true, true, false, false],
            &[14, 20, 14, 4],
        ),
        ("bbbbbb\n  ab\nbbbbbbbb", 5, &[true, true], &[11, 17]),
        ("      ab\nbbbbbb", 5, &[true], &[12]),
        ("bbbbbb\n      ab", 6, &[true], &[15]),
        (
            "bbbbbb\n      ab\nbb",
            3,
            &[true, true, false, false],
            &[13, 18, 13, 3],
        ),
        ("ab\n\nbbbbbb", 2, &[true, true], &[3, 6]),
        // the first line has nothing above it and the last nothing below: the cursor stays
        ("abc\ndef", 2, &[false], &[2]),
        ("abc\ndef", 6, &[true], &[6]),
        ("abc\ndef", 2, &[false, true], &[2, 6]),
    ];
    for (text, start, moves, expected) in cases {
        let doc = oracle_document(text);
        let mut cursor = TextCursor::at_position(start);
        let mut got = Vec::new();
        for &down in moves {
            got.push(press(
                &mut cursor,
                &doc,
                if down {
                    MoveOperation::Down
                } else {
                    MoveOperation::Up
                },
            ));
        }
        assert_eq!(got, expected, "{text:?} from {start}");
    }
}

/// The x that `Up` and `Down` keep goes with a cloned cursor, survives a zero-count move and is
/// forgotten by every other move (`QTextCursorPrivate::x`).
#[test]
fn test_cursor_sticky_x_without_wrapping() {
    let doc = oracle_document("bbbbbb\n  ab\nbbbbbbbb\nbbbbbb");
    let mut c = TextCursor::at_position(5);
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Down),
        11,
        "clamped into the short line"
    );
    // A zero-count move, vertical or not, does not reset it.
    for op in [MoveOperation::Left, MoveOperation::Up, MoveOperation::Down] {
        for mode in [MoveMode::MoveAnchor, MoveMode::KeepAnchor] {
            let mut c = c.clone();
            c.move_position(&doc, op, mode, 0);
            assert_eq!(
                press(&mut c, &doc, MoveOperation::Up),
                5,
                "back at the original x after {op:?} x0"
            );
        }
    }
    assert_eq!(
        press(&mut c, &doc, MoveOperation::Up),
        5,
        "back at the original x"
    );
    press(&mut c, &doc, MoveOperation::Down);

    // Cloned cursors carry it along.
    let mut copy = c.clone();
    assert_eq!(press(&mut copy, &doc, MoveOperation::Down), 17);

    // Any non-vertical move forgets it: Down behaves like on a fresh cursor.
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

    // An edit forgets it too.
    let mut doc2 = doc.clone();
    let mut c = TextCursor::at_position(5);
    press(&mut c, &doc2, MoveOperation::Down);
    c.insert_text(&mut doc2, "z");
    let mut fresh = TextCursor::at_position(c.position());
    assert_eq!(
        press(&mut c, &doc2, MoveOperation::Down),
        press(&mut fresh, &doc2, MoveOperation::Down)
    );
}

/// `Up`/`Down` through narrow wrapped blocks with an empty block between them keep the x of the
/// first move. Positions are `QTextCursor::movePosition` of PySide6 6.11.2 with the oracle font,
/// document margin 0 and a text width of 60 px (the blocks wrap as `"bbbbbb "|"bb"`, `""`,
/// `"bb "|"bbbbbbb"|"b"` and `"bb "|"bbbbbb"`, `""`, `"bbbbbb "|"bb"`).
#[test]
fn test_cursor_sticky_x_across_wrapped_lines_and_an_empty_block_as_in_qt() {
    let cases: [(&str, usize, &str, &[usize]); 4] = [
        (
            "bbbbbb bb\n\nbb bbbbbbbb",
            5,
            "DDDDUUUU",
            &[9, 10, 13, 19, 13, 10, 9, 5],
        ),
        (
            "bbbbbb bb\n\nbb bbbbbbbb",
            8,
            "DDDUUU",
            &[10, 12, 15, 12, 10, 8],
        ),
        (
            "bb bbbbbb\n\nbbbbbb bb",
            6,
            "DDDUUU",
            &[10, 14, 20, 14, 10, 6],
        ),
        (
            "bb bbbbbb\n\nbbbbbb bb",
            8,
            "DDDDUUUU",
            &[10, 16, 20, 20, 16, 10, 8, 2],
        ),
    ];
    for (text, start, moves, expected) in cases {
        let mut doc = oracle_document(text);
        doc.set_text_width(60.0);
        let mut cursor = TextCursor::at_position(start);
        let got: Vec<usize> = moves
            .chars()
            .map(|m| {
                let op = if m == 'D' {
                    MoveOperation::Down
                } else {
                    MoveOperation::Up
                };
                press(&mut cursor, &doc, op)
            })
            .collect();
        assert_eq!(got, expected, "{text:?} from {start} {moves}");
    }
}

/// `set_position` forgets the saved x even when it puts the cursor where it already is
/// (`QTextCursor::setPosition` resets `QTextCursorPrivate::x`). PySide6 6.11.2, oracle font: from
/// 5, `Down` gives 11; then `setPosition(11)` and `Down` gives 15 (17 without it), `Up` gives 3
/// (5 without it), wrapped or not.
#[test]
fn test_cursor_set_position_to_the_same_spot_forgets_the_saved_x_as_in_qt() {
    for width in [-1.0, 1000.0] {
        let mut doc = oracle_document("bbbbbb\n  ab\nbbbbbbbb");
        doc.set_text_width(width);
        for (op, kept, forgotten) in [(MoveOperation::Down, 17, 15), (MoveOperation::Up, 5, 3)] {
            let mut c = TextCursor::at_position(5);
            assert_eq!(press(&mut c, &doc, MoveOperation::Down), 11);
            let mut same = c.clone();
            assert_eq!(press(&mut c, &doc, op), kept, "{op:?} at {width}");
            same.set_position(11, MoveMode::MoveAnchor);
            assert_eq!(press(&mut same, &doc, op), forgotten, "{op:?} at {width}");
        }
    }
}
