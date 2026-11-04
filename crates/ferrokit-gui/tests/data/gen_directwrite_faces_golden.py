#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates the golden of tests/test_directwrite_faces.rs: what real Qt on Windows lays out and
draws for faces that need DirectWrite's font simulations or that are a member of a font
collection, at device pixel ratios 1 and 1.25.

Needs Windows and PySide6 (measured with 6.12.0) and the synthetic font tests/data/oracle_text.ttf.
Run it with `python gen_directwrite_faces_golden.py > directwrite_faces_golden.txt`.

Qt (qwindowsdirectwritefontdatabase.cpp `fontEngine`) creates the face of a request with
`DWRITE_FONT_SIMULATIONS_BOLD` when the requested weight is at least DemiBold and the face is
lighter, and with `DWRITE_FONT_SIMULATIONS_OBLIQUE` when the request is italic and the face is
upright; the font family of a request is looked up among all faces of all font files, so the
member of a collection (`.ttc`) is the face of its own name.

Lines (fields separated by one space; the fonts are `family` with `_` for a space):
* `fix <dpr> <weight> <italic> <px> adv <text id> <1/64 px>` (`QFontMetricsF::horizontalAdvance`),
  `fix ... avg <1/64 px>` (`averageCharWidth`), `fix ... glyphs latin|cjk <fnv1a64>` (the A8
  bitmaps, `QRawFont.alphaMapForGlyph`, of the distinct characters of GLYPH_TEXT or, for the
  application font, FIXTURE_GLYPH_TEXT, followed by
  CJK_GLYPH_TEXT for `cjk`: each as width and height as 16 bit little endian and its bytes), `fix ... draw <text id> <width> <height> <fnv1a64>`
  (`QPainter.drawText` of the text with a black pen on white into an `ARGB32_Premultiplied` image,
  ClearType as the setting of the machine says, hashed as RGBA bytes): the application font
  FerrokitOracleText (tests/data/oracle_text.ttf, added with `addApplicationFontFromData`), which
  has one upright regular face, so a bold or italic request is synthesized.
* `sysfont <file> <size> <fnv1a64>` then `sys <dpr> <family> <weight> <italic> <px> ...` lines of
  the Windows system font request whose face is in that file (`adv`, `avg`, `glyphs`, `draw` as
  above): only valid for the font file hashed in the `sysfont` line, which the test checks first.
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS_DIR = os.environ.get("WINDIR", "C:/Windows") + "/Fonts"

# `fa` and `fb`: the characters FerrokitOracleText has glyphs for (anything else would be drawn
# with another font, which is not what these lines are about).
TEXTS = {"a": "Hello, World 123 AVAWTo", "b": "The quick brown fox jumps over the lazy dog",
         "c": "日本語テキスト中文字 ABC123", "fa": "Hello Wor-ld 0123 AVTo", "fb": "ACEFHJKMOPRTUWY"}
GLYPH_TEXT = "Hamburgefonts 0123"
FIXTURE_GLYPH_TEXT = "Hello Wor-ld 0123 AVTo"
CJK_GLYPH_TEXT = "日本語中文字テキスト"
SIZES = {1.0: (8, 9, 12, 13, 16, 17, 20, 24), 1.25: (9, 13, 17, 24)}
DPRS = (1.0, 1.25)

# (family, weight, italic) of the application font FerrokitOracleText
FIXTURE_REQUESTS = [(700, False), (400, True), (700, True)]
# file -> [(family, weight, italic, cjk)]
SYSTEM_REQUESTS = {
    "tahoma.ttf": [("Tahoma", 400, True, False)],
    "tahomabd.ttf": [("Tahoma", 700, True, False)],
    "impact.ttf": [("Impact", 700, False, False), ("Impact", 400, True, False)],
    "msgothic.ttc": [("MS Gothic", 400, False, True), ("MS Gothic", 700, False, True),
                     ("MS Gothic", 400, True, True), ("MS Gothic", 700, True, True),
                     ("MS UI Gothic", 400, False, True), ("MS PGothic", 400, False, True)],
    "simsun.ttc": [("SimSun", 700, False, True), ("NSimSun", 400, False, True),
                   ("NSimSun", 700, False, True)],
    "mingliu.ttc": [("MingLiU", 700, False, True), ("PMingLiU", 400, False, True)],
    "YuGothM.ttc": [("Yu Gothic UI", 400, False, True)],
    "YuGothB.ttc": [("Yu Gothic UI", 700, False, True)],
    "msjh.ttc": [("Microsoft JhengHei UI", 400, False, True)],
    "msjhbd.ttc": [("Microsoft JhengHei UI", 700, False, True)],
    "msyh.ttc": [("Microsoft YaHei UI", 400, False, True)],
    "msyhbd.ttc": [("Microsoft YaHei UI", 700, False, True)],
}


def fnv1a64(data):
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def child(dpr):
    from PySide6.QtCore import QByteArray, QPointF
    from PySide6.QtGui import (QColor, QFont, QFontDatabase, QFontInfo, QFontMetricsF,
                               QGuiApplication, QImage, QPainter, QRawFont, QTransform)

    app = QGuiApplication(["x", "-platform", "windows"])
    assert abs(app.devicePixelRatio() - dpr) < 1e-9, (app.devicePixelRatio(), dpr)
    fixture = "FerrokitOracleText"
    with open(os.path.join(HERE, "oracle_text.ttf"), "rb") as f:
        fid = QFontDatabase.addApplicationFontFromData(QByteArray(f.read()))
    assert QFontDatabase.applicationFontFamilies(fid) == [fixture], fid

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
        transform = QTransform().scale(dpr, dpr)
        blob = bytearray()
        seen = []
        for ch in text:
            if ch in seen:
                continue
            seen.append(ch)
            (gid,) = raw.glyphIndexesForString(ch)
            img = raw.alphaMapForGlyph(gid, QRawFont.PixelAntialiasing, transform)
            w, h = img.width(), img.height()
            blob += w.to_bytes(2, "little") + h.to_bytes(2, "little")
            if w and h:
                blob += alpha8(img)
        return "%016x" % fnv1a64(blob)

    def draw_hash(font, px, text):
        w, h = 320, px * 2 + 12
        img = QImage(round(w * dpr), round(h * dpr), QImage.Format_ARGB32_Premultiplied)
        img.setDevicePixelRatio(dpr)
        img.fill(QColor(255, 255, 255, 255))
        p = QPainter(img)
        p.setPen(QColor(0, 0, 0, 255))
        p.setFont(font)
        p.drawText(QPointF(6.0, px + 6.0), text)
        p.end()
        raw = bytes(img.constBits())
        bpl = img.bytesPerLine()
        out = bytearray()
        for y in range(img.height()):
            row = raw[y * bpl:y * bpl + img.width() * 4]
            for x in range(img.width()):
                b, g, r, a = row[x * 4:x * 4 + 4]
                out += bytes((r, g, b, a))
        return img.width(), img.height(), "%016x" % fnv1a64(out)

    def font_of(family, px, weight, italic):
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

    def measure(prefix, family, weight, italic, cjk):
        for px in SIZES[dpr]:
            font = font_of(family, px, weight, italic)
            head = "%s %s %d %d %d" % (prefix, repr(dpr), weight, int(italic), px)
            if prefix == "sys":
                head = "sys %s %s %d %d %d" % (repr(dpr), family.replace(" ", "_"), weight,
                                              int(italic), px)
            first, second = ("fa", "fb") if prefix == "fix" else ("a", "c" if cjk else "b")
            for text_id in (first, second):
                print(head, "adv", text_id, adv64(font, TEXTS[text_id]))
            v = QFontMetricsF(font).averageCharWidth() * 64
            assert v == int(v)
            print(head, "avg", int(v))
            glyph_text = FIXTURE_GLYPH_TEXT if prefix == "fix" else GLYPH_TEXT
            print(head, "glyphs", "cjk" if cjk else "latin",
                  glyph_hash(font, glyph_text + (CJK_GLYPH_TEXT if cjk else "")))
            for text_id in (first, second):
                w, h, hs = draw_hash(font, px, TEXTS[text_id])
                print(head, "draw", text_id, w, h, hs)

    if dpr == 1.0:
        print("# Generated by gen_directwrite_faces_golden.py from PySide6 %s (Qt %s), platform "
              "windows" % (__import__("PySide6").__version__,
                           __import__("PySide6.QtCore").QtCore.qVersion()))
    for weight, italic in FIXTURE_REQUESTS:
        measure("fix", fixture, weight, italic, False)
    for file, requests in SYSTEM_REQUESTS.items():
        path = os.path.join(FONTS_DIR, file)
        if not os.path.exists(path):
            sys.stderr.write("skipping %s: not installed\n" % file)
            continue
        with open(path, "rb") as f:
            data = f.read()
        print("sysfont", file, len(data), "%016x" % fnv1a64(data))
        for family, weight, italic, cjk in requests:
            measure("sys", family, weight, italic, cjk)


def screen_dpr():
    env = dict(os.environ)
    env.pop("QT_SCALE_FACTOR", None)
    env.pop("QT_ENABLE_HIGHDPI_SCALING", None)
    return float(subprocess.run(
        [sys.executable, "-c",
         "from PySide6.QtGui import QGuiApplication; import sys; "
         "a = QGuiApplication(sys.argv[:1]); print(a.devicePixelRatio())"],
        capture_output=True, text=True, env=env, check=True).stdout)


def main():
    base = screen_dpr()
    chunks = []
    for dpr in DPRS:
        env = dict(os.environ)
        env.pop("QT_SCALE_FACTOR", None)
        env.pop("QT_ENABLE_HIGHDPI_SCALING", None)
        if dpr == 1.0:
            env["QT_ENABLE_HIGHDPI_SCALING"] = "0"
        elif abs(dpr - base) > 1e-9:
            env["QT_SCALE_FACTOR"] = repr(dpr / base)
        out = subprocess.run([sys.executable, __file__, "child", repr(dpr)], capture_output=True,
                             text=True, env=env, encoding="utf-8")
        sys.stderr.write(out.stderr)
        if out.returncode:
            raise SystemExit("child failed")
        chunks.append(out.stdout)
    # Every child prints its own `sysfont` headers; the test applies the header above a line.
    sys.stdout.reconfigure(encoding="utf-8", newline="\n")
    for chunk in chunks:
        sys.stdout.write(chunk)


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "child":
        sys.stdout.reconfigure(encoding="utf-8", newline="\n")
        child(float(sys.argv[2]))
    else:
        main()
