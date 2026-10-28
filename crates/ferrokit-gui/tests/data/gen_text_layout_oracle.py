#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates oracle_text.ttf and text_layout_oracle.txt for tests/test_text_layout_oracle.rs.

The fixture is what PySide6's real QTextLayout / QTextLine measure; the file header records the
version, platform and font engine. Run it on Windows (the DirectWrite engine the Rust side
reproduces) with fontTools installed:

    python gen_text_layout_oracle.py > text_layout_oracle.txt

`oracle_text.ttf` (written next to this script) is a synthetic font built here, so that the cases
that exercise shaping (kerning, a ligature, ligature carets, combining marks, right-to-left
letters) give the same numbers on every platform and do not depend on an installed font's
version. Its units per em is 1024 and the cases use 16 px, so every design value is an exact
multiple of 1/64 px: DirectWrite's truncation to 26.6 does not change any advance and the
expected numbers are the same under every font backend. The cases that use the installed Arial
(with Segoe UI Emoji and Arial's Hebrew) are Windows-only in the Rust test.

Line formats (strings are UTF-8 in hex; offsets are UTF-16 code units, as Qt reports them; x
values are Qt's doubles, shortest round-trip decimal):
    font ID file|family PX NAME      font ID, size in pixels; `file` fonts are oracle_text.ttf
    cursor CASE FONT HEXTEXT N OFF:X ...
                                     cursorToX(OFF) of every grapheme boundary reached by
                                     nextCursorPosition from 0, on one unwrapped line;
                                     then N is the count and the pairs follow
    wrap CASE FONT MODE WIDTH HEXTEXT N START:LEN:NATURAL:ENDX ...
                                     lines of the text wrapped at WIDTH (mode: word | anywhere):
                                     textStart, textLength, naturalTextWidth() (trailing spaces
                                     excluded) and cursorToX(textStart + textLength) of each
    align CASE FONT ALIGN WIDTH HEXTEXT N START:LEN:X0:XEND ...
                                     lines wrapped (as `anywhere`) at WIDTH and aligned
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
    QTextOption,
)

HERE = os.path.dirname(os.path.abspath(__file__))
UPEM = 1024
PX = 16

# (codepoint, glyph name, advance in font units); the glyph is a box of that width.
GLYPHS = (
    [(0x20, "space", 256), (0x2D, "hyphen", 320), (0x2C, "comma", 256), (0x2E, "period", 256),
        (0x2F, "slash", 288)]
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

FEATURES = """
languagesystem DFLT dflt;
languagesystem latn dflt;
languagesystem hebr dflt;

@BASE = [A B C D E F G H I J K L M N O P Q R S T U V W X Y Z a e o u];
@MARK = [acutecomb dieresiscomb];

table GDEF {
    GlyphClassDef @BASE, [f_i f_l f_f_i], @MARK, ;
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
} liga;

markClass [acutecomb dieresiscomb] <anchor 0 0> @TOP;

feature mark {
    pos base [A E a e o u] <anchor 300 700> mark @TOP;
} mark;
"""


def build_font():
    order = [".notdef"] + [g[1] for g in GLYPHS] + [g[0] for g in LIGATURES]
    cmap = {cp: name for cp, name, _ in GLYPHS}
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap(cmap)
    adv = {".notdef": 600}
    adv.update({name: a for _, name, a in GLYPHS})
    adv.update({name: a for name, a in LIGATURES})
    glyphs = {}
    for name in order:
        pen = TTGlyphPen(None)
        w = adv[name]
        if w:
            pen.moveTo((40, 0))
            pen.lineTo((40, 700))
            pen.lineTo((w - 40, 700))
            pen.lineTo((w - 40, 0))
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
    ("arial_hebrew", "arial", "\u05e9\u05dc\u05d5\u05dd"),
    ("arial_mixed_rtl", "arial", "abc \u05e9\u05dc\u05d5\u05dd def"),
]

PANGRAM = "The quick brown fox jumps over the lazy dog"

# (name, font id, mode, text, widths): wrapped lines.
WRAP_CASES = [
    ("syn_words", "syn", "anywhere", "AT Wa To Ty VA aaa bbb ccc", [60, 100, 140, 200, 400]),
    ("syn_hyphen", "syn", "anywhere", "ab-cd ef-gh ij-kl", [50, 80, 120]),
    ("syn_trailing_space", "syn", "anywhere", "aaaa bbbb cccc", [60, 62, 64, 70, 80, 100]),
    ("syn_long_word", "syn", "anywhere", "aa bbbbbbbbbbbbbbbbbbbb cc", [40, 80, 120]),
    ("syn_long_word_word_mode", "syn", "word", "aa bbbbbbbbbbbbbbbbbbbb cc", [40, 80, 120]),
    # a space that does not fit hangs past the width in Qt
    ("syn_hanging_space", "syn", "anywhere", "aaaa bbbb cccc", [30, 31, 33, 34, 35]),
    ("syn_hanging_spaces", "syn", "anywhere", "aaaa   bbbb", [30, 32, 38, 60]),
    ("syn_hyphen_hang", "syn", "anywhere", "ab-cd ef-gh ij-kl", [76, 77, 80]),
    # the spaces at the end of the text stay in the width of the last line
    ("syn_final_spaces", "syn", "anywhere", "aaaa bbbb  ", [30, 100]),
    ("syn_leading_spaces", "syn", "anywhere", "  aaaa bbbb", [30, 40, 100]),
    ("syn_only_spaces", "syn", "anywhere", "      ", [10, 100]),
    # QTextLayout does not break at '\n' (a paragraph separator belongs to QTextDocument); it breaks
    # at U+2028
    ("syn_newline", "syn", "anywhere", "aaa\nbbb ccc\n\nddd", [30, 60, 120]),
    ("syn_trailing_newline", "syn", "anywhere", "aaa bbb\n", [40, 200]),
    ("syn_line_separator", "syn", "anywhere", "aaa\u2028bbb ccc\u2028\u2028ddd", [30, 60, 120]),
    ("syn_trailing_line_separator", "syn", "anywhere", "aaa bbb\u2028", [40, 200]),
    ("syn_liga_wrap", "syn", "anywhere", "fi fl ffi fi", [20, 40, 80]),
    ("syn_slash", "syn", "anywhere", "ab/cd/ef gh/ij/kl", [30, 50, 90]),
    ("syn_comma", "syn", "anywhere", "ab,cd,ef gh,ij,kl", [30, 50, 90]),
    ("arial_pangram", "arial", "anywhere", PANGRAM, [60, 100, 160, 240, 500]),
    ("arial_kerned_words", "arial", "anywhere", "AVAVAV To Ty AVAT Wa", [50, 80, 120]),
]


# (name, font id, alignment, text, widths): wrapped lines aligned inside the width.
ALIGN_CASES = [
    ("syn_align_spaces", "syn", "right", "aaaa bbbb cccc ", [34, 40, 100]),
    ("syn_align_spaces", "syn", "center", "aaaa bbbb cccc ", [34, 40, 100]),
    ("syn_align_spaces", "syn", "left", "aaaa bbbb cccc ", [34, 40, 100]),
    ("arial_align_words", "arial", "right", "The quick brown fox jumps over", [60, 120]),
    ("arial_align_words", "arial", "center", "The quick brown fox jumps over", [60, 120]),
]


def utf16(text, upto=None):
    """Number of UTF-16 code units of the code points before character index `upto`."""
    return len(text[:upto].encode("utf-16-le")) // 2


def cursor_row(case, font_id, font, text):
    layout = QTextLayout(text, font)
    opt = QTextOption()
    opt.setWrapMode(QTextOption.WrapMode.NoWrap)
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
    pairs = ["%d:%r" % (o, line.cursorToX(o)[0]) for o in offs]
    return ["cursor", case, font_id, text.encode().hex(), str(len(offs))] + pairs


def wrap_row(case, font_id, font, mode, width, text):
    layout = QTextLayout(text, font)
    opt = QTextOption()
    opt.setWrapMode(
        QTextOption.WrapMode.WrapAtWordBoundaryOrAnywhere
        if mode == "anywhere"
        else QTextOption.WrapMode.WordWrap
    )
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
    return ["wrap", case, font_id, mode, str(width), text.encode().hex(), str(len(parts))] + parts


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
    for case, font_id, mode, text, widths in WRAP_CASES:
        for w in widths:
            print(" ".join(wrap_row(case, font_id, fonts[font_id], mode, w, text)))
    for case, font_id, align, text, widths in ALIGN_CASES:
        for w in widths:
            print(" ".join(align_row(case, font_id, fonts[font_id], align, w, text)))


if __name__ == "__main__":
    main()
