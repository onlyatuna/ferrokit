#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates the fixtures and the golden of tests/test_directwrite_modes.rs: what real Qt on
Windows lays out and draws at a device pixel ratio of 1, where `QWindowsFontEngineDirectWrite`
picks its rendering mode and its advances from the font's `maxp` and `gasp` tables.

Needs Windows, PySide6 (measured with 6.12.0, the first Qt with the `gasp` rule) and the synthetic
font tests/data/oracle_text.ttf (built by gen_text_layout_oracle.py). Run it with
`python gen_directwrite_modes_golden.py > directwrite_modes_golden.txt`; it first (re)writes the
two derived fonts next to it, then measures with Qt at a ratio of 1 (QT_ENABLE_HIGHDPI_SCALING=0).

Fixtures (all three have the same boxes and advances, see gen_text_layout_oracle.py):
* `oracle_text.ttf` (FerrokitOracleText): no `gasp`, `maxp.maxSizeOfInstructions` 0, so no
  hinting: symmetric antialiasing at every size (`!hasHinting()`).
* `oracle_text_hinted.ttf` (FerrokitOracleHinted): hinting instructions announced, no `gasp`:
  classic up to 16 px, symmetric above.
* `oracle_text_gasp.ttf` (FerrokitOracleGasp): hinting announced and a `gasp` table whose ranges
  override both defaults: up to 10 px symmetric, up to 20 px not (17 to 20 px included), symmetric
  above 20 px.

Lines (fields separated by one space):
* `adv <family> <px> <text id> <advance in 1/64 px>`: `QFontMetricsF::horizontalAdvance`.
* `glyphs <family> <px> <fnv1a64>`: the A8 bitmaps (`QRawFont.alphaMapForGlyph`) of the distinct
  characters of TEXT_A in order, each as width, height (16 bit little endian) and its bytes.
* `draw <family> <px> <width> <height> <fnv1a64>`: `QPainter.drawText` of TEXT_A with a black pen
  on white into an `ARGB32_Premultiplied` image (ClearType, as the setting of the machine says),
  hashed as RGBA bytes.
* `sysfont <file> <size> <fnv1a64>` then `sysadv`/`sysglyphs` lines of that Windows system font
  (`sysadv <file> <px> <text id> <1/64 px>`, `sysglyphs <file> <px> <fnv1a64>`): only valid for
  the font file hashed in the `sysfont` line, which the test checks before it compares.
"""
import io
import os
import subprocess
import sys

from fontTools.ttLib import TTFont, newTable

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS_DIR = os.environ.get("WINDIR", "C:/Windows") + "/Fonts"

TEXT_A = "Hello Wor-ld 0123 AVTo"
TEXT_B = "ACEFHJKMOPRTUWY"
SYS_TEXTS = {"a": "Hello, World 123 AVAWTo", "b": "The quick brown fox jumps over the lazy dog",
             "c": "WWWWW iiiii fi fl ffi 0123456789"}
SYS_GLYPH_TEXT = "Hamburgefonts 0123"
PIXEL_SIZES = range(8, 25)
SYS_GLYPH_SIZES = (8, 9, 10, 12, 13, 16, 17, 18, 19, 20, 21, 24)

# (family, file, gasp ranges or None, hinting instructions announced)
FIXTURES = [
    ("FerrokitOracleText", "oracle_text.ttf", None, False),
    ("FerrokitOracleHinted", "oracle_text_hinted.ttf", None, True),
    # rangeMaxPPEM, rangeGaspBehavior: 0x1 gridfit, 0x2 do gray, 0x8 symmetric smoothing
    ("FerrokitOracleGasp", "oracle_text_gasp.ttf", [(10, 0x8), (14, 0x3), (20, 0x3), (0xFFFF, 0xA)],
     True),
]
# file, family, weight, italic
SYSTEM_FONTS = [
    ("arial.ttf", "Arial", 400, False),
    ("arialbd.ttf", "Arial", 700, False),
    ("ariali.ttf", "Arial", 400, True),
    ("segoeui.ttf", "Segoe UI", 400, False),
    ("calibri.ttf", "Calibri", 400, False),
    ("consola.ttf", "Consolas", 400, False),
    ("tahoma.ttf", "Tahoma", 400, False),
]


def fnv1a64(data):
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def write_fixtures():
    for family, file, gasp, hinted in FIXTURES:
        if file == "oracle_text.ttf":
            continue
        font = TTFont(os.path.join(HERE, "oracle_text.ttf"))
        font["maxp"].maxSizeOfInstructions = 8 if hinted else 0
        for record in font["name"].names:
            if record.nameID in (1, 4, 16):
                record.string = family
            elif record.nameID == 6:
                record.string = family + "-Regular"
            elif record.nameID == 3:
                record.string = family + ";regular"
        if gasp:
            table = newTable("gasp")
            table.version = 1
            table.gaspRange = dict(gasp)
            font["gasp"] = table
        font["head"].created = font["head"].modified = 3_600_000_000
        buf = io.BytesIO()
        font.save(buf)
        with open(os.path.join(HERE, file), "wb") as out:
            out.write(buf.getvalue())


def child():
    from PySide6.QtCore import QByteArray, QPointF
    from PySide6.QtGui import (QColor, QFont, QFontDatabase, QFontInfo, QFontMetricsF,
                               QGuiApplication, QImage, QPainter, QRawFont)

    app = QGuiApplication(["x", "-platform", "windows"])
    assert abs(app.devicePixelRatio() - 1.0) < 1e-9, app.devicePixelRatio()
    for family, file, _, _ in FIXTURES:
        with open(os.path.join(HERE, file), "rb") as f:
            fid = QFontDatabase.addApplicationFontFromData(QByteArray(f.read()))
        assert QFontDatabase.applicationFontFamilies(fid) == [family], (file, fid)

    def distinct(text):
        seen = []
        for ch in text:
            if ch not in seen:
                seen.append(ch)
        return seen

    def alpha8(img):
        w, h = img.width(), img.height()
        bpl = img.bytesPerLine()
        raw = bytes(img.constBits())
        out = bytearray()
        for y in range(h):
            out += raw[y * bpl:y * bpl + w]
        return bytes(out)

    def glyph_hash(font, text):
        raw = QRawFont.fromFont(font)
        blob = bytearray()
        for ch in distinct(text):
            (gid,) = raw.glyphIndexesForString(ch)
            img = raw.alphaMapForGlyph(gid, QRawFont.PixelAntialiasing)
            w, h = img.width(), img.height()
            blob += w.to_bytes(2, "little") + h.to_bytes(2, "little")
            if w and h:
                blob += alpha8(img)
        return "%016x" % fnv1a64(blob)

    def draw_hash(font, px):
        w, h = 320, px * 2 + 12
        img = QImage(w, h, QImage.Format_ARGB32_Premultiplied)
        img.fill(QColor(255, 255, 255, 255))
        p = QPainter(img)
        p.setPen(QColor(0, 0, 0, 255))
        p.setFont(font)
        p.drawText(QPointF(6.0, px + 6.0), TEXT_A)
        p.end()
        raw = bytes(img.constBits())
        bpl = img.bytesPerLine()
        out = bytearray()
        for y in range(h):
            row = raw[y * bpl:y * bpl + w * 4]
            for x in range(w):
                b, g, r, a = row[x * 4:x * 4 + 4]
                out += bytes((r, g, b, a))
        return w, h, "%016x" % fnv1a64(out)

    def font_of(family, px, weight=400, italic=False):
        font = QFont(family)
        font.setPixelSize(px)
        font.setWeight(QFont.Weight(weight))
        font.setItalic(italic)
        assert QFontInfo(font).family() == family, (family, QFontInfo(font).family())
        return font

    def adv64(font, text):
        v = QFontMetricsF(font).horizontalAdvance(text) * 64
        assert v == int(v), (text, v)
        return int(v)

    for family, _, _, _ in FIXTURES:
        for px in PIXEL_SIZES:
            font = font_of(family, px)
            print("adv", family, px, "a", adv64(font, TEXT_A))
            print("adv", family, px, "b", adv64(font, TEXT_B))
            print("glyphs", family, px, glyph_hash(font, TEXT_A))
            w, h, hs = draw_hash(font, px)
            print("draw", family, px, w, h, hs)
    for file, family, weight, italic in SYSTEM_FONTS:
        path = os.path.join(FONTS_DIR, file)
        if not os.path.exists(path):
            sys.stderr.write("skipping %s: not installed\n" % file)
            continue
        with open(path, "rb") as f:
            data = f.read()
        print("sysfont", file, len(data), "%016x" % fnv1a64(data))
        for px in PIXEL_SIZES:
            font = font_of(family, px, weight, italic)
            for text_id, text in SYS_TEXTS.items():
                print("sysadv", file, px, text_id, adv64(font, text))
        for px in SYS_GLYPH_SIZES:
            print("sysglyphs", file, px, glyph_hash(font_of(family, px, weight, italic),
                                                    SYS_GLYPH_TEXT))


def main():
    import PySide6
    import PySide6.QtCore
    write_fixtures()
    print("# Generated by gen_directwrite_modes_golden.py from PySide6 %s (Qt %s), platform "
          "windows, device pixel ratio 1" % (PySide6.__version__, PySide6.QtCore.qVersion()))
    env = dict(os.environ)
    env.pop("QT_SCALE_FACTOR", None)
    env["QT_ENABLE_HIGHDPI_SCALING"] = "0"
    out = subprocess.run([sys.executable, __file__, "child"], capture_output=True, text=True,
                         env=env)
    sys.stderr.write(out.stderr)
    if out.returncode:
        raise SystemExit("child failed")
    sys.stdout.write(out.stdout)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "child":
        child()
    else:
        main()
