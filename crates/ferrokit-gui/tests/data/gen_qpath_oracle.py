#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates qpath_oracle.txt, the Qt measurements tests/test_qpath_stroker.rs compares against.

Every value is read from the real QPainterPath / QPainterPathStroker of PySide6 (the file header
records the version), so the Rust implementation is checked against Qt itself.

    python gen_qpath_oracle.py > qpath_oracle.txt

All numbers are the 16 hexadecimal digits of an IEEE-754 double (`qreal`); the inputs are chosen
exactly representable as `f32`, because the Rust path stores `f32`. Counts and types are decimal.
Line formats:
    arc MODE X Y W H START SWEEP N (T X Y)*
        MODE 0: QPainterPath().arcTo(rect, start, sweep)
        MODE 1: arcMoveTo(rect, start) then arcTo(rect, start, sweep)
        MODE 2: moveTo(1.5, 2.5), lineTo(3, 4) then arcTo
        T is QPainterPath.ElementType (0 move, 1 line, 2 curve, 3 curve data)
    stroke WIDTH CAP JOIN MITER DASHOFFSET CURVETHRESHOLD ND D* NOPS OP* N (T X Y)*
        CAP 0 flat, 1 square, 2 round; JOIN 0 miter, 1 bevel, 2 round, 3 svg miter;
        CURVETHRESHOLD 0 keeps the default; OP is `M x y`, `L x y`, `Q cx cy x y`,
        `C c1x c1y c2x c2y x y` or `Z`; the result is createStroke() of that path
    hud X Y W H START SWEEP WIDTH N CPX CPY CPW CPH AREA PX PY PW PH
        the HUD dial's hatch stroke: arcMoveTo + arcTo, flat cap, width WIDTH; CP* is
        controlPointRect(), AREA the shoelace area and P* the bounds of toFillPolygon()
    width REQUESTED STORED THRESHOLD
        setWidth(REQUESTED): width() and curveThreshold() afterwards
    wedge W H CX CY X Y SIDE START SWEEP ALPHA
        QPainter.fillPath of QPainterPath(QPointF(CX, CY)).arcTo(QRectF(X, Y, SIDE, SIDE), START,
        SWEEP) + closeSubpath() with a white brush and Antialiasing, as ALPHA below
    ringfill W H X Y SIDE START SWEEP RING ALPHA
        QPainter.fillPath of createStroke(arcMoveTo + arcTo) of a flat-cap stroker of width RING
        (the HUD's hatched stretch of the outer ring), white brush, Antialiasing
    poly W H RULE PENWIDTH BRUSH N (X Y)* ALPHA
        QPainter.drawPolygon on a transparent W x H ARGB32_Premultiplied image with
        Antialiasing, white brush (BRUSH 1) or none, white pen of width PENWIDTH (0 = NoPen) and
        fill rule RULE (0 odd-even, 1 winding); ALPHA is the W*H alpha bytes in hex, row by row
"""
import math
import random
import struct
import sys

import PySide6
from PySide6.QtCore import QPointF, QRectF, Qt
from PySide6.QtGui import (QBrush, QColor, QImage, QPainter, QPainterPath, QPainterPathStroker, QPen,
                           QPolygonF)

# Keeps the oracle file small: a dashed stroke of a long path can have 100k elements.
MAX_STROKE_ELEMENTS = 150

CAPS = [Qt.PenCapStyle.FlatCap, Qt.PenCapStyle.SquareCap, Qt.PenCapStyle.RoundCap]
JOINS = [Qt.PenJoinStyle.MiterJoin, Qt.PenJoinStyle.BevelJoin, Qt.PenJoinStyle.RoundJoin,
         Qt.PenJoinStyle.SvgMiterJoin]


def f32(x):
    return struct.unpack("<f", struct.pack("<f", x))[0]


def hx(x):
    return "%016x" % struct.unpack("<Q", struct.pack("<d", x))[0]


def etype(e):
    return {"MoveToElement": 0, "LineToElement": 1, "CurveToElement": 2,
            "CurveToDataElement": 3}[e.type.name]


def elements(path):
    out = [str(path.elementCount())]
    for i in range(path.elementCount()):
        e = path.elementAt(i)
        out += [str(etype(e)), hx(e.x), hx(e.y)]
    return out


def arc_case(mode, rect, start, sweep):
    rect = tuple(f32(v) for v in rect)
    start, sweep = f32(start), f32(sweep)
    r = QRectF(*rect)
    p = QPainterPath()
    if mode == 1:
        p.arcMoveTo(r, start)
    elif mode == 2:
        p.moveTo(1.5, 2.5)
        p.lineTo(3.0, 4.0)
    p.arcTo(r, start, sweep)
    return ["arc", str(mode)] + [hx(v) for v in (*rect, start, sweep)] + elements(p)


def build_path(ops):
    p = QPainterPath()
    for op in ops:
        k, a = op[0], op[1:]
        if k == "M":
            p.moveTo(*a)
        elif k == "L":
            p.lineTo(*a)
        elif k == "Q":
            p.quadTo(*a)
        elif k == "C":
            p.cubicTo(*a)
        elif k == "Z":
            p.closeSubpath()
    return p


def stroke_case(width, cap, join, miter, doff, thresh, dashes, ops):
    s = QPainterPathStroker()
    s.setWidth(width)
    s.setCapStyle(CAPS[cap])
    s.setJoinStyle(JOINS[join])
    s.setMiterLimit(miter)
    if thresh:
        s.setCurveThreshold(thresh)
    s.setDashOffset(doff)
    if dashes:
        s.setDashPattern(dashes)
    out = s.createStroke(build_path(ops))
    line = ["stroke", hx(width), str(cap), str(join), hx(miter), hx(doff), hx(thresh),
            str(len(dashes))] + [hx(d) for d in dashes] + [str(len(ops))]
    for op in ops:
        line.append(op[0])
        line += [hx(v) for v in op[1:]]
    if out.elementCount() > MAX_STROKE_ELEMENTS:
        return None
    return line + elements(out)


def shoelace(poly):
    n = poly.count()
    a = 0.0
    for i in range(n):
        p, q = poly.at(i), poly.at((i + 1) % n)
        a += p.x() * q.y() - q.x() * p.y()
    return a / 2


def hud_case(side, pct, mark, ring_scale):
    """UsageDial.paintEvent's hatched stretch of the outer ring, in f32-exact inputs."""
    ring = f32(max(5.0, side * ring_scale))
    inset = ring / 2 + 1
    rect = tuple(f32(v) for v in (inset, inset, side - 2 * inset, side - 2 * inset))
    deg = lambda p: f32(-360 * min(100.0, max(0.0, p)) / 100)
    start = f32(90 + deg(mark))
    sweep = f32(deg(pct) - deg(mark))
    path = QPainterPath()
    r = QRectF(*rect)
    path.arcMoveTo(r, start)
    path.arcTo(r, start, sweep)
    s = QPainterPathStroker()
    s.setWidth(ring)
    s.setCapStyle(Qt.PenCapStyle.FlatCap)
    out = s.createStroke(path)
    cp = out.controlPointRect()
    poly = out.toFillPolygon()
    pb = poly.boundingRect()
    return (["hud"] + [hx(v) for v in (*rect, start, sweep, ring)] + [str(out.elementCount())]
            + [hx(v) for v in (cp.x(), cp.y(), cp.width(), cp.height(), shoelace(poly),
                               pb.x(), pb.y(), pb.width(), pb.height())])


def poly_case(w, h, rule, pen_width, brush, pts):
    img = QImage(w, h, QImage.Format.Format_ARGB32_Premultiplied)
    img.fill(0)
    p = QPainter(img)
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    p.setBrush(QBrush(QColor(255, 255, 255)) if brush else QBrush())
    p.setPen(QPen(QColor(255, 255, 255), pen_width) if pen_width else Qt.PenStyle.NoPen)
    poly = QPolygonF([QPointF(x, y) for x, y in pts])
    p.drawPolygon(poly, Qt.FillRule.WindingFill if rule else Qt.FillRule.OddEvenFill)
    p.end()
    alpha = bytes(img.pixelColor(x, y).alpha() for y in range(h) for x in range(w))
    return (["poly", str(w), str(h), str(rule), hx(pen_width), str(brush), str(len(pts))]
            + [hx(v) for xy in pts for v in xy] + [alpha.hex()])


def fill_alpha(w, h, path):
    img = QImage(w, h, QImage.Format.Format_ARGB32_Premultiplied)
    img.fill(0)
    p = QPainter(img)
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    p.fillPath(path, QBrush(QColor(255, 255, 255)))
    p.end()
    return bytes(img.pixelColor(x, y).alpha() for y in range(h) for x in range(w)).hex()


def wedge_case(w, h, cx, cy, rect, start, sweep):
    path = QPainterPath(QPointF(cx, cy))
    path.arcTo(QRectF(*rect), start, sweep)
    path.closeSubpath()
    return (["wedge", str(w), str(h)] + [hx(v) for v in (cx, cy, *rect[:3], start, sweep)]
            + [fill_alpha(w, h, path)])


def ringfill_case(w, h, rect, start, sweep, ring):
    path = QPainterPath()
    r = QRectF(*rect)
    path.arcMoveTo(r, start)
    path.arcTo(r, start, sweep)
    s = QPainterPathStroker()
    s.setWidth(ring)
    s.setCapStyle(Qt.PenCapStyle.FlatCap)
    return (["ringfill", str(w), str(h)] + [hx(v) for v in (*rect[:3], start, sweep, ring)]
            + [fill_alpha(w, h, s.createStroke(path))])


def rnd_pt(rng, span):
    return (f32(rng.uniform(-span, span)), f32(rng.uniform(-span, span)))


def rnd_ops(rng):
    span = rng.choice([5.0, 40.0, 200.0])
    ops = []
    for _ in range(rng.choice([1, 1, 2, 3])):
        ops.append(("M", *rnd_pt(rng, span)))
        for _ in range(rng.randint(1, 5)):
            kind = rng.choice("LLLQCCC")
            if kind == "L":
                ops.append(("L", *rnd_pt(rng, span)))
            elif kind == "Q":
                ops.append(("Q", *rnd_pt(rng, span), *rnd_pt(rng, span)))
            else:
                ops.append(("C", *rnd_pt(rng, span), *rnd_pt(rng, span), *rnd_pt(rng, span)))
        if rng.random() < 0.3:
            ops.append(("Z",))
    return ops


def main():
    sys.stdout.reconfigure(newline="\n")
    rng = random.Random(20260611)
    print(f"# Generated by gen_qpath_oracle.py from PySide6 {PySide6.__version__} "
          f"(Qt {PySide6.QtCore.__version__})")
    out = lambda toks: print(" ".join(toks))

    # ---- arcTo / arcMoveTo ----
    rects = [(0.0, 0.0, 100.0, 100.0), (4.5, 4.5, 72.0, 72.0), (10.0, 20.0, 80.0, 40.0),
             (-30.0, 12.25, 6.0, 90.0), (0.0, 0.0, 0.001, 0.001), (2.0, 3.0, 1000.0, 1000.0)]
    edge_angles = [0.0, 90.0, 180.0, 270.0, 360.0, -90.0, -180.0, -270.0, -360.0, 450.0, 720.0,
                   -720.0, 45.0, 135.0, 1.0, 89.9999, 90.0001, 179.5, 359.9, 0.001, -0.001, 1e-7]
    edge_sweeps = edge_angles + [0.0, 1e-9, -1e-9, 100000.0, -100000.0]
    for rect in rects:
        for start in edge_angles:
            for sweep in rng.sample(edge_sweeps, 4):
                out(arc_case(rng.choice([0, 1, 2]), rect, start, sweep))
    for _ in range(400):
        rect = (f32(rng.uniform(-50, 50)), f32(rng.uniform(-50, 50)),
                f32(rng.uniform(0.5, 300)), f32(rng.uniform(0.5, 300)))
        start = f32(rng.uniform(-800, 800))
        sweep = f32(rng.choice([rng.uniform(-420, 420), rng.uniform(-30, 30),
                                float(rng.randint(-5, 5) * 90)]))
        out(arc_case(rng.choice([0, 1, 2]), rect, start, sweep))
    # null rectangle, negative size and invalid arguments
    out(arc_case(0, (1.0, 2.0, 0.0, 0.0), 10.0, 50.0))
    out(arc_case(1, (1.0, 2.0, 0.0, 0.0), 10.0, 50.0))
    out(arc_case(0, (1.0, 2.0, 20.0, 0.0), 10.0, 50.0))
    out(arc_case(0, (1.0, 2.0, -20.0, 30.0), 10.0, 50.0))
    out(arc_case(0, (1.0, 2.0, 20.0, 30.0), math.inf, 50.0))
    out(arc_case(1, (1.0, 2.0, 20.0, 30.0), math.nan, 50.0))
    out(arc_case(0, (1.0, 2.0, 20.0, 30.0), 10.0, -math.inf))

    # ---- QPainterPathStroker ----
    simple = [("M", 0.0, 0.0), ("L", 40.0, 0.0), ("L", 40.0, 30.0)]
    for cap in range(3):
        for join in range(4):
            for miter in (1.0, 2.0, 10.0):
                out(stroke_case(6.0, cap, join, miter, 0.0, 0.0, [], simple))
                out(stroke_case(6.0, cap, join, miter, 0.0, 0.0, [], simple + [("Z",)]))
    sharp = [("M", 0.0, 0.0), ("L", 30.0, 4.0), ("L", 0.0, 8.0)]
    for join in range(4):
        for miter in (1.0, 2.0, 4.0, 100.0):
            out(stroke_case(5.0, 0, join, miter, 0.0, 0.0, [], sharp))
    out(stroke_case(3.0, 1, 1, 2.0, 0.0, 0.0, [], [("M", 5.0, 5.0), ("L", 5.0, 5.0)]))
    out(stroke_case(3.0, 2, 2, 2.0, 0.0, 0.0, [], [("M", 5.0, 5.0)]))
    out(stroke_case(3.0, 2, 2, 2.0, 0.0, 0.0, [], []))
    out(stroke_case(3.0, 0, 0, 2.0, 0.0, 0.0, [], [("M", 1.0, 1.0), ("L", 20.0, 1.0), ("L", 1.0, 1.0)]))
    for width in (0.0, -2.0, 0.5, 1.0, 2.5, 17.0, 120.0):
        s = QPainterPathStroker()
        s.setWidth(width)
        out(["width", hx(width), hx(s.width()), hx(s.curveThreshold())])
    for width in (0.5, 1.0, 6.0, 20.0):
        for dashes in ([2.0, 1.0], [4.0, 2.0, 1.0, 2.0], [1.0], [0.0, 3.0], [3.0, 2.0, 5.0]):
            for doff in (0.0, 1.5, -2.0, 7.0):
                while True:
                    ops = rng.choice([simple, rnd_ops(rng), rnd_ops(rng)])
                    case = stroke_case(width, rng.randrange(3), rng.randrange(4), 2.0, doff,
                                       0.0, dashes, ops)
                    if case:
                        out(case)
                        break
    for _ in range(220):
        width = f32(rng.choice([rng.uniform(0.2, 4), rng.uniform(4, 30), 1.0, 6.0]))
        thresh = 0.0 if rng.random() < 0.8 else f32(rng.choice([0.01, 0.05, 0.5, 1.0]))
        dashes = [] if rng.random() < 0.7 else [f32(rng.uniform(0.2, 5)) for _ in range(rng.randint(2, 5))]
        while True:
            case = stroke_case(width, rng.randrange(3), rng.randrange(4),
                               f32(rng.uniform(0.5, 8)),
                               f32(rng.uniform(-5, 5)) if dashes else 0.0, thresh, dashes,
                               rnd_ops(rng))
            if case:
                out(case)
                break

    # ---- drawPolygon ----
    scale = 1.25
    size = 11 * scale
    x, y = f32(34 - size - 2 * scale), f32(1.5 * scale)
    tri = [(f32(x + size / 2), y), (f32(x + size), f32(y + size * 0.92)),
           (x, f32(y + size * 0.92))]
    star = [(f32(20 + 17 * math.sin(2 * math.pi * k * 2 / 5)),
             f32(20 - 17 * math.cos(2 * math.pi * k * 2 / 5))) for k in range(5)]
    arrow = [(4.0, 14.0), (22.5, 14.0), (22.5, 6.0), (36.0, 20.0), (22.5, 34.0), (22.5, 26.0),
             (4.0, 26.0)]
    out(poly_case(34, 18, 1, 0.0, 1, tri))
    out(poly_case(34, 18, 0, 0.0, 1, tri))
    out(poly_case(40, 40, 0, 0.0, 1, star))
    out(poly_case(40, 40, 1, 0.0, 1, star))
    out(poly_case(40, 40, 1, 2.0, 1, star))
    out(poly_case(40, 40, 0, 0.0, 1, arrow))
    out(poly_case(40, 40, 1, 1.5, 1, arrow))
    out(poly_case(40, 40, 1, 1.5, 0, arrow))
    out(poly_case(40, 40, 1, 0.0, 1, [(5.0, 5.0), (30.0, 5.0), (30.0, 25.0), (5.0, 25.0)]))
    out(poly_case(40, 40, 1, 3.0, 1, [(5.5, 5.5), (30.5, 5.5), (30.5, 25.5), (5.5, 25.5)]))
    out(poly_case(40, 40, 1, 3.0, 1, [(5.5, 5.5), (30.5, 5.5), (30.5, 25.5), (5.5, 25.5),
                                      (5.5, 5.5)]))
    out(poly_case(40, 40, 1, 2.0, 1, [(5.0, 5.0)]))

    # ---- fillPath of the HUD dial's pie wedge and ring hatch ----
    for start, sweep in ((-10.0, -150.0), (90.0, -90.0), (90.0 - 36.0, -216.5), (30.0, 270.0)):
        out(wedge_case(60, 60, 30.0, 30.0, (4.5, 4.5, 51.0, 51.0), start, sweep))
    for start, sweep in ((90.0 - 100.0, -150.0), (90.0, -270.0), (90.0 - 17.5, -120.25)):
        out(ringfill_case(80, 80, (3.5, 3.5, 73.0, 73.0), f32(start), f32(sweep), 5.5))

    # ---- the HUD's donut-arc hatch ----
    for side in (62.0, 84.0, 100.0, 140.0, 201.0, 320.0):
        for _ in range(6):
            pct = rng.uniform(20, 100)
            mark = rng.uniform(1, pct - 1)
            out(hud_case(side, pct, mark, 0.07))
    out(hud_case(100.0, 73.3, 41.7, 0.07))
    out(hud_case(100.0, 99.9, 0.1, 0.07))


if __name__ == "__main__":
    main()
