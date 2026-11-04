#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates tests/data/directwrite_placement_golden.txt, the whole-string `QPainter.drawText`
pixels of real Qt on Windows that tests/test_directwrite_placement.rs compares ferrokit's
`Painter::draw_text` with.

Needs Windows, PySide6 (measured with 6.12.0) and the synthetic font tests/data/oracle_text.ttf
(built by gen_text_layout_oracle.py). Run it with `python gen_directwrite_placement_golden.py >
directwrite_placement_golden.txt`; it starts itself once per device pixel ratio (the ratio is set
through QT_ENABLE_HIGHDPI_SCALING / QT_SCALE_FACTOR, relative to the ratio of the primary screen).

Greyscale text needs Qt's ClearType off: the child process switches the system's font smoothing
type to "standard" for its own lifetime (not persisted) and restores it on exit.

Each line: `dpr smoothing px pen_rgba x ls text_id width height fnv1a64`, the hash taken over the
RGBA bytes (premultiplied, as `QImage.Format_ARGB32_Premultiplied` and ferrokit's pixmap hold
them) of an image of `width x height` device pixels, drawn with the pen and background `PENS`
gives. The cases are chosen so that nothing depends on behaviour ferrokit does not
have yet (see the test's documentation): sizes and strings whose advances are exact.
"""
import atexit
import ctypes
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))

DPRS = (1.0, 1.25, 1.5, 2.0)
# Text with distinct glyph widths and kerning: exact in 26.6 at 16 px and 20 px.
TEXT_A = "Hello Wor-ld 0123 AVTo"
# Capitals whose advances are whole pixels at 32 px (640 + 16 * (i % 5) font units with i % 5 even,
# 1/32 px per unit), without kerning pairs: the layout at a ratio of 1 rounds every advance to a
# whole pixel, which is then exact.
TEXT_B = "ACEFHJKMOPRTUWY"
PENS = {
    "black": ((0, 0, 0, 255), (255, 255, 255, 255)),
    "light": ((226, 232, 240, 255), (15, 23, 42, 255)),
    "red": ((220, 38, 38, 255), (255, 255, 255, 255)),
    "alpha": ((100, 160, 40, 200), (240, 240, 200, 255)),
}


def cases(dpr):
    """(px, pen name, x fraction, letter spacing, text id)."""
    out = []
    if dpr != 1.0:
        for px in (16, 20):
            for pen in PENS:
                out.append((px, pen, 0.0, 0.0, "a"))
        for fx in (0.25, 0.5, 0.75):
            out.append((16, "black", fx, 0.0, "a"))
        for ls in (0.4, -1.0):
            out.append((16, "black", 0.0, ls, "a"))
    else:
        for pen in PENS:
            out.append((32, pen, 0.0, 0.0, "b"))
        for fx in (0.25, 0.5, 0.75):
            out.append((32, "black", fx, 0.0, "b"))
        out.append((32, "black", 0.0, 0.4, "b"))
    return out


def fnv1a64(data):
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def child(dpr, grey):
    from PySide6.QtCore import QByteArray, QPointF
    from PySide6.QtGui import (QColor, QFont, QFontDatabase, QFontInfo, QGuiApplication, QImage,
                               QPainter)

    if grey:
        user32 = ctypes.windll.user32
        old = ctypes.c_uint(0)
        user32.SystemParametersInfoW(0x200A, 0, ctypes.byref(old), 0)  # SPI_GETFONTSMOOTHINGTYPE
        user32.SystemParametersInfoW(0x200B, 0, ctypes.c_void_p(1), 0)  # standard, not persisted
        atexit.register(lambda: user32.SystemParametersInfoW(0x200B, 0, ctypes.c_void_p(old.value), 0))
    app = QGuiApplication(["x", "-platform", "windows"])
    assert abs(app.devicePixelRatio() - dpr) < 1e-9, (app.devicePixelRatio(), dpr)
    with open(os.path.join(HERE, "oracle_text.ttf"), "rb") as f:
        fid = QFontDatabase.addApplicationFontFromData(QByteArray(f.read()))
    assert QFontDatabase.applicationFontFamilies(fid) == ["FerrokitOracleText"]
    for px, pen_name, fx, ls, text_id in cases(dpr):
        font = QFont("FerrokitOracleText")
        font.setPixelSize(px)
        assert QFontInfo(font).family() == "FerrokitOracleText"
        if ls:
            font.setLetterSpacing(QFont.AbsoluteSpacing, ls)
        pen, bg = PENS[pen_name]
        w, h = 200, px * 2 + 12
        img = QImage(round(w * dpr), round(h * dpr), QImage.Format_ARGB32_Premultiplied)
        img.setDevicePixelRatio(dpr)
        img.fill(QColor(*bg))
        p = QPainter(img)
        p.setPen(QColor(*pen))
        p.setFont(font)
        p.drawText(QPointF(6.0 + fx, px + 6.0), TEXT_A if text_id == "a" else TEXT_B)
        p.end()
        raw = bytes(img.constBits())
        bpl, W = img.bytesPerLine(), img.width()
        out = bytearray()
        for y in range(img.height()):
            row = raw[y * bpl:y * bpl + W * 4]
            for x in range(W):
                b, g, r, a = row[x * 4:x * 4 + 4]
                out += bytes((r, g, b, a))
        print(dpr, "grey" if grey else "cleartype", px, ",".join(map(str, pen)), fx, ls,
              text_id, img.width(), img.height(), "%016x" % fnv1a64(out))


def screen_dpr():
    code = ("import sys;from PySide6.QtGui import QGuiApplication;"
            "a=QGuiApplication(['x','-platform','windows']);"
            "print(a.primaryScreen().devicePixelRatio())")
    env = dict(os.environ)
    env.pop("QT_SCALE_FACTOR", None)
    env.pop("QT_ENABLE_HIGHDPI_SCALING", None)
    return float(subprocess.run([sys.executable, "-c", code], capture_output=True, text=True,
                                env=env, check=True).stdout)


def main():
    import PySide6
    import PySide6.QtCore
    smoothing_gamma = ctypes.c_uint(0)
    ctypes.windll.user32.SystemParametersInfoW(0x200C, 0, ctypes.byref(smoothing_gamma), 0)  # contrast
    print("# Generated by gen_directwrite_placement_golden.py from PySide6 %s (Qt %s), platform "
          "windows, font smoothing contrast %d (Qt gamma %s)"
          % (PySide6.__version__, PySide6.QtCore.qVersion(), smoothing_gamma.value,
             smoothing_gamma.value / 1000.0))
    print("# dpr smoothing px pen x ls text width height fnv1a64")
    base = screen_dpr()
    for dpr in DPRS:
        for grey in (False, True):
            env = dict(os.environ)
            env.pop("QT_SCALE_FACTOR", None)
            env.pop("QT_ENABLE_HIGHDPI_SCALING", None)
            if dpr == 1.0:
                env["QT_ENABLE_HIGHDPI_SCALING"] = "0"
            elif abs(dpr - base) > 1e-9:
                env["QT_SCALE_FACTOR"] = repr(dpr / base)
            out = subprocess.run([sys.executable, __file__, "child", repr(dpr), "grey" if grey else "ct"],
                                 capture_output=True, text=True, env=env)
            if out.returncode:
                sys.stderr.write(out.stderr)
                raise SystemExit("child failed")
            sys.stdout.write(out.stdout)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "child":
        child(float(sys.argv[2]), sys.argv[3] == "grey")
    else:
        main()
