#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates text_draw_golden.txt, the measurements of real Qt that tests/test_text_draw_golden.rs
compares `FontMetrics::horizontal_advance_exact` and `Painter::draw_text` with: the advance of a
whole string (`QFontMetricsF::horizontalAdvance`, with and without a `QTextOption`) and the
pixels `QPainter::drawText(QPointF, text)` makes, for strings with bidirectional text, tabs and
controls. Run it once on every platform and concatenate the outputs:

    # Linux (FreeType; any 6.12 PySide6 with fontconfig hintslight, DejaVu Sans installed)
    QT_QPA_PLATFORM=offscreen python gen_text_draw_golden.py L > golden_linux.txt
    # Windows (DirectWrite; the script sets QT_SCALE_FACTOR=2, which selects Qt's DirectWrite engine
    # and its design advances, as gen_text_layout_oracle.py does; the greyscale pixels need
    # ClearType off, see gen_directwrite_placement_golden.py)
    python gen_text_draw_golden.py W > golden_windows.txt
    cat golden_linux.txt golden_windows.txt > text_draw_golden.txt

Needs oracle_text.ttf next to this script (built by gen_text_layout_oracle.py). Its units per em is
1024 and the cases use 16 and 32 px, so every advance is a multiple of 1/64 px and the numbers are the
same under every font backend; the `syn` records are compared on every platform. The `sys`
records use an installed font (their file's size and hash are in the record: the test compares
only when the font of the machine is that file) and so depend on its release.

Record formats (fields separated by one space, strings are UTF-8 in hex):
    FONT ID KIND SPEC PX FINGERPRINT
        a font: `syn` (oracle_text.ttf) or `sys` (SPEC a family name with `_` for blanks,
        FINGERPRINT `size:fnv1a64` of its file); the first letter of ID is the platform of the Qt
        that measured it, L or W
    A ID HEXTEXT ADVANCE ADVANCE_INT
        `QFontMetricsF.horizontalAdvance` (shortest round-trip decimal) and
        `QFontMetrics.horizontalAdvance`
    O ID TABSTOP HEXTEXT ADVANCE
        `QFontMetricsF.horizontalAdvance(text, option)` with `QTextOption.setTabStopDistance(TABSTOP)`
    D ID X Y W H HEXTEXT IMAGE
        `QPainter.drawText(QPointF(X, Y), text)` into a W x H `Format_ARGB32_Premultiplied` image
        (logical pixels; the Windows images have a device pixel ratio of 2) filled with white,
        black pen; IMAGE: `h` and the FNV-1a 64 of the premultiplied RGBA bytes, or, with the
        script's `full` argument, those bytes run-length encoded as `RRGGBBAA*count` runs
"""
import os
import subprocess
import sys

PLATFORM = sys.argv[1] if len(sys.argv) > 1 else "L"
FULL = sys.argv[2:3] == ["full"]
# Windows: Qt's DirectWrite engine with design advances is the one a device pixel ratio other than 1
# selects; the ratio is 2 whatever the screen's own is (QT_SCALE_FACTOR is relative to it).
DPR = 2.0 if PLATFORM == "W" else 1.0
if PLATFORM == "W":
    _probe = ("from PySide6.QtGui import QGuiApplication;"
              "a=QGuiApplication(['x']);print(a.primaryScreen().devicePixelRatio())")
    _env = {k: v for k, v in os.environ.items() if k not in ("QT_SCALE_FACTOR", "QT_ENABLE_HIGHDPI_SCALING")}
    _screen = float(subprocess.run([sys.executable, "-c", _probe], capture_output=True, text=True,
                                   env=_env, check=True).stdout)
    os.environ["QT_SCALE_FACTOR"] = repr(DPR / _screen)

import PySide6
from PySide6.QtCore import QPointF
from PySide6.QtGui import (QColor, QFont, QFontDatabase, QFontMetrics, QFontMetricsF, QGuiApplication,
                           QImage, QPainter, QPen, QTextOption)

HERE = os.path.dirname(os.path.abspath(__file__))

A, B, G, D, M = "\u05d0", "\u05d1", "\u05d2", "\u05d3", "\u05dd"  # alef bet gimel dalet final mem
HEB = A + B + G + D
LRM, RLM = "\u200e", "\u200f"
LRE, RLE, PDF, LRO, RLO = "\u202a", "\u202b", "\u202c", "\u202d", "\u202e"
LRI, RLI, FSI, PDI = "\u2066", "\u2067", "\u2068", "\u2069"

# Strings of the synthetic font: Latin, the five Hebrew letters, digits and ASCII punctuation, tabs.
SYN_CASES = [
    # left to right
    "abc", "Hello Wor-ld 0123 AVTo", "AV To", "fi fl ffi", "a\u0301b e\u0308o",
    # right to left only
    HEB, A + B + " " + G + D + B, A + B + G + M,
    # mixed directions
    "abc " + HEB + " def", HEB + " abc " + A + G + D, "abc " + A + B + G, A + B + G + " abc",
    "abc " + A + B + G + " def " + A + B + G, "ab" + A + B + "cd", A + B + "ab" + G + D,
    # numbers in right to left text and the separators that go with them
    A + B + G + " 123 " + G + D + B, "123 " + A + B + G, A + B + G + " 123", A + B + G + " 12.5 " + D + A,
    "abc 123 " + A + B + G, A + B + "-123", A + B + G + " 1,234 " + D + A, A + B + " 12 34 " + G,
    # neutrals
    "abc, " + HEB + "!", "(abc) " + HEB + " (def)", HEB + " (abc)", HEB + " (abc) " + G + D + B,
    "a.b " + A + B + "." + G + D, A + B + G + "... abc", A + B + G + " - abc - " + D + A + B,
    '"' + A + B + G + '" abc', "[" + A + B + "] [ab]", "<" + A + B + "> a=b",
    # white space at the ends and between runs
    " abc", "abc ", A + B + G + " ", " " + A + B + G, "abc " + A + B + G + " ", A + B + G + " abc ",
    "  " + A + B + G + "  abc  ", "abc  " + A + B + G, A + B + " " + " " + G + D,
    # tabs, default stop distance (80 px at any size)
    "a\tb", "\ta", "a\t", "ab\t\tc", "aaaaaaaaaaaa\tb", "aaaaa\tb\tc", "\t\t", "abc\tabc\tabc",
    A + B + G + "\tabc", "abc\t" + A + B + G, "abc\t" + A + B + G + "\tdef", A + B + "\t" + G + D,
    "abc " + A + B + "\tdef", "\t" + A + B, "abc\t", A + B + " abc\t" + G + D, "a b\t" + A + B + " " + G,
    "bbbbb a\tb", "\ta\t" + A + B + "\tb",
    # base direction and the control characters
    RLM + "abc", LRM + A + B + G, "abc" + RLM + A + B + G, A + B + LRM + "abc", A + B + RLM + "abc",
    "a\nb", "a\u2028b", "a\u2029b", A + "\n" + B, "a\u200db", "a\u200cb", "a\u00adb", "a\ufeffb",
    # explicit embeddings, overrides and isolates
    RLO + "abc" + PDF, LRO + A + B + G + PDF, RLE + A + B + " abc" + PDF, LRE + A + B + PDF + " abc",
    "abc " + RLI + A + B + " abc" + PDI + " def", RLI + "abc" + PDI + " " + A + B,
    FSI + A + B + PDI + " abc", LRI + A + B + PDI + " abc", RLI + A + B + PDI + " abc",
    LRI + "abc" + PDI + " " + A + B, A + B + " " + LRI + "abc" + PDI + " " + G,
]

# Strings of an installed font that has Arabic and Hebrew (DejaVu Sans on the Linux host, Arial on
# Windows): real words, Arabic joining, numbers.
HEB_WORD = "\u05e9\u05dc\u05d5\u05dd"  # shalom
AR_WORD = "\u0627\u0644\u0639\u0631\u0628\u064a\u0629"  # al-arabiyya
AR_SHORT = "\u0627\u0644\u0639\u0631"
AR_HELLO = "\u0645\u0631\u062d\u0628\u0627"
SYS_CASES = [
    "Hello, world 0123", "AVATAR To Wa", "abc " + HEB_WORD + " def", HEB_WORD + " abc " + HEB_WORD,
    "abc " + AR_WORD + " def", "abc " + AR_SHORT + " def", AR_WORD, AR_HELLO + " " + AR_WORD,
    AR_WORD + " abc " + AR_HELLO, AR_WORD + " 123 " + AR_HELLO, AR_WORD + " (abc) " + AR_HELLO,
    "abc " + AR_WORD + " " + HEB_WORD + " def", AR_WORD + "\tabc", "abc\t" + AR_WORD, "a\tb",
    "abc\t" + HEB_WORD + "\tdef", "a\u00a0b\tc", AR_WORD + " ", " " + HEB_WORD,
    "a" + RLM + AR_WORD + "b", RLI + AR_WORD + PDI + " abc",
    # characters that are never shown, where the font's own space has no ink
    "a\u200db", "a\u200cb", "a\u00adb", "a\ufeffb", "a" + LRM + "b", "abc" + RLM + HEB_WORD,
    RLO + "abc" + PDF, RLE + HEB_WORD + " abc" + PDF, "abc " + RLI + HEB_WORD + " abc" + PDI + " def",
    LRI + HEB_WORD + PDI + " abc",
]

# `QFontMetricsF::horizontalAdvance` ends the string at U+009C; `drawText` does not. The Arabic
# one is for the installed font only: the synthetic font has no Arabic, so its width would be that
# of whichever fallback font the machine's fontconfig picks (DejaVu Sans in Qt on the machines
# measured, not what ferrokit's fixed fallback slots pick on every machine).
ADVANCE_ONLY_CASES = ["ab\u009ccd", "\u009cab", "a\u009c", HEB + "\u009c" + "abc"]
SYS_ADVANCE_ONLY_CASES = [AR_WORD + "\u009c" + "abc"]

SYN_SIZES_ADVANCE = [16, 32]
SYN_SIZES_DRAW = [16, 32]
SYS_SIZES = [13, 16]
TAB_OPTIONS = [20.0, 30.5, 100.0]
TAB_CASES = ["a\tb", "aaa\tb", "ab\t\tc", "abc\t" + A + B + G, A + B + "\tabc", "abc\t" + A + B + "\tdef",
             "\tabc", "bbbbb a\tb", "aaaaaaaaaaaa\tb"]
FRACTIONS = [0.0, 0.25]  # x of the pen is 4 + fraction


def fnv1a(data):
    h = 0xCBF29CE484222325
    for b in data:
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def image_field(img):
    """`h` and the FNV-1a 64 of the RGBA bytes (the default), or the whole image run-length encoded
    (a second argument `full`, for looking at what differs)."""
    if FULL:
        return rle(img)
    w, h, bpl = img.width(), img.height(), img.bytesPerLine()
    buf = bytes(img.constBits())
    rgba = bytearray()
    for y in range(h):
        for x in range(w):
            b, g, r, a = buf[y * bpl + 4 * x: y * bpl + 4 * x + 4]
            rgba += bytes((r, g, b, a))
    return "h%016x" % fnv1a(rgba)


def rle(img):
    """Run-length encoding of the premultiplied RGBA bytes of a QImage: RRGGBBAA*count runs."""
    w, h, bpl = img.width(), img.height(), img.bytesPerLine()
    buf = bytes(img.constBits())
    runs = []
    cur, n = None, 0
    for y in range(h):
        for x in range(w):
            b, g, r, a = buf[y * bpl + 4 * x: y * bpl + 4 * x + 4]  # little-endian ARGB32
            px = "%02x%02x%02x%02x" % (r, g, b, a)
            if px == cur:
                n += 1
            else:
                if cur is not None:
                    runs.append("%s*%d" % (cur, n))
                cur, n = px, 1
    runs.append("%s*%d" % (cur, n))
    return ",".join(runs)


def system_font():
    """(family, file) of the installed font with Arabic and Hebrew the sys records use."""
    if PLATFORM == "W":
        return "Arial", "C:/Windows/Fonts/arial.ttf"
    return "DejaVu Sans", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"


def make_font(family, px):
    f = QFont(family)
    f.setPixelSize(px)
    return f


def main():
    if PLATFORM == "W":
        # Greyscale text needs Qt's ClearType off: standard smoothing for this process's lifetime
        # (not persisted), restored on exit.
        import atexit
        import ctypes

        user32 = ctypes.windll.user32
        old = ctypes.c_uint(0)
        user32.SystemParametersInfoW(0x200A, 0, ctypes.byref(old), 0)  # SPI_GETFONTSMOOTHINGTYPE
        user32.SystemParametersInfoW(0x200B, 0, ctypes.c_void_p(1), 0)  # SPI_SETFONTSMOOTHINGTYPE
        atexit.register(lambda: user32.SystemParametersInfoW(0x200B, 0, ctypes.c_void_p(old.value), 0))
    app = QGuiApplication(sys.argv)
    assert abs(app.devicePixelRatio() - DPR) < 1e-9, (app.devicePixelRatio(), DPR)
    print("# PySide6 %s platform %s; see gen_text_draw_golden.py" % (PySide6.__version__, PLATFORM))
    fid = QFontDatabase.addApplicationFont(os.path.join(HERE, "oracle_text.ttf"))
    assert fid >= 0
    syn_family = QFontDatabase.applicationFontFamilies(fid)[0]
    sys_family, sys_file = system_font()
    sys_data = open(sys_file, "rb").read()
    fonts = []  # (id, family, px, cases)
    for px in SYN_SIZES_ADVANCE:
        fonts.append(("%ssyn%d" % (PLATFORM, px), "syn", syn_family, px, SYN_CASES, "-"))
    for px in SYS_SIZES:
        fonts.append(("%ssys%d" % (PLATFORM, px), "sys", sys_family, px, SYS_CASES,
                      "%d:%x" % (len(sys_data), fnv1a(sys_data))))
    for ident, kind, family, px, cases, finger in fonts:
        print("FONT %s %s %s %d %s" % (ident, kind, family.replace(" ", "_"), px, finger))
    for ident, kind, family, px, cases, finger in fonts:
        font = make_font(family, px)
        fm, fmi = QFontMetricsF(font), QFontMetrics(font)
        draw = (kind == "syn" and px in SYN_SIZES_DRAW) or kind == "sys"
        for text in ADVANCE_ONLY_CASES + (SYS_ADVANCE_ONLY_CASES if kind == "sys" else []):
            print("A %s %s %r %d" % (ident, text.encode().hex(), fm.horizontalAdvance(text),
                                     fmi.horizontalAdvance(text)))
        for text in cases:
            hx = text.encode().hex()
            adv = fm.horizontalAdvance(text)
            print("A %s %s %r %d" % (ident, hx, adv, fmi.horizontalAdvance(text)))
            if kind == "syn" and px == 16 and text in TAB_CASES:
                for tab in TAB_OPTIONS:
                    option = QTextOption()
                    option.setTabStopDistance(tab)
                    print("O %s %r %s %r" % (ident, tab, hx, fm.horizontalAdvance(text, option)))
            if not draw or (px == 32 and cases is SYN_CASES and len(text) % 3):
                continue
            ascent = fm.ascent()
            h = int(ascent + fm.descent()) + 8
            for fraction in FRACTIONS if (px == 16 or kind == "sys") else FRACTIONS[:1]:
                x, y = 4.0 + fraction, 4.0 + ascent
                w = int(adv) + 12  # logical size; the image is DPR times larger
                img = QImage(round(w * DPR), round(h * DPR), QImage.Format_ARGB32_Premultiplied)
                img.setDevicePixelRatio(DPR)
                img.fill(QColor("#ffffff"))
                p = QPainter(img)
                p.setRenderHint(QPainter.TextAntialiasing, True)
                p.setFont(font)
                p.setPen(QPen(QColor(0, 0, 0, 255)))
                p.drawText(QPointF(x, y), text)
                p.end()
                print("D %s %r %r %d %d %s %s" % (ident, x, y, w, h, hx, image_field(img)))


main()
