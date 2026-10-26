#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates qcolor_oracle.txt, the Qt measurements tests/test_qcolor.rs compares against.

Every value is read from the real QColor of PySide6 (the file header records the version), so the
Rust implementation is checked against Qt itself and not against its own reading of qcolor.cpp.

    python gen_qcolor_oracle.py > qcolor_oracle.txt

Line formats (all numbers hexadecimal; `f:` values are the bits of an IEEE-754 single):
    sweep R DIGEST                        digest over all 65536 colours with red R, see `mix` below
    hsl RRGGBB H S L OUT                  getHslF() of #RRGGBB (f: bits) and fromHslF(max(0,h),s,l).rgb()
    conv RGBA ...                         many readings of one RGBA colour, see conv_row()
    from KIND A B C D ALPHA ...           colour built from float components, see from_row()
    name HEXUTF8 VALID ARGB               QColor(name) for a UTF-8 name given in hex
"""
import random
import struct
import sys
from multiprocessing import Pool

import numpy as np
import PySide6
from PySide6.QtGui import QColor

M64 = (1 << 64) - 1


def fb(x):
    """Bits of the float32 nearest to x (QColor's getters return float32 widened to double)."""
    return struct.unpack("<I", struct.pack("<f", x))[0]


def f32(x):
    return struct.unpack("<f", struct.pack("<f", x))[0]


def mix(x):
    """splitmix64 finaliser, vectorised."""
    x = x ^ (x >> np.uint64(30))
    x = x * np.uint64(0xBF58476D1CE4E5B9)
    x = x ^ (x >> np.uint64(27))
    x = x * np.uint64(0x94D049BB133111EB)
    return x ^ (x >> np.uint64(31))


def sweep_chunk(r):
    hs, ss, ls, outs = [], [], [], []
    for g in range(256):
        for b in range(256):
            h, s, l, _ = QColor(r, g, b).getHslF()
            out = QColor.fromHslF(max(0.0, h), s, l).rgb() & 0xFFFFFF
            hs.append(fb(h))
            ss.append(fb(s))
            ls.append(fb(l))
            outs.append(out)
    idx = np.arange(65536, dtype=np.uint64)
    with np.errstate(over="ignore"):
        e = mix(idx)
        for col in (hs, ss, ls, outs):
            e = mix(e ^ np.array(col, dtype=np.uint64))
        digest = int(np.sum(e, dtype=np.uint64))
    return r, digest


def hsl_row(rgb):
    h, s, l, _ = QColor((rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255).getHslF()
    out = QColor.fromHslF(max(0.0, h), s, l).rgb() & 0xFFFFFF
    return f"hsl {rgb:06x} {fb(h):08x} {fb(s):08x} {fb(l):08x} {out:06x}"


def conv_row(r, g, b, a):
    """RGBA, then: getHsvF(3 f:), getHsl(3), getCmyk(4), getCmykF(4 f:), and the RGBA after
    fromHsvF(getHsvF) / fromCmykF(getCmykF) / fromHsl(getHsl) / fromHsv(getHsv) (-1 hue allowed),
    then lighter(150) / darker(150) / darker(250) / lighter(50) of the HSL-specified colour:
    spec, 5 raw-ish readings (getHslF f: bits), rgba."""
    c = QColor(r, g, b, a)
    hsv = c.getHsvF()
    hsl = c.getHsl()
    cmyk = c.getCmyk()
    cmykf = c.getCmykF()
    rt = [
        QColor.fromHsvF(*hsv).rgba(),
        QColor.fromCmykF(*cmykf).rgba(),
        QColor.fromHsl(*hsl).rgba(),
        QColor.fromHsv(*c.getHsv()).rgba(),
    ]
    parts = [f"conv {a:02x}{r:02x}{g:02x}{b:02x}"]
    parts += [f"{fb(v):08x}" for v in hsv[:3]]
    parts += [f"{v:x}" if v >= 0 else "-1" for v in hsl[:3]]
    parts += [f"{v:x}" for v in cmyk[:4]]
    parts += [f"{fb(v):08x}" for v in cmykf[:4]]
    parts += [f"{v:08x}" for v in rt]
    hslc = QColor.fromHslF(*c.getHslF())
    for fn in (lambda q: q.lighter(150), lambda q: q.darker(150), lambda q: q.darker(250),
               lambda q: q.lighter(50)):
        res = fn(hslc)
        h2 = res.getHslF()
        parts += [f"{res.spec().value:x}", f"{fb(h2[0]):08x}", f"{fb(h2[1]):08x}",
                  f"{fb(h2[2]):08x}", f"{res.rgba():08x}"]
    return " ".join(parts)


def from_row(kind, rng):
    """`from KIND c1 c2 c3 c4 ALPHA` (f: bits of the float arguments; c4 is 0 unless cmyk), then the
    resulting spec, rgba(), getHslF (3 f:), getHsvF (3 f:), getCmykF (4 f:)."""
    comps = [f32(rng.random()) for _ in range(4 if kind == "cmyk" else 3)]
    if kind in ("hsv", "hsl") and rng.random() < 0.1:
        comps[0] = -1.0
    if kind in ("hsv", "hsl") and rng.random() < 0.1:
        comps[0] = 1.0
    alpha = f32(rng.random())
    fn = {"hsv": QColor.fromHsvF, "hsl": QColor.fromHslF, "cmyk": QColor.fromCmykF,
          "rgb": QColor.fromRgbF}[kind]
    c = fn(*comps, alpha)
    padded = comps + [0.0] * (4 - len(comps))
    out = [f"from {kind}"] + [f"{fb(v):08x}" for v in padded] + [f"{fb(alpha):08x}"]
    out += [f"{c.spec().value:x}", f"{c.rgba():08x}"]
    out += [f"{fb(v):08x}" for v in c.getHslF()[:3]]
    out += [f"{fb(v):08x}" for v in c.getHsvF()[:3]]
    out += [f"{fb(v):08x}" for v in c.getCmykF()[:4]]
    return " ".join(out)


NAMES = [
    "#fff", "#FFF", "#abc", "#aabbcc", "#AABBCC", "#80aabbcc", "#fe112233", "#123", "#abcdef01",
    "#123456789", "#abcdefabcdef", "#abcabcabc", "#fffffffff", "#ffffffffffff", "#000000000000",
    "#1", "#12", "#1234", "#12345", "#1234567", "#1234567890", "#12345678901", "#zzzzzz", "#ggg",
    "# fff", "#fff ", " #fff", "", "#", "red", "Red", "RED", "  rEd\t", "alice blue", "Alice Blue",
    "aliceblue", "aliceblu", "aliceblue2", "transparent", "Transparent", "black", "white", "green",
    "darkgray", "dark gray", "darkgrey", "gray", "grey", "lightgoldenrodyellow", "rebeccapurple",
    "yellowgreen", "yellow green", "notacolor", "red\u4e00", "re\u4e00d", "r\u00e9d", "\u00e9", "\u4e00",
    "#ff\u4e00", "#f\u00e9f", "tomato\t", "\ttomato", "to mato", "#12\u00003", "red\x00blue",
]


def name_row(name):
    c = QColor(name)
    return f"name {name.encode('utf-8').hex() or '-'} {int(c.isValid())} {c.rgba():08x}"


def main():
    print(f"# Generated by gen_qcolor_oracle.py from PySide6 {PySide6.__version__} (Qt {PySide6.QtCore.qVersion()})")
    rng = random.Random(0x51C0102)
    # Fixed samples: every grey, the primaries and secondaries, the colours of the HUD gauges, then
    # seeded random colours.
    rgbs = [i * 0x010101 for i in range(256)]
    rgbs += [0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0x00FFFF, 0xFF00FF, 0x808000, 0x800080,
             0x008080, 0xFF8000, 0x80FF00, 0x00FF80, 0x0080FF, 0x8000FF, 0xFF0080, 0x010000,
             0x000001, 0xFEFFFF, 0x010203, 0xFE0000, 0xFEFEFF]
    rgbs += [rng.getrandbits(24) for _ in range(900)]
    for rgb in rgbs:
        print(hsl_row(rgb))
    for _ in range(400):
        print(conv_row(rng.randrange(256), rng.randrange(256), rng.randrange(256), rng.randrange(256)))
    for r, g, b in [(0, 0, 0), (255, 255, 255), (255, 0, 0), (0, 255, 0), (0, 0, 255), (1, 1, 1)]:
        print(conv_row(r, g, b, 255))
    for kind in ("hsv", "hsl", "cmyk", "rgb"):
        for _ in range(350):
            print(from_row(kind, rng))
    for name in NAMES:
        print(name_row(name))
    with Pool() as pool:
        for r, digest in sorted(pool.map(sweep_chunk, range(256))):
            print(f"sweep {r:02x} {digest:016x}")


if __name__ == "__main__":
    sys.stdout.reconfigure(newline="\n")
    main()
