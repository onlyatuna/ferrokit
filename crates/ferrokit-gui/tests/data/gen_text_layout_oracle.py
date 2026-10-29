#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates oracle_text.ttf and text_layout_oracle.txt for tests/test_text_layout_oracle.rs.

The fixture is what PySide6's real QTextLayout / QTextLine measure; the file header records the
version, platform and font engine. Run it on Windows (the DirectWrite engine the Rust side
reproduces) with fontTools installed:

    python gen_text_layout_oracle.py > text_layout_oracle.txt

`oracle_text.ttf` (written next to this script) is a synthetic font built here, so that the cases
that exercise shaping (kerning, a ligature, ligature carets, combining marks, right-to-left
letters, ink hanging over the advance) give the same numbers on every platform and do not depend on an installed font's
version. Its units per em is 1024 and the cases use 16 px, so every design value is an exact
multiple of 1/64 px: DirectWrite's truncation to 26.6 does not change any advance and the
expected numbers are the same under every font backend. The cases that use the installed Arial
(with Segoe UI Emoji and Arial's Hebrew) are Windows-only in the Rust test.

Line formats (strings are UTF-8 in hex; offsets are UTF-16 code units, as Qt reports them; x
values are Qt's doubles, shortest round-trip decimal):
    font ID file|family PX NAME      font ID, size in pixels; `file` fonts are oracle_text.ttf
    cursor CASE FONT HEXTEXT N OFF:X:XTRAILING ...
                                     cursorToX(OFF) of every grapheme boundary reached by
                                     nextCursorPosition from 0, on one unwrapped line;
                                     then N is the count and the pairs follow
    wrap CASE FONT MODE WIDTH HEXTEXT N START:LEN:NATURAL:ENDX ...
                                     lines of the text wrapped at WIDTH (mode: nowrap | word | anywhere
                                     | word-or-anywhere, QTextOption's four wrap modes):
                                     textStart, textLength, naturalTextWidth() (trailing spaces
                                     excluded) and cursorToX(textStart + textLength) of each
    align CASE FONT ALIGN WIDTH HEXTEXT N START:LEN:X0:XEND ...
                                     lines wrapped (as `word-or-anywhere`) at WIDTH and aligned
                                     (left | right | center): textStart, textLength,
                                     cursorToX(textStart) and cursorToX(textStart + textLength)
"""
import io
import os
import sys

os.environ["QT_SCALE_FACTOR"] = "2"  # any ratio but 1 selects Qt's DirectWrite engine on Windows

import PySide6
from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from PySide6.QtCore import QByteArray, Qt
from PySide6.QtGui import (
    QFont,
    QFontDatabase,
    QFontInfo,
    QGuiApplication,
    QTextLayout,
    QTextLine,
    QTextOption,
)

HERE = os.path.dirname(os.path.abspath(__file__))
UPEM = 1024
PX = 16

# (codepoint, glyph name, advance in font units); the glyph is a box of that width.
GLYPHS = (
    [(0x20, "space", 256), (0x2D, "hyphen", 320), (0x2C, "comma", 256), (0x2E, "period", 256),
        (0x2F, "slash", 288)]
    # the other ASCII punctuation, for the line breaking cases: one width, each its own glyph
    + [(c, "punct%02x" % c, 384) for c in
       list(range(0x21, 0x2C)) + list(range(0x3A, 0x41)) + list(range(0x5B, 0x61))
       + list(range(0x7B, 0x7F))]
    + [(0x30 + i, "digit%d" % i, 512) for i in range(10)]
    # Capitals: wide, with distinct widths so that a wrong glyph shows.
    + [(0x41 + i, chr(0x41 + i), 640 + 16 * (i % 5)) for i in range(26)]
    + [(0x61 + i, chr(0x61 + i), 480 + 8 * (i % 7)) for i in range(26)]
    # Hebrew alef .. dalet and a final mem: right-to-left letters.
    + [(0x5D0, "alef", 560), (0x5D1, "bet", 544), (0x5D2, "gimel", 400), (0x5D3, "dalet", 528),
       (0x5DD, "memfinal", 576)]
    # Combining acute and diaeresis: zero advance, attached by mark-to-base.
    + [(0x301, "acutecomb", 0), (0x308, "dieresiscomb", 0)]
)

# Glyph names that exist only through GSUB.
LIGATURES = [("f_i", 700), ("f_l", 760), ("f_f_i", 1000)]

# Glyphs whose ink hangs over the advance, a negative right bearing (Qt adds it to the width of a
# line that ends in the glyph, `QTextLine::layout_helper`): (codepoint, glyph name, advance, font
# units the box reaches past the advance). At 16 px a unit is 1/64 px, so the bearings are
# 1.5 px, 0.375 px and 1/64 px; `thorn` ends exactly at its advance (no bearing) and `tav` is a
# Hebrew letter, whose bearing belongs to a right to left run. `ligature` hangs by 0.625 px.
OVERHANG = [(0xE6, "ae", 640, 96), (0xF8, "oslash", 576, 24), (0xDF, "germandbls", 608, 1),
            (0xFE, "thorn", 592, 0), (0x5EA, "tav", 560, 64)]
OVERHANG_LIGATURE = ("oslash_oslash", 1152, 40)

FEATURES = """
languagesystem DFLT dflt;
languagesystem latn dflt;
languagesystem hebr dflt;

@BASE = [A B C D E F G H I J K L M N O P Q R S T U V W X Y Z a e o u];
@MARK = [acutecomb dieresiscomb];

table GDEF {
    GlyphClassDef @BASE, [f_i f_l f_f_i oslash_oslash], @MARK, ;
    LigatureCaretByPos f_l 200;
} GDEF;

feature kern {
    pos A V -128;
    pos V A -128;
    pos T o -96;
    pos T y -64;
    pos W a -80;
    pos A T -112;
    pos space A -48;
} kern;

feature liga {
    sub f f i by f_f_i;
    sub f i by f_i;
    sub f l by f_l;
    sub oslash oslash by oslash_oslash;
} liga;

markClass [acutecomb dieresiscomb] <anchor 0 0> @TOP;

feature mark {
    pos base [A E a e o u] <anchor 300 700> mark @TOP;
} mark;
"""


def build_font():
    hang = {name: over for _, name, _, over in OVERHANG}
    hang[OVERHANG_LIGATURE[0]] = OVERHANG_LIGATURE[2]
    order = ([".notdef"] + [g[1] for g in GLYPHS] + [g[0] for g in LIGATURES]
             + [g[1] for g in OVERHANG] + [OVERHANG_LIGATURE[0]])
    cmap = {cp: name for cp, name, _ in GLYPHS}
    cmap.update({cp: name for cp, name, _, _ in OVERHANG})
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap(cmap)
    adv = {".notdef": 600}
    adv.update({name: a for _, name, a in GLYPHS})
    adv.update({name: a for name, a in LIGATURES})
    adv.update({name: a for _, name, a, _ in OVERHANG})
    adv[OVERHANG_LIGATURE[0]] = OVERHANG_LIGATURE[1]
    glyphs = {}
    for name in order:
        pen = TTGlyphPen(None)
        w = adv[name]
        if w:
            pen.moveTo((40, 0))
            pen.lineTo((40, 700))
            right = w + hang[name] if name in hang else w - 40
            pen.lineTo((right, 700))
            pen.lineTo((right, 0))
            pen.closePath()
        glyphs[name] = pen.glyph()
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({n: (adv[n], 40 if adv[n] else 0) for n in order})
    fb.setupHorizontalHeader(ascent=800, descent=-224)
    fb.setupNameTable({"familyName": "FerrokitOracleText", "styleName": "Regular"})
    fb.setupOS2(sTypoAscender=800, sTypoDescender=-224, usWinAscent=800, usWinDescent=224,
                sTypoLineGap=0)
    fb.setupPost()
    fb.font["head"].created = fb.font["head"].modified = 3_600_000_000  # fixed: reproducible
    addOpenTypeFeaturesFromString(fb.font, FEATURES)
    buf = io.BytesIO()
    fb.save(buf)
    return buf.getvalue()


# (name, font id, text): single-line cursor positions.
CURSOR_CASES = [
    # kerned pairs of the synthetic font
    ("syn_AVAVAV", "syn", "AVAVAV"),
    ("syn_To", "syn", "To"),
    ("syn_Ty", "syn", "Ty"),
    ("syn_kern_sentence", "syn", "AT Wa To Ty VA"),
    ("syn_kern_after_space", "syn", "x Ab"),
    # a carriage return is a character of no width, alone or before a line feed
    # tabs: the default stop distance, a distance, a tab on a stop, and distances that are not
    # whole pixels (Qt keeps the distance in 1/64 px)
    ("syn_tab", "syn", "a\tb"),
    ("syn_tab_leading", "syn", "\tb"),
    ("syn_tab_two", "syn", "a\t\tb"),
    ("syn_tab_long", "syn", "aaaaaaaaaaaa\tb"),
    ("syn_tab_stop_20", "syn", "aaa\tb"),
    ("syn_tab_on_stop", "syn", "aa\tb"),
    ("syn_tab_fraction", "syn", "aa\tb"),
    ("syn_tab_small_gap", "syn", "aa\tb"),
    ("syn_tab_crlf", "syn", "a\t\r\nb"),
    ("arial_tab", "arial", "ab\tcd"),
    ("syn_carriage_return", "syn", "a\rb"),
    ("syn_crlf", "syn", "a\r\nb"),
    ("syn_carriage_return_end", "syn", "ab\r"),
    ("arial_carriage_return", "arial", "ab\rcd"),
    ("arial_crlf", "arial", "ab\r\ncd"),
    # ligatures: caret positions inside them (f_l has a GDEF caret, the others divide evenly)
    ("syn_liga_fi", "syn", "fi"),
    ("syn_liga_ffi", "syn", "ffi"),
    ("syn_liga_fl_caret", "syn", "afla"),
    ("syn_liga_run", "syn", "off file ffi flat"),
    # combining marks: one grapheme each, zero advance
    ("syn_combining_acute", "syn", "a\u0301e\u0301b"),
    ("syn_combining_stack", "syn", "A\u0301\u0308E"),
    ("syn_combining_lone", "syn", "x\u0301"),
    # right-to-left letters, and mixed direction
    ("syn_rtl_hebrew", "syn", "\u05d0\u05d1\u05d2\u05d3"),
    ("syn_rtl_mixed", "syn", "abc \u05d0\u05d1\u05d2 def"),
    ("syn_rtl_final", "syn", "A\u05dd\u05d0B"),
    ("syn_rtl_numbers", "syn", "\u05d0\u05d1 12 \u05d2\u05d3"),
    ("syn_rtl_nested", "syn", "a \u05d0\u05d1 b1 \u05d2 c"),
    ("syn_rtl_trailing_space", "syn", "\u05d0\u05d1 "),
    ("syn_rtl_then_ltr", "syn", "\u05d0\u05d1 abc"),
    ("syn_rtl_punct", "syn", "abc \u05d0\u05d1, \u05d2. def"),
    ("syn_rtl_two_runs", "syn", "ab \u05d0\u05d1 cd \u05d2\u05d3 ef"),
    # ink hanging over the advance of the last glyph (the hit tests reach into it)
    ("syn_bearing", "syn", "ab \u00e6"),
    ("syn_bearing_rtl", "syn", "ab \u05d0\u05ea"),
    # real Arial (Windows only in the Rust test)
    ("arial_AVAVAV", "arial", "AVAVAV"),
    ("arial_To", "arial", "To"),
    ("arial_Ty", "arial", "Ty"),
    ("arial_kern_sentence", "arial", "AVAT To Ty Wa"),
    ("arial_combining", "arial", "e\u0301a\u0308o\u0302n"),
    ("arial_fi", "arial", "fi fl ffi"),
    ("arial_emoji_zwj", "arial", "a\U0001F468\u200D\U0001F469\u200D\U0001F467b"),
    ("arial_emoji_flag", "arial", "a\U0001F1EF\U0001F1F5b"),
    ("arial_emoji_skin_tone", "arial", "a\U0001F44D\U0001F3FDb"),
    ("arial_emoji_keycap", "arial", "a1\uFE0F\u20E3b"),
    ("arial_emoji_keycap_hash", "arial", "a#\uFE0F\u20E3*\uFE0F\u20E3b"),
    ("arial_emoji_vs16_heart", "arial", "a\u2764\uFE0Fb"),
    ("arial_emoji_vs16_copyright", "arial", "a\u00A9\uFE0Fb\u00A9c"),
    ("arial_emoji_rainbow_flag", "arial", "a\U0001F3F3\uFE0F\u200D\U0001F308b"),
    ("arial_emoji_zwj_skin_tone", "arial", "a\U0001F468\U0001F3FD\u200D\U0001F469\U0001F3FDb"),
    ("arial_emoji_couple_heart", "arial", "a\U0001F469\u200D\u2764\uFE0F\u200D\U0001F468b"),
    ("arial_emoji_run", "arial", "ab\U0001F600\U0001F680\U0001F44D\U0001F3FEcd"),
    ("arial_hebrew", "arial", "\u05e9\u05dc\u05d5\u05dd"),
    ("arial_arabic", "arial", "\u0645\u0631\u062d\u0628\u0627"),
    ("arial_arabic_mixed", "arial", "abc \u0645\u0631\u062d\u0628\u0627 def"),
    ("arial_arabic_lam_alef", "arial", "a\u0644\u0627b"),
    ("arial_hebrew_emoji", "arial", "\u05e9\u05dc\u05d5\u05dd \U0001F600 \u05e2\u05d5\u05dc\u05dd"),
    ("arial_hebrew_numbers", "arial", "\u05e9\u05dc 123 \u05d5\u05dd"),
    ("arial_mixed_rtl", "arial", "abc \u05e9\u05dc\u05d5\u05dd def"),
    ("arial_bearing", "arial", "def"),
]

# The same texts in every wrap mode.
MODE_CASES = [
    ("syn_mode_words", "aaaa bbbb cccc dddd", [30, 40, 70]),
    ("syn_mode_long", "aa bbbbbbbbbbbbbbbbbbbb cc  dd", [40, 80]),
    ("syn_mode_spaces", "aaaa bbbb  ", [30, 36, 100]),
    ("syn_mode_hyphen", "ab-cd ef-gh", [30, 50]),
    ("syn_mode_leading", "  aaaa bbbb", [30, 100]),
    # the bearing of an ae (1.5 px) decides whether the last of them fits
    ("syn_mode_bearing", "\u00e6\u00e6\u00e6\u00e6", [20, 21.25, 21.5, 31.5, 41.5]),
    ("syn_mode_bearing_words", "aa \u00e6\u00e6 bb \u00e6 \u00f8\u00f8", [20, 22, 39, 40.5, 59, 60.5]),
]
PANGRAM = "The quick brown fox jumps over the lazy dog"

# (name, font id, mode, text, widths): wrapped lines.
WRAP_CASES = [
    ("syn_words", "syn", "word-or-anywhere", "AT Wa To Ty VA aaa bbb ccc", [60, 100, 140, 200, 400]),
    ("syn_hyphen", "syn", "word-or-anywhere", "ab-cd ef-gh ij-kl", [50, 80, 120]),
    ("syn_trailing_space", "syn", "word-or-anywhere", "aaaa bbbb cccc", [60, 62, 64, 70, 80, 100]),
    ("syn_long_word", "syn", "word-or-anywhere", "aa bbbbbbbbbbbbbbbbbbbb cc", [40, 80, 120]),
    ("syn_long_word_word_mode", "syn", "word", "aa bbbbbbbbbbbbbbbbbbbb cc", [40, 80, 120]),
    # a space that does not fit hangs past the width in Qt
    ("syn_hanging_space", "syn", "word-or-anywhere", "aaaa bbbb cccc", [30, 31, 33, 34, 35]),
    ("syn_hanging_spaces", "syn", "word-or-anywhere", "aaaa   bbbb", [30, 32, 38, 60]),
    ("syn_hyphen_hang", "syn", "word-or-anywhere", "ab-cd ef-gh ij-kl", [76, 77, 80]),
    # the spaces at the end of the text stay in the width of the last line
    ("syn_final_spaces", "syn", "word-or-anywhere", "aaaa bbbb  ", [30, 100]),
    ("syn_leading_spaces", "syn", "word-or-anywhere", "  aaaa bbbb", [30, 40, 100]),
    ("syn_only_spaces", "syn", "word-or-anywhere", "      ", [10, 100]),
    # QTextLayout does not break at '\n' (a paragraph separator belongs to QTextDocument); it breaks
    # at U+2028
    ("syn_newline", "syn", "word-or-anywhere", "aaa\nbbb ccc\n\nddd", [30, 60, 120]),
    ("syn_trailing_newline", "syn", "word-or-anywhere", "aaa bbb\n", [40, 200]),
    ("syn_tab_wrap", "syn", "word-or-anywhere", "a\tb", [20, 70, 87, 88, 90]),
    ("syn_tab_wrap_20", "syn", "word-or-anywhere", "aa\tbb\tcc", [30, 45]),
    ("syn_tab_wrap_20_leading", "syn", "word-or-anywhere", "\tb", [10]),
    ("syn_tab_wrap_20_space", "syn", "word-or-anywhere", "a \tb", [25]),
    ("syn_tab_wrap_20_after_space", "syn", "word-or-anywhere", "a\t b", [45]),
    ("syn_tab_wrap_20_word", "syn", "word-or-anywhere", "ab\tcd", [25]),
    ("syn_carriage_return_wrap", "syn", "word-or-anywhere", "aaa\rbbb ccc\r\rddd", [30, 60, 120]),
    ("syn_crlf_wrap", "syn", "word-or-anywhere", "aaa\r\nbbb ccc\r\n\r\nddd", [30, 60, 120]),
    ("syn_trailing_crlf", "syn", "word-or-anywhere", "aaa bbb\r\n", [40, 200]),
    ("syn_carriage_return_break_after", "syn", "word-or-anywhere", "aaaa\rbbb", [45]),
    ("syn_crlf_break_after", "syn", "word-or-anywhere", "aaaa\r\nbbb", [45]),
    ("syn_space_carriage_return_hangs", "syn", "word-or-anywhere", "a \r", [10]),
    ("syn_space_carriage_return_text", "syn", "word-or-anywhere", "a \rb", [10]),
    ("syn_crlf_line_separator", "syn", "word-or-anywhere", "aaa\r\u2028bbb\u2028\r\nccc", [30, 120]),
    ("syn_line_separator", "syn", "word-or-anywhere", "aaa\u2028bbb ccc\u2028\u2028ddd", [30, 60, 120]),
    ("syn_trailing_line_separator", "syn", "word-or-anywhere", "aaa bbb\u2028", [40, 200]),
    ("syn_liga_wrap", "syn", "word-or-anywhere", "fi fl ffi fi", [20, 40, 80]),
    ("syn_slash", "syn", "word-or-anywhere", "ab/cd/ef gh/ij/kl", [30, 50, 90]),
    ("syn_comma", "syn", "word-or-anywhere", "ab,cd,ef gh,ij,kl", [30, 50, 90]),
    ("syn_rtl_wrap", "syn", "word-or-anywhere", "ab \u05d0\u05d1 \u05d2\u05d3 cd \u05d0\u05d1\u05d2 ef", [30, 45, 60, 100]),
    ("syn_rtl_wrap_numbers", "syn", "word-or-anywhere", "\u05d0\u05d1 12 \u05d2\u05d3 34 \u05d0", [25, 40, 70]),
    ("arial_rtl_wrap", "arial", "word-or-anywhere", "abc \u05e9\u05dc\u05d5\u05dd \u05e2\u05d5\u05dc\u05dd def", [40, 70, 110]),
    ("arial_pangram", "arial", "word-or-anywhere", PANGRAM, [60, 100, 160, 240, 500]),
    ("arial_kerned_words", "arial", "word-or-anywhere", "AVAVAV To Ty AVAT Wa", [50, 80, 120]),
    # --- the negative right bearing of the last glyph of a line ---
    # a line ends in each kind of glyph: ae 1.5 px, oslash 0.375 px, germandbls 1/64 px, thorn none,
    # the oslash ligature 0.625 px; spaces before the separator are left out
    ("syn_bearing_ends", "syn", "word-or-anywhere",
     "ab \u00e6\u2028cd \u00f8\u2028ef \u00df\u2028gh \u00fe\u2028ij \u00f8\u00f8\u2028kl \u00e6  \u2028mn", [400]),
    ("syn_bearing_text_end", "syn", "word-or-anywhere", "ab \u00e6", [400]),
    # a text that ends in white space has no bearing, whether the spaces fit or hang
    ("syn_bearing_trailing_space", "syn", "word-or-anywhere", "ab \u00e6 ", [28, 30, 400]),
    ("syn_bearing_trailing_spaces", "syn", "word-or-anywhere", "ab \u00e6   ", [28, 30, 33, 400]),
    ("syn_bearing_line_feed", "syn", "word-or-anywhere", "ab \u00e6\ncd", [400]),
    ("syn_bearing_line_feed_end", "syn", "word-or-anywhere", "ab \u00e6\n", [400]),
    # the bearing counts when the width is tested: the ae fits at 44 px by its advance, at 45.5 by
    # its advance and its bearing
    ("syn_bearing_fit", "syn", "word-or-anywhere", "aaaa \u00e6", [43.75, 44, 44.25, 45.25, 45.5, 46]),
    ("syn_bearing_fit_small", "syn", "word-or-anywhere", "aaaa \u00f8", [43, 43.25, 43.375, 43.5]),
    ("syn_bearing_fit_tiny", "syn", "word-or-anywhere", "aaaa \u00df", [43.5, 43.515625, 43.53125]),
    ("syn_bearing_fit_flush", "syn", "word-or-anywhere", "aaaa \u00fe", [43.234375, 43.25, 43.5]),
    ("syn_bearing_fit_lig", "syn", "word-or-anywhere", "aaaa \u00f8\u00f8", [48.75, 49, 49.375, 49.5]),
    ("syn_bearing_fit_middle", "syn", "word-or-anywhere", "ab \u00e6 cd", [29.125, 30.5, 30.625, 38]),
    ("syn_bearing_fit_word", "syn", "word", "aaaa \u00e6", [44, 45.25, 45.5]),
    # right to left: the bearing is that of the last character in reading order
    ("syn_bearing_rtl", "syn", "word-or-anywhere", "ab \u05d0\u05ea", [400]),
    ("syn_bearing_rtl_base", "syn", "word-or-anywhere", "\u05d0\u05d1 \u05d2\u05ea", [400, 14]),
    ("syn_bearing_rtl_first", "syn", "word-or-anywhere", "\u05d0\u05ea ab", [400]),
    ("syn_bearing_rtl_wrap", "syn", "word-or-anywhere", "ab \u05d0\u05d1 \u05d2\u05ea cd \u05d0\u05ea ef",
     [24, 31, 32, 40, 41, 50, 60, 70]),
    # Arial: every ASCII and Latin-1 glyph ends a line of its own
    ("arial_bearing_ascii", "arial", "word-or-anywhere", "\u2028".join(chr(c) for c in range(0x21, 0x7F)), [400]),
    ("arial_bearing_latin1", "arial", "word-or-anywhere",
     "\u2028".join(chr(c) for c in range(0xA1, 0x100) if c != 0xAD), [400]),
    ("arial_bearing_fit", "arial", "word-or-anywhere", "aa def", [44.4375, 44.96875, 44.984375, 45]),
    ("arial_bearing_fit_words", "arial", "word-or-anywhere", "jumps over the lazy dog",
     [78.203125, 78.40625, 78.421875, 78.5]),
    ("arial_bearing_trailing", "arial", "word-or-anywhere", "aa def ", [44.4375, 44.984375, 45.03125]),
    ("arial_bearing_hanging", "arial", "word-or-anywhere", "def   x", [22.3, 22.8, 23, 45]),
    ("arial_bearing_hanging_end", "arial", "word-or-anywhere", "def   ", [22.3, 22.8, 23, 45]),
    ("arial_bearing_word_or_anywhere", "arial", "word-or-anywhere", "defdef", [22.21875, 22.5, 22.765625]),
    ("arial_bearing_anywhere", "arial", "anywhere", "defdef", [22.21875, 22.5, 22.765625]),
    ("arial_bearing_word", "arial", "word", "defdef", [22.21875, 22.5, 22.765625]),
]


# (name, font id, alignment, text, widths): wrapped lines aligned inside the width.
ALIGN_CASES = [
    ("syn_align_spaces", "syn", "right", "aaaa bbbb cccc ", [34, 40, 100]),
    ("syn_align_spaces", "syn", "center", "aaaa bbbb cccc ", [34, 40, 100]),
    ("syn_align_spaces", "syn", "left", "aaaa bbbb cccc ", [34, 40, 100]),
    ("syn_align_rtl", "syn", "right", "ab \u05d0\u05d1 \u05d2\u05d3 cd \u05d0\u05d1\u05d2 ef", [45, 100]),
    ("syn_align_rtl", "syn", "center", "ab \u05d0\u05d1 \u05d2\u05d3 cd \u05d0\u05d1\u05d2 ef", [45, 100]),
    ("arial_align_words", "arial", "right", "The quick brown fox jumps over", [60, 120]),
    ("arial_align_words", "arial", "center", "The quick brown fox jumps over", [60, 120]),
    # the line is aligned by its advances: the bearing of its last glyph hangs past the edge
    ("syn_align_bearing", "syn", "right", "aaa \u00e6", [60, 100]),
    ("syn_align_bearing", "syn", "center", "aaa \u00e6", [60, 100]),
    ("syn_align_bearing", "syn", "left", "aaa \u00e6", [60, 100]),
    ("syn_align_bearing_wrap", "syn", "right", "aaa \u00e6 bb \u00e6 cc", [30, 40]),
    ("arial_align_bearing", "arial", "right", "def", [100]),
    ("arial_align_bearing", "arial", "center", "def", [100]),
]


# Cases laid out with a tab stop distance (`QTextOption.setTabStopDistance`); the test has the same
# table.
TAB_STOPS = {
    "syn_tab_stop_20": 20.0,
    "syn_tab_on_stop": 15.0,
    "syn_tab_fraction": 14.9,
    "syn_tab_small_gap": 16.0,
    "syn_tab_wrap_20": 20.0,
    "syn_tab_wrap_20_leading": 20.0,
    "syn_tab_wrap_20_space": 20.0,
    "syn_tab_wrap_20_after_space": 20.0,
    "syn_tab_wrap_20_word": 20.0,
}


WRAP_MODES = {
    "nowrap": QTextOption.WrapMode.NoWrap,
    "word": QTextOption.WrapMode.WordWrap,
    "anywhere": QTextOption.WrapMode.WrapAnywhere,
    "word-or-anywhere": QTextOption.WrapMode.WrapAtWordBoundaryOrAnywhere,
}


def utf16(text, upto=None):
    """Number of UTF-16 code units of the code points before character index `upto`."""
    return len(text[:upto].encode("utf-16-le")) // 2


def cursor_row(case, font_id, font, text):
    layout = QTextLayout(text, font)
    opt = QTextOption()
    opt.setWrapMode(QTextOption.WrapMode.NoWrap)
    if case in TAB_STOPS:
        opt.setTabStopDistance(TAB_STOPS[case])
    layout.setTextOption(opt)
    layout.beginLayout()
    line = layout.createLine()
    line.setLineWidth(100000)
    layout.endLayout()
    assert layout.lineCount() == 1
    n16 = len(text.encode("utf-16-le")) // 2
    offs = [0]
    while offs[-1] < n16:
        nxt = layout.nextCursorPosition(offs[-1])
        assert nxt > offs[-1], (case, offs)
        offs.append(nxt)
    from PySide6.QtGui import QTextLine

    trailing = QTextLine.Edge.Trailing
    pairs = ["%d:%r:%r" % (o, line.cursorToX(o)[0], line.cursorToX(o, trailing)[0]) for o in offs]
    return ["cursor", case, font_id, text.encode().hex(), str(len(offs))] + pairs


# Texts whose hit tests (xToCursor) are recorded.
HIT_CASES = [
    "syn_AVAVAV", "syn_liga_run", "syn_combining_acute", "syn_rtl_hebrew", "syn_rtl_mixed",
    "syn_rtl_numbers", "syn_rtl_nested", "syn_rtl_two_runs", "syn_rtl_trailing_space",
    "arial_arabic_mixed", "arial_emoji_zwj", "arial_hebrew_emoji", "syn_bearing", "syn_bearing_rtl",
    "arial_bearing",
]


def hit_row(case, font_id, font, text):
    """xToCursor at x from -3 to the width + 3 in steps of 1.25 px, in both cursor modes."""
    layout = QTextLayout(text, font)
    opt = QTextOption()
    opt.setWrapMode(QTextOption.WrapMode.NoWrap)
    layout.setTextOption(opt)
    layout.beginLayout()
    line = layout.createLine()
    line.setLineWidth(100000)
    layout.endLayout()
    width = line.naturalTextWidth()
    parts = []
    x = -3.0
    while x <= width + 3:
        parts.append(
            "%r:%d:%d"
            % (x, line.xToCursor(x, QTextLine.CursorPosition.CursorBetweenCharacters),
               line.xToCursor(x, QTextLine.CursorPosition.CursorOnCharacter))
        )
        x += 1.25
    return ["hit", case, font_id, text.encode().hex(), str(len(parts))] + parts


# (name, font id, text, width): wrapped text whose lines' hit tests are recorded.
HITWRAP_CASES = [
    ("syn_hitwrap", "syn", "aaaa bbbb cccc dddd", 45),
    ("syn_hitwrap_long", "syn", "aa bbbbbbbbbbbbbbbbbbbb cc", 40),
    ("syn_hitwrap_rtl", "syn", "ab \u05d0\u05d1 \u05d2\u05d3 cd \u05d0\u05d1\u05d2 ef", 45),
    ("syn_hitwrap_rtl_base", "syn", "\u05d0\u05d1 ab \u05d2\u05d3 cd \u05d0\u05d1\u05d2 ef gh", 45),
]


def hitwrap_rows(case, font_id, font, text, width):
    """xToCursor on every line of the text wrapped at `width`, x from -3 to width + 3."""
    layout = QTextLayout(text, font)
    opt = QTextOption()
    opt.setWrapMode(QTextOption.WrapMode.WrapAtWordBoundaryOrAnywhere)
    layout.setTextOption(opt)
    layout.beginLayout()
    lines = []
    while True:
        line = layout.createLine()
        if not line.isValid():
            break
        line.setLineWidth(width)
        lines.append(line)
    layout.endLayout()
    rows = []
    for number, line in enumerate(lines):
        parts = []
        x = -3.0
        while x <= width + 3:
            parts.append(
                "%r:%d:%d"
                % (x, line.xToCursor(x, QTextLine.CursorPosition.CursorBetweenCharacters),
                   line.xToCursor(x, QTextLine.CursorPosition.CursorOnCharacter))
            )
            x += 2.5
        rows.append(["hitwrap", case, font_id, str(width), str(number), text.encode().hex(),
                     str(len(parts))] + parts)
    return rows


def wrap_row(case, font_id, font, mode, width, text):
    layout = QTextLayout(text, font)
    opt = QTextOption()
    opt.setWrapMode(WRAP_MODES[mode])
    if case in TAB_STOPS:
        opt.setTabStopDistance(TAB_STOPS[case])
    layout.setTextOption(opt)
    layout.beginLayout()
    lines = []
    while True:
        line = layout.createLine()
        if not line.isValid():
            break
        line.setLineWidth(width)
        lines.append(line)
    layout.endLayout()
    parts = [
        "%d:%d:%r:%r"
        % (l.textStart(), l.textLength(), l.naturalTextWidth(),
           l.cursorToX(l.textStart() + l.textLength())[0])
        for l in lines
    ]
    return ["wrap", case, font_id, mode, "%s" % (width,), text.encode().hex(), str(len(parts))] + parts


# Templates of the line break opportunity cases: a word, a punctuation mark P and another word.
LB_TEMPLATES = [("LL", "aaaa%sbbbb"), ("DD", "1111%s2222"), ("LD", "aaaa%s2222"), ("DL", "1111%sbbbb")]
LB_PUNCT = [c for c in range(0x21, 0x7F) if not chr(c).isalnum()]


def lb_rows(font_id, font):
    """One row per template and punctuation mark: `anywhere` wrapping at a width that holds the
    text up to two characters past the mark. A break opportunity after the mark ends the first
    line right after it; without one the line runs on to the width."""
    from PySide6.QtGui import QFontMetricsF

    fm = QFontMetricsF(font)
    rows = []
    for name, tpl in LB_TEMPLATES:
        for c in LB_PUNCT:
            text = tpl % chr(c)
            head = text[: text.index(chr(c)) + 1]
            width = fm.horizontalAdvance(text[: len(head) + 2]) + 0.25
            rows.append(wrap_row("lb_%s_%02x" % (name, c), font_id, font, "word-or-anywhere", width, text))
    return rows


def align_row(case, font_id, font, align, width, text):
    layout = QTextLayout(text, font)
    opt = QTextOption(
        {"left": Qt.AlignmentFlag.AlignLeft, "right": Qt.AlignmentFlag.AlignRight,
         "center": Qt.AlignmentFlag.AlignHCenter}[align]
    )
    opt.setWrapMode(QTextOption.WrapMode.WrapAtWordBoundaryOrAnywhere)
    layout.setTextOption(opt)
    layout.beginLayout()
    lines = []
    while True:
        line = layout.createLine()
        if not line.isValid():
            break
        line.setLineWidth(width)
        lines.append(line)
    layout.endLayout()
    parts = [
        "%d:%d:%r:%r"
        % (l.textStart(), l.textLength(), l.cursorToX(l.textStart())[0],
           l.cursorToX(l.textStart() + l.textLength())[0])
        for l in lines
    ]
    return ["align", case, font_id, align, str(width), text.encode().hex(), str(len(parts))] + parts


def main():
    sys.stdout.reconfigure(newline="\n")  # the fixture is committed with LF line ends
    ttf = build_font()
    with open(os.path.join(HERE, "oracle_text.ttf"), "wb") as f:
        f.write(ttf)

    app = QGuiApplication(["x", "-platform", "windows"])
    dpr = app.devicePixelRatio()
    assert dpr != 1.0, "the DirectWrite engine needs a device pixel ratio other than 1"
    fid = QFontDatabase.addApplicationFontFromData(QByteArray(ttf))
    assert fid >= 0
    families = QFontDatabase.applicationFontFamilies(fid)
    assert families == ["FerrokitOracleText"], families

    fonts = {}
    for font_id, family in (("syn", "FerrokitOracleText"), ("arial", "Arial")):
        f = QFont(family)
        f.setPixelSize(PX)
        f.setStyleStrategy(QFont.StyleStrategy.PreferDefault)
        assert QFontInfo(f).family() == family, (family, QFontInfo(f).family())
        fonts[font_id] = f

    print("# Generated by gen_text_layout_oracle.py from PySide6 %s (Qt %s)"
          % (PySide6.__version__, PySide6.QtCore.qVersion()))
    print("# platform %s, devicePixelRatio != 1 (Qt DirectWrite engine), %d px, Arial from the "
          "system" % (app.platformName(), PX))
    print("font syn file %d FerrokitOracleText" % PX)
    print("font arial family %d Arial" % PX)
    for case, font_id, text in CURSOR_CASES:
        print(" ".join(cursor_row(case, font_id, fonts[font_id], text)))
    for case, font_id, text in CURSOR_CASES:
        if case in HIT_CASES:
            print(" ".join(hit_row(case, font_id, fonts[font_id], text)))
    for case, font_id, text, width in HITWRAP_CASES:
        for row in hitwrap_rows(case, font_id, fonts[font_id], text, width):
            print(" ".join(row))
    for case, font_id, mode, text, widths in WRAP_CASES:
        for w in widths:
            print(" ".join(wrap_row(case, font_id, fonts[font_id], mode, w, text)))
    for mode in WRAP_MODES:
        for case, text, widths in MODE_CASES:
            for w in widths:
                print(" ".join(wrap_row("%s_%s" % (case, mode), "syn", fonts["syn"], mode, w, text)))
    for row in lb_rows("syn", fonts["syn"]):
        print(" ".join(row))
    for case, font_id, align, text, widths in ALIGN_CASES:
        for w in widths:
            print(" ".join(align_row(case, font_id, fonts[font_id], align, w, text)))


if __name__ == "__main__":
    main()
