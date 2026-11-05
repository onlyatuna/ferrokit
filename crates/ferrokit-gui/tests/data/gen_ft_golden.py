#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates ft_golden.txt and qt_golden.txt, the FreeType and Qt measurements that
tests/test_ft_golden.rs compares the Linux glyph backend (`SkrifaFace`, `ftgrays`) against.

    python gen_ft_golden_fonts.py                            # the fixture fonts, once
    PYTHONPATH=<fontTools>:<freetype-py on FreeType 2.14.2> python gen_ft_golden.py ft  > ft_golden.txt
    QT_QPA_PLATFORM=offscreen python gen_ft_golden.py qt  > qt_golden.txt
    PYTHONPATH=<freetype-py> python gen_ft_golden.py outline        # ftgrays unit-test values (stdout)

`ft` needs freetype-py bound to the system FreeType 2.14.2 (the release Qt 6.12 links in the Linux
reference setup; the script refuses to run against another version) and loads every glyph with Qt's
flags (`FT_LOAD_IGNORE_GLOBAL_ADVANCE_WIDTH | FT_LOAD_NO_BITMAP`, plus the target of the hint mode),
sets the 26.6 sub-pixel translation Qt's raster engine uses with `FT_Set_Transform`, and renders with
the mode Qt picks. CFF stem darkening is switched on, as Qt does in `qt_getFreetypeData`.
`qt` needs PySide6 (any 6.x on FreeType 2.14.2) with fontconfig `hintstyle=hintslight`, the
default of the Linux reference setup, which makes the default hinting preference mean light.

Record formats (fields separated by one space; a field that is empty is `-`):
    ft_golden.txt
        R FONT CP SIZE MODE DELTA ADVANCE LINEAR FLAGS CONTOURS POINTS BITMAP
            FONT: ft_golden.ttf, ft_golden.otf or oracle_text.ttf; CP: hexadecimal code point;
            SIZE: pixels per em; MODE: 0 FT_LOAD_TARGET_NORMAL, 1 FT_LOAD_TARGET_LIGHT,
            2 FT_LOAD_NO_HINTING; DELTA: sub-pixel x offset in 64ths; ADVANCE: `advance.x` (26.6);
            LINEAR: `linearHoriAdvance` (16.16);
            FLAGS: outline flags in hex; CONTOURS: end points, comma separated;
            POINTS: `x:y:tag` (26.6, tag as FreeType stores it), comma separated;
            BITMAP: `left:top:width:rows:DATA`, DATA the hexadecimal coverage, or `h<16 hex digits>`
            (FNV-1a 64 of the coverage) for the larger sizes, or `-` when the glyph has no bitmap.
    qt_golden.txt
        A FONT CP SIZE PREF GID W H LEFT TOP ADVANCE DATA
            `QRawFont.alphaMapForGlyph` and `boundingRect`; PREF: d default, n none, f full hinting.
        S ID FONT SIZE SPACING PEN BG X Y W H TEXT IMAGE
            `QPainter.drawText(QPointF(X, Y), text)` into a W x H `Format_ARGB32_Premultiplied` image
            filled with BG (RRGGBB) with pen PEN (RRGGBBAA), letter spacing SPACING (pixels);
            TEXT: hexadecimal UTF-8; IMAGE: premultiplied RGBA bytes run-length encoded as
            `RRGGBBAA*count` runs, comma separated.
"""
import ctypes
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = ["ft_golden.ttf", "ft_golden.otf", "oracle_text.ttf", "ft_golden_accents.ttf"]
CHARS = {
    "ft_golden.ttf": "osgamvwxpke ",
    "ft_golden.otf": "osgwxe ",
    "oracle_text.ttf": "AHVe. ",
    # The glyphs of the vertical separation adjustment (afadjust.c): the dots of i and j, the dot
    # of ! and ? (no height check), accents above (acute, circumflex, dieresis, ring, tilde on
    # capitals and on small letters), the cedilla below with the ignored bottom zone, and the
    # strokes of the slashed o that are ignored in both zones.
    "ft_golden_accents.ttf": "ij!?\u00aa\u00e9\u00f1\u00d1\u00c7\u00e7\u00d8\u00f8\u00c4\u00fc\u00ff\u00c3",
}
# The accents font is small (a few sizes, the two modes that auto-hint it, no sub-pixel offset).
FONT_SIZES = {"ft_golden_accents.ttf": [9, 12, 16]}
FONT_MODES = {"ft_golden_accents.ttf": [0, 1]}
SIZES = [9, 12, 13, 16, 24, 37]
FULL_BITMAP_MAX_SIZE = 16
LOAD_BASE = 0x200 | 0x8  # FT_LOAD_IGNORE_GLOBAL_ADVANCE_WIDTH | FT_LOAD_NO_BITMAP
MODES = {0: (0x0, 0), 1: (1 << 16, 1), 2: (0x2, 0)}  # load flags, FT_Render_Mode
DELTAS = {0: [0], 1: [0, 32], 2: [0, 32]}


def selected_fonts(default=FONTS):
    """The fonts named on the command line after the mode (all of `default` without any)."""
    return [f for f in default if f in sys.argv[2:]] if sys.argv[2:] else default


def fnv1a(data):
    h = 0xCBF29CE484222325
    for b in data:
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def ft_records():
    import freetype
    from freetype import raw

    if freetype.version() != (2, 14, 2):
        sys.exit(f"freetype-py is bound to FreeType {freetype.version()}, 2.14.2 is required")
    lib = ctypes.CDLL("libfreetype.so.6")
    print("# FreeType %d.%d.%d" % freetype.version() + ", Qt's load flags, see gen_ft_golden.py")
    for font in selected_fonts():
        if font.endswith(".otf"):
            # Qt: FT_Property_Set(library, "cff", "no-stem-darkening", false), before any face exists
            off = ctypes.c_ubyte(0)
            assert lib.FT_Property_Set(freetype.get_handle(), b"cff", b"no-stem-darkening", ctypes.byref(off)) == 0
        face = freetype.Face(os.path.join(HERE, font))
        matrix = freetype.FT_Matrix(0x10000, 0, 0, 0x10000)
        for size in FONT_SIZES.get(font, SIZES):
            face.set_char_size(size * 64, size * 64, 0, 0)
            for mode, (flags, render_mode) in MODES.items():
                if mode not in FONT_MODES.get(font, MODES):
                    continue
                for delta in DELTAS[mode] if font not in FONT_MODES else [0]:
                    vec = freetype.FT_Vector(delta, 0)
                    raw.FT_Set_Transform(face._FT_Face, ctypes.byref(matrix), ctypes.byref(vec))
                    for ch in CHARS[font]:
                        gid = face.get_char_index(ord(ch))
                        if gid == 0:
                            continue
                        assert raw.FT_Load_Glyph(face._FT_Face, gid, LOAD_BASE | flags) == 0
                        slot = face._FT_Face.contents.glyph.contents
                        o = slot.outline
                        conts = [o.contours[i] for i in range(o.n_contours)]
                        pts = ["%d:%d:%d" % (o.points[i].x, o.points[i].y, o.tags[i]) for i in range(o.n_points)]
                        adv = slot.advance.x
                        lin = slot.linearHoriAdvance  # 16.16
                        oflags = o.flags
                        assert raw.FT_Render_Glyph(ctypes.byref(slot), render_mode) == 0
                        bm = slot.bitmap
                        data = b""
                        if bm.width and bm.rows:
                            buf = ctypes.string_at(bm.buffer, bm.rows * abs(bm.pitch))
                            data = b"".join(buf[y * bm.pitch: y * bm.pitch + bm.width] for y in range(bm.rows))
                        if not data:
                            bitmap = "-"
                        else:
                            body = data.hex() if size <= FULL_BITMAP_MAX_SIZE else "h%016x" % fnv1a(data)
                            bitmap = "%d:%d:%d:%d:%s" % (slot.bitmap_left, slot.bitmap_top, bm.width, bm.rows, body)
                        print("R %s %x %d %d %d %d %d %x %s %s %s" % (
                            font, ord(ch), size, mode, delta, adv, lin, oflags,
                            ",".join(map(str, conts)) or "-", ",".join(pts) or "-", bitmap))


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


def qt_records():
    import PySide6
    from PySide6.QtCore import QPointF
    from PySide6.QtGui import (QColor, QFont, QFontDatabase, QGuiApplication, QImage, QPainter, QPen,
                               QRawFont)

    app = QGuiApplication(sys.argv)
    print("# PySide6 %s, offscreen, fontconfig hintslight; see gen_ft_golden.py" % PySide6.__version__)
    prefs = {"d": QFont.PreferDefaultHinting, "n": QFont.PreferNoHinting, "f": QFont.PreferFullHinting}
    for font in selected_fonts(["ft_golden.ttf", "ft_golden.otf", "ft_golden_accents.ttf"]):
        path = os.path.join(HERE, font)
        for size in FONT_SIZES.get(font, [9, 12, 16, 24]):
            for pname, pref in prefs.items():
                if font in FONT_MODES and pname == "n":
                    continue  # unhinted: nothing to learn from the font
                raw = QRawFont(path, size, pref)
                assert raw.isValid()
                for ch in CHARS[font]:
                    gi = raw.glyphIndexesForString(ch)[0]
                    if gi == 0:
                        continue
                    img = raw.alphaMapForGlyph(gi, QRawFont.PixelAntialiasing)
                    w, h, bpl = img.width(), img.height(), img.bytesPerLine()
                    buf = bytes(img.constBits()) if w and h else b""
                    data = b"".join(buf[y * bpl: y * bpl + w] for y in range(h)) if w and h else b""
                    br = raw.boundingRect(gi)
                    adv = raw.advancesForGlyphIndexes([gi])[0].x()
                    print("A %s %x %d %s %d %d %d %d %d %r %s" % (
                        font, ord(ch), size, pname, gi, w, h, round(br.x()), round(-br.y()), adv, data.hex() or "-"))
    fonts = selected_fonts(["ft_golden.ttf", "ft_golden.otf", "ft_golden_accents.ttf"])
    fid = {f: QFontDatabase.addApplicationFont(os.path.join(HERE, f)) for f in fonts}
    assert all(i >= 0 for i in fid.values())
    cases = [
        # font, size, spacing, pen, bg, x, y, text
        ("ft_golden.ttf", 16, 0.0, "000000ff", "ffffff", 4.0, 22.0, "owsgampvxe wow"),
        ("ft_golden.ttf", 16, 0.0, "000000ff", "ffffff", 4.25, 22.0, "owsgampvxe wow"),
        ("ft_golden.ttf", 16, 0.0, "000000ff", "ffffff", 4.5, 22.0, "owsgampvxe wow"),
        ("ft_golden.ttf", 16, 0.0, "000000ff", "ffffff", 4.75, 22.4, "owsgampvxe wow"),
        ("ft_golden.ttf", 12, 0.0, "1e78c8ff", "ffffff", 3.3, 17.0, "sgam xpvw"),
        ("ft_golden.ttf", 13, 1.5, "ffffffff", "141414", 3.0, 18.0, "owsg pxe"),
        ("ft_golden.ttf", 14, -4.0, "c82828ff", "f0e678", 6.5, 20.0, "wwxx ooww"),
        ("ft_golden.ttf", 20, 0.0, "1e78c864", "ffffff", 5.0, 26.0, "ogxw"),
        ("ft_golden.ttf", 9, 0.0, "000000ff", "ffffff", 2.5, 14.0, "owsgampvxe"),
        ("ft_golden.otf", 16, 0.0, "000000ff", "ffffff", 4.25, 22.0, "osgwxe wo"),
        ("ft_golden.otf", 12, 0.0, "1e78c8ff", "ffffff", 3.0, 17.0, "osgw xe"),
        ("ft_golden.otf", 14, -3.0, "c82828c8", "f0e678", 6.5, 20.0, "wwxx oo"),
        # accents: whole strings with the adjustments, at whole and fractional pen positions
        ("ft_golden_accents.ttf", 12, 0.0, "000000ff", "ffffff", 3.0, 17.0, "i\u00e9\u00f1\u00c7\u00e7\u00d8j\u00c4!?"),
        ("ft_golden_accents.ttf", 9, 0.0, "000000ff", "ffffff", 2.5, 14.0, "\u00ff\u00fc\u00c3\u00aa\u00f8ij"),
        ("ft_golden_accents.ttf", 16, 0.0, "000000ff", "ffffff", 4.25, 22.0, "\u00c4\u00e9\u00d1\u00e7\u00ff"),
    ]
    cases = [(i, c) for i, c in enumerate(cases) if c[0] in fonts]
    for i, (font, size, spacing, pen, bg, x, y, text) in cases:
        family = QFontDatabase.applicationFontFamilies(fid[font])[0]
        img = QImage(120, 30, QImage.Format_ARGB32_Premultiplied)
        img.fill(QColor("#" + bg))
        p = QPainter(img)
        p.setRenderHint(QPainter.TextAntialiasing, True)
        f = QFont(family)
        f.setPixelSize(size)
        if spacing:
            f.setLetterSpacing(QFont.AbsoluteSpacing, spacing)
        p.setFont(f)
        p.setPen(QPen(QColor(int(pen[0:2], 16), int(pen[2:4], 16), int(pen[4:6], 16), int(pen[6:8], 16))))
        p.drawText(QPointF(x, y), text)
        p.end()
        print("S %d %s %d %r %s %s %r %r %d %d %s %s" % (
            i, font, size, spacing, pen, bg, x, y, 120, 30, text.encode().hex(), rle(img)))


def outline_values():
    """Bitmaps of hand-made outlines from FT_Outline_Get_Bitmap, for the ftgrays unit tests."""
    import freetype
    from freetype import raw

    if freetype.version() != (2, 14, 2):
        sys.exit("FreeType 2.14.2 is required")
    outlines = {
        "square_3px": ([(0, 0), (0, 192), (192, 192), (192, 0)], [1, 1, 1, 1], [3], 0),
        "square_offset": ([(32, 32), (32, 224), (224, 224), (224, 32)], [1, 1, 1, 1], [3], 0),
        "triangle": ([(0, 0), (160, 320), (320, 0)], [1, 1, 1], [2], 0),
        "conic": ([(0, 0), (128, 384), (256, 0)], [1, 0, 1], [2], 0),
        "conic_ring": ([(0, 65), (130, 321), (257, 65), (131, -191)], [0, 0, 0, 0], [3], 0),
        "cubic": ([(0, 0), (0, 400), (300, 400), (300, 0)], [1, 2, 2, 1], [3], 0),
        "even_odd": (
            [(0, 0), (0, 384), (384, 384), (384, 0), (96, 96), (96, 288), (288, 288), (288, 96)],
            [1] * 8, [3, 7], 2,
        ),
    }
    lib = freetype.get_handle()
    for name, (pts, tags, conts, flags) in outlines.items():
        n = len(pts)
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        # FT_Render_Glyph's box: floor of the control box's minimum, ceiling of its maximum, in pixels.
        x0, y0 = min(xs) >> 6, min(ys) >> 6
        x1, y1 = -((-max(xs)) >> 6), -((-max(ys)) >> 6)
        w, h = x1 - x0, y1 - y0
        vec = (raw.FT_Vector * n)(*[raw.FT_Vector(x - x0 * 64, y - y0 * 64) for x, y in pts])
        tg = (ctypes.c_ubyte * n)(*tags)
        ct = (ctypes.c_short * len(conts))(*conts)
        outline = raw.FT_Outline(len(conts), n, ctypes.cast(vec, ctypes.POINTER(raw.FT_Vector)),
                                 ctypes.cast(tg, ctypes.POINTER(ctypes.c_ubyte)),
                                 ctypes.cast(ct, ctypes.POINTER(ctypes.c_short)), flags)
        buf = (ctypes.c_ubyte * (w * h))()
        bm = raw.FT_Bitmap()
        bm.rows, bm.width, bm.pitch = h, w, w
        bm.buffer = ctypes.cast(buf, ctypes.POINTER(ctypes.c_ubyte))
        bm.num_grays, bm.pixel_mode = 256, raw.FT_PIXEL_MODE_GRAY
        assert raw.FT_Outline_Get_Bitmap(lib, ctypes.byref(outline), ctypes.byref(bm)) == 0
        print("%s: left %d top %d width %d rows %d" % (name, x0, y1, w, h))
        print("    ", [bytes(buf[r * w:(r + 1) * w]).hex() for r in range(h)])


if __name__ == "__main__":
    {"ft": ft_records, "qt": qt_records, "outline": outline_values}[sys.argv[1]]()
