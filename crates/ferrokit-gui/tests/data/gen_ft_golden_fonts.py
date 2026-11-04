#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Builds the synthetic fixture fonts of the FreeType goldens (`ft_golden.ttf`, `ft_golden.otf`).

The fonts hold a few glyphs that exercise what `SkrifaFace` and `ftgrays` have to reproduce from
FreeType: quadratic contours with implied on-curve points (odd and even coordinate sums, contours
made of off-curve points only, contours that start or end off the curve, an explicit point on the
midpoint of its neighbours, a repeated last point, zero-length segments), overlapping contours with
the overlap flag, TrueType instructions that move points along a diagonal freedom vector, a glyph
smaller than a pixel, and (OpenType/CFF) cubic curves with stem hints. `gen_ft_golden.py` then asks
FreeType and Qt what they make of them.

    python gen_ft_golden_fonts.py          # writes ft_golden.ttf and ft_golden.otf next to the script

Needs fontTools. The fonts are original work of the ferrokit contributors (LGPL-3.0-only).
"""
import array
import os
import sys

from fontTools.fontBuilder import FontBuilder
from fontTools.ttLib import newTable
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.ttLib.tables import ttProgram
from fontTools.ttLib.tables._g_l_y_f import Glyph, GlyphCoordinates

UPEM = 1024
FAMILY = "FerrokitFtGolden"
HERE = os.path.dirname(os.path.abspath(__file__))
ON, OFF = 1, 0


def tt_glyph(contours, program=None, overlap=False):
    """A simple glyph from contours of (x, y, on_curve) points."""
    coords, flags, ends = [], [], []
    for contour in contours:
        for x, y, on in contour:
            coords.append((x, y))
            flags.append(on)
        ends.append(len(coords) - 1)
    glyph = Glyph()
    glyph.numberOfContours = len(contours)
    glyph.coordinates = GlyphCoordinates(coords)
    glyph.flags = array.array("B", flags)
    glyph.endPtsOfContours = ends
    glyph.program = ttProgram.Program()
    if program:
        assembled = ttProgram.Program()
        assembled.fromAssembly(program)
        # maxp's `maxSizeOfInstructions` is computed from the bytecode form.
        glyph.program.fromBytecode(assembled.getBytecode())
    else:
        glyph.program.fromBytecode(b"")
    if overlap:
        glyph.flags[0] |= 0x40  # OVERLAP_SIMPLE
    return glyph


def rect(x0, y0, x1, y1):
    """A clockwise rectangle (TrueType outer contour)."""
    return [(x0, y0, ON), (x0, y1, ON), (x1, y1, ON), (x1, y0, ON)]


# The glyph set, shared by both fonts as far as the outline type allows: character -> name.
CHARS = {
    "o": "o",  # ring of off-curve points only, odd sums
    "s": "s",  # starts off the curve, ends on it
    "g": "g",  # starts on the curve, ends off it
    "a": "a",  # three off-curve points
    "m": "m",  # explicit point on the midpoint of its neighbours, repeated point
    "v": "v",  # last point repeats the first
    "w": "w",  # overlapping contours, overlap flag
    "x": "x",  # instructions: diagonal freedom vector
    "p": "p",  # instructions: stem rounded on the y axis
    "k": "k",  # instructions: another diagonal freedom vector
    "e": "e",  # smaller than a pixel
}

DIAGONAL = [
    [(0, 0, ON), (180, 0, ON), (700, 700, ON), (520, 700, ON)],
    [(520, 0, ON), (700, 0, ON), (180, 700, ON), (0, 700, ON)],
]

TT_OUTLINES = {
    "o": dict(
        contours=[
            [(150, 701, OFF), (451, 709, OFF), (603, 501, OFF), (599, 199, OFF),
             (449, -11, OFF), (151, -9, OFF), (-3, 201, OFF), (1, 499, OFF)],
            [(201, 499, OFF), (199, 201, OFF), (302, 101, OFF), (399, 203, OFF),
             (401, 497, OFF), (299, 599, OFF)],
        ]
    ),
    "s": dict(
        contours=[
            [(61, 41, OFF), (300, -21, ON), (521, 71, OFF), (519, 261, OFF), (301, 331, ON),
             (151, 381, OFF), (149, 521, OFF), (301, 561, ON), (481, 521, OFF), (541, 641, OFF),
             (301, 701, ON), (21, 601, OFF)],
        ]
    ),
    "g": dict(
        contours=[
            [(100, 0, ON), (500, 0, ON), (500, 500, OFF), (300, 701, OFF), (100, 501, OFF)],
            [(200, 100, ON), (200, 300, OFF), (300, 301, ON), (400, 300, OFF), (400, 100, ON)],
        ]
    ),
    "a": dict(
        contours=[
            [(0, 0, OFF), (301, 0, OFF), (150, 551, OFF)],
            [(200, 200, ON), (250, 400, ON), (300, 200, ON)],
        ]
    ),
    "m": dict(
        contours=[
            [(0, 0, ON), (0, 400, OFF), (200, 400, ON), (400, 400, OFF), (400, 0, ON), (400, 0, ON)],
        ]
    ),
    "v": dict(contours=[[(0, 700, ON), (300, 0, ON), (600, 700, ON), (0, 700, ON)]]),
    "w": dict(contours=[rect(0, 0, 400, 500), rect(200, 200, 600, 700)], overlap=True),
    "x": dict(
        contours=DIAGONAL,
        overlap=True,
        # Found by comparing skrifa's interpreter with FreeType 2.14.2 on random programs: it
        # moves points along a diagonal freedom vector, where skrifa 0.49 disagrees.
        program=["SVTCA[1]", "PUSHB[ ] 6 1", "SFVTL[1]", "PUSHB[ ] 2", "IP[ ]", "PUSHB[ ] 0",
                 "ALIGNRP[ ]", "IUP[0]", "IUP[1]"],
    ),
    "p": dict(
        contours=[rect(60, -203, 220, 700), [(220, 300, ON), (220, 600, OFF), (420, 600, ON),
                                              (420, 400, OFF), (220, 300, OFF)]],
        program=[
            "SVTCA[0]",
            "PUSHB[ ] 0",
            "MDAP[1]",
            "PUSHB[ ] 1",
            "MDRP[11100]",
            "PUSHB[ ] 4",
            "MDAP[1]",
            "PUSHB[ ] 5",
            "MDRP[11100]",
            "IUP[0]",
            "IUP[1]",
        ],
    ),
    "k": dict(
        contours=DIAGONAL,
        overlap=True,
        program=["SVTCA[1]", "PUSHB[ ] 0 6", "SFVTL[1]", "PUSHB[ ] 3", "ALIGNRP[ ]", "SFVTPV[ ]",
                 "IUP[0]", "IUP[1]"],
    ),
    "e": dict(contours=[rect(0, 0, 10, 10)]),
}

# Cubic outlines for the CFF font: character -> (hint list, path commands).
# Path commands are ("M", (x, y)), ("L", (x, y)), ("C", (x1, y1), (x2, y2), (x3, y3)).
CFF_OUTLINES = {
    "o": dict(
        hstems=[(-10, 80), (620, 80)], vstems=[(0, 90), (510, 90)],
        path=[("M", (300, 710)), ("C", (466, 710), (600, 560), (600, 350)),
              ("C", (600, 140), (466, -10), (300, -10)), ("C", (134, -10), (0, 140), (0, 350)),
              ("C", (0, 560), (134, 710), (300, 710)), ("Z",),
              ("M", (300, 620)), ("C", (190, 620), (90, 520), (90, 350)),
              ("C", (90, 180), (190, 80), (300, 80)), ("C", (410, 80), (510, 180), (510, 350)),
              ("C", (510, 520), (410, 620), (300, 620)), ("Z",)],
    ),
    "s": dict(
        hstems=[(-10, 80), (320, 70), (630, 80)], vstems=[(40, 80)],
        path=[("M", (61, 41)), ("C", (150, -30), (400, -30), (521, 71)),
              ("C", (600, 160), (560, 260), (400, 331)), ("C", (250, 400), (150, 440), (149, 521)),
              ("C", (150, 620), (400, 650), (541, 641)), ("L", (541, 701)),
              ("C", (400, 720), (100, 700), (21, 601)), ("C", (60, 500), (200, 460), (301, 400)),
              ("C", (480, 330), (480, 240), (400, 161)), ("C", (300, 60), (150, 80), (61, 101)),
              ("Z",)],
    ),
    "g": dict(
        hstems=[(0, 80)], vstems=[(100, 90)],
        path=[("M", (100, 0)), ("L", (500, 0)), ("C", (500, 300), (450, 500), (300, 701)),
              ("L", (100, 501)), ("Z",)],
    ),
    "w": dict(
        hstems=[(0, 100), (600, 100)], vstems=[(0, 100), (500, 100)],
        path=[("M", (0, 0)), ("L", (0, 500)), ("L", (400, 500)), ("L", (400, 0)), ("Z",),
              ("M", (200, 200)), ("L", (200, 700)), ("L", (600, 700)), ("L", (600, 200)), ("Z",)],
    ),
    "x": dict(
        hstems=[], vstems=[],
        path=[("M", (0, 0)), ("L", (180, 0)), ("L", (700, 700)), ("L", (520, 700)), ("Z",),
              ("M", (520, 0)), ("L", (700, 0)), ("L", (180, 700)), ("L", (0, 700)), ("Z",)],
    ),
    "e": dict(hstems=[], vstems=[], path=[("M", (0, 0)), ("L", (0, 10)), ("L", (10, 10)),
                                          ("L", (10, 0)), ("Z",)]),
}


def setup_common(fb, names, advances, family):
    fb.setupGlyphOrder(names)
    cmap = {32: "space"}
    for ch, name in CHARS.items():
        if name in names:
            cmap[ord(ch)] = name
    fb.setupCharacterMap(cmap)
    fb.setupHorizontalMetrics({n: (advances[n], 0) for n in names})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable(
        {
            "familyName": family,
            "styleName": "Regular",
            "copyright": "Original work of the ferrokit contributors, LGPL-3.0-only",
        }
    )
    fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200,
                sxHeight=500, sCapHeight=700)
    fb.setupPost()


def build_ttf(path):
    names = [".notdef", "space"] + list(TT_OUTLINES)
    advances = {n: 600 for n in names}
    advances["space"] = 300
    fb = FontBuilder(UPEM, isTTF=True)
    glyphs = {".notdef": tt_glyph([rect(50, 0, 550, 700)]), "space": tt_glyph([])}
    for name, spec in TT_OUTLINES.items():
        glyphs[name] = tt_glyph(spec["contours"], spec.get("program"), spec.get("overlap", False))
    setup_common(fb, names, advances, FAMILY)
    fb.setupGlyf(glyphs)
    # fontTools does not count glyph programs in maxp. FreeType (and skrifa) auto-hint a TrueType
    # font without `fpgm` and with `maxSizeOfInstructions == 0` instead of running its bytecode, so
    # the limits are set by hand and kept from being recomputed.
    maxp = fb.font["maxp"]
    maxp.recalc(fb.font)
    maxp.maxSizeOfInstructions = max(len(g.program.getBytecode()) for g in glyphs.values())
    maxp.maxStackElements = 16
    maxp.maxFunctionDefs = 1
    # A font program that is not empty sends the font to the TrueType interpreter in normal mode;
    # FreeType 2.14 auto-hints a font without `fpgm` and with a `prep` of at most 7 bytes.
    fpgm = newTable("fpgm")
    fpgm.program = ttProgram.Program()
    fpgm.program.fromAssembly(["PUSHB[ ] 0", "FDEF[ ]", "ENDF[ ]"])
    fb.font["fpgm"] = fpgm
    fb.font.recalcBBoxes = False
    fb.save(path)


def build_otf(path):
    names = [".notdef", "space"] + list(CFF_OUTLINES)
    advances = {n: 600 for n in names}
    advances["space"] = 300
    fb = FontBuilder(UPEM, isTTF=False)
    charstrings = {}
    for name in names:
        pen = T2CharStringPen(advances[name], None)
        if name in CFF_OUTLINES:
            for cmd in CFF_OUTLINES[name]["path"]:
                if cmd[0] == "M":
                    pen.moveTo(cmd[1])
                elif cmd[0] == "L":
                    pen.lineTo(cmd[1])
                elif cmd[0] == "C":
                    pen.curveTo(*cmd[1:])
                else:
                    pen.closePath()
        elif name == ".notdef":
            for i, (x, y, _) in enumerate(rect(50, 0, 550, 700)):
                (pen.moveTo if i == 0 else pen.lineTo)((x, y))
            pen.closePath()
        cs = pen.getCharString()
        spec = CFF_OUTLINES.get(name)
        if spec and (spec["hstems"] or spec["vstems"]):
            def stems(pairs):
                out, prev = [], 0
                for edge, width in sorted(pairs):
                    out += [edge - prev, width]
                    prev = edge + width
                return out
            hints = []
            if spec["hstems"]:
                hints += stems(spec["hstems"]) + ["hstem"]
            if spec["vstems"]:
                hints += stems(spec["vstems"]) + ["vstem"]
            # The width, when the glyph has one, is the first operand of the first operator.
            prog = list(cs.program)
            cs.program = prog[:1] + hints + prog[1:]
        charstrings[name] = cs
    setup_common(fb, names, advances, FAMILY + "Cff")
    fb.setupCFF(FAMILY + "Cff-Regular", {"FullName": FAMILY + "Cff Regular", "FamilyName": FAMILY + "Cff"},
                charstrings, {})
    # Hints (stem widths) go through the private dict: StdHW/StdVW, as real CFF fonts have them.
    cff = fb.font["CFF "].cff
    top = cff[cff.fontNames[0]]
    priv = top.Private
    priv.StdHW = 80
    priv.StdVW = 90
    priv.BlueValues = [-10, 0, 700, 710]
    priv.OtherBlues = []
    priv.StemSnapH = [80]
    priv.StemSnapV = [90]
    fb.save(path)


if __name__ == "__main__":
    build_ttf(os.path.join(HERE, "ft_golden.ttf"))
    if "--no-otf" not in sys.argv:
        build_otf(os.path.join(HERE, "ft_golden.otf"))
