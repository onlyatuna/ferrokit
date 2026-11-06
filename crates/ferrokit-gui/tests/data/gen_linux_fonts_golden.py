#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-3.0-only
"""Regenerates the golden of tests/test_linux_fonts.rs: which font files real Qt picks on Linux for a family
name and weight/slant (QFontDatabase + fontconfig: `QFontconfigDatabase::populateFontDatabase`,
`QFontDatabasePrivate::match` / `bestStyle`, and for a family that is missing `fallbacksForFamily`),
which font each character of a text gets when the primary font has no glyph for it
(`QFontEngineMulti`, `fallbacksForFamily` per script item), and the advances that follow.

Needs Linux with fontconfig and PySide6 (measured with 6.12.0, `QT_QPA_PLATFORM=offscreen`). Run it with
`QT_QPA_PLATFORM=offscreen python gen_linux_fonts_golden.py > linux_fonts_golden_<name>.txt` and append the
output to linux_fonts_golden.txt (one chunk per machine).

Lines (fields separated by one space; families have `_` for a space, an empty hint is `-`):
* `fontset <count> <fnv1a64> <lang>`: the number of font files fontconfig lists and the hash of their sorted
  paths, each followed by a line feed; `<lang>` is the first of LC_ALL, LC_CTYPE, LANG that is set (`-` if
  none), which decides the language `FcDefaultSubstitute` adds. The test compares the lines below only when
  its machine has the same set and language.
* `face <family> <hint> <weight> <italic> <face id> adv <text id> <1/64 px>`: the face of the primary
  engine (`QRawFont.fromFont`, identified by its PostScript name and `head` checksum) and the
  `QFontMetricsF.horizontalAdvance` of a text, at 16 px.
* `runs <family> <hint> <weight> <italic> <text id> <face id>:<characters> ...`: the face the glyph run of each
  character belongs to (`QTextLayout.glyphRuns(i, 1)`), run-length coded.
"""
import os
import subprocess
import sys

TEXTS = {
    "l": "Hamburgefonstiv 0123 AVAWTo",
    "a": "\u0627\u0644\u0639\u0631\u0628\u064a\u0629",
    "A": "\u0627\u0644\u0639\u0631\u0628\u064a\u0629 \u0639\u0631\u0628\u064a",
    "h": "\u05e9\u05dc\u05d5\u05dd",
    "d": "\u0928\u092e\u0938\u094d\u0924\u0947",
    "b": "\u09ac\u09be\u0982\u09b2\u09be",
    "m": "\u0ba4\u0bae\u0bbf\u0bb4\u0bcd",
    "t": "\u0e2a\u0e27\u0e31\u0e2a\u0e14\u0e35",
    "s": "\u0dc3\u0dd2\u0d82\u0dc4\u0dbd",
    "k": "\uc548\ub155\ud558\uc138\uc694",
    "j": "\u3072\u3089\u304c\u306a\u30ab\u30bf\u30ab\u30ca",
    "c": "\u4e2d\u6587\u5b57\u6f22\u5b57",
    "g": "\u03b1\u03b2\u03b3 \u0394\u03a9",
    "r": "\u0436\u0443\u043a \u042f",
    "n": "\u0540\u0561\u0575\u0565\u0580\u0565\u0576",
    "o": "\u10e5\u10d0\u10e0\u10d7\u10e3\u10da\u10d8",
    "y": "\u2605\u2713\u2192\u2211\u2665\u00a9\u20ac",
    "x": "\u250c\u2500\u2510\u2588\u2591",
    "e": "Hi \U0001F600\U0001F389 \u2600\u2714",
    "u": "\u2200\u2202\u2208\u211d\u221e",
    "w": "a\u00a0b",
    "z": "abc \u0627\u0644\u0639\u0631\u0628 \u6f22\u5b57 \u2713 \U0001F600 \u0928 z",
}
ADV_TEXTS = ("l", "a", "A", "h", "c", "y", "z")
FALLBACK_TEXTS = tuple(k for k in TEXTS if k != "l")
HINTS = ("-", "SansSerif", "Serif", "TypeWriter", "Monospace", "Cursive", "Fantasy", "System", "OldEnglish")
# Families asked for besides every family Qt lists: names that map through fontconfig's rules or are missing.
EXTRA_FAMILIES = (
    "Arial", "arial", "ARIAL", "Helvetica", "Helvetica Neue", "Times New Roman", "Times", "Courier New", "Courier",
    "Segoe UI", "Tahoma", "Verdana", "Calibri", "Cambria", "Consolas", "Menlo", "Monaco", "SF Pro Text",
    "Microsoft JhengHei UI", "Microsoft YaHei", "MS Gothic",
    "Liberation Sans", "Liberation Serif", "Liberation Mono", "Liberation Sans Narrow", "Arimo", "Tinos", "Cousine",
    "DejaVu Sans", "DejaVu Serif", "DejaVu Sans Mono", "DejaVu Sans Condensed", "DejaVu Sans Light",
    "DejaVu Serif Condensed", "DejaVu Sans Mono Bold",
    "Noto Sans", "Noto Serif", "Noto Sans Mono", "Noto Sans CJK SC", "Noto Sans CJK TC", "Noto Sans CJK JP",
    "Noto Serif CJK SC", "Noto Color Emoji", "Noto Emoji", "Noto Sans Arabic", "Noto Sans Hebrew",
    "sans-serif", "serif", "monospace", "Sans Serif", "Serif", "Monospace", "cursive", "fantasy", "system-ui",
    "Ubuntu", "Roboto", "Open Sans", "FreeSans", "FreeSerif", "FreeMono", "Lucida Grande", "NoSuchFamilyQQQ",
)
MISSING_FOR_HINTS = ("NoSuchFamilyQQQ",)
# Families whose fallbacks per script are probed.
FALLBACK_FAMILIES = (
    "DejaVu Sans", "Liberation Sans", "Arial", "Times New Roman", "Courier New", "Liberation Mono", "DejaVu Sans Mono",
    "Noto Sans CJK SC", "NoSuchFamilyQQQ", "sans-serif", "Verdana", "Georgia", "Comic Sans MS",
)
PX = 16


def fnv1a64(data):
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def fontset_line():
    out = subprocess.run(["fc-list", "--format", "%{file}\\n"], capture_output=True, text=True, check=True).stdout
    paths = sorted(set(line for line in out.split("\n") if line))
    blob = "".join(p + "\n" for p in paths).encode()
    lang = "-"
    for var in ("LC_ALL", "LC_CTYPE", "LANG"):
        if os.environ.get(var):
            lang = os.environ[var]
            break
    return "fontset %d %016x %s" % (len(paths), fnv1a64(blob), lang)


def psname(name_table):
    if len(name_table) < 6:
        return "?"
    count = int.from_bytes(name_table[2:4], "big")
    base = int.from_bytes(name_table[4:6], "big")
    best = None
    for i in range(count):
        rec = name_table[6 + 12 * i: 18 + 12 * i]
        if len(rec) < 12:
            break
        platform, _enc, _lang, name_id, length, offset = (int.from_bytes(rec[j:j + 2], "big") for j in range(0, 12, 2))
        if name_id != 6:
            continue
        raw = name_table[base + offset: base + offset + length]
        if platform == 3 or platform == 0:
            text = raw.decode("utf-16-be", "replace")
            rank = 0
        elif platform == 1:
            text = raw.decode("latin-1")
            rank = 1
        else:
            continue
        if best is None or rank < best[0]:
            best = (rank, text)
    return best[1] if best else "?"


def face_id(raw):
    if not raw.isValid():
        return "invalid"
    name = bytes(raw.fontTable("name"))
    head = bytes(raw.fontTable("head"))
    if not head:
        return "box"
    return "%s#%s" % (psname(name).replace(" ", "_"), head[8:12].hex())


def main():
    from PySide6.QtGui import QFont, QFontDatabase, QFontMetricsF, QGuiApplication, QRawFont, QTextLayout
    app = QGuiApplication(sys.argv)
    hints = {
        "-": QFont.StyleHint.AnyStyle, "SansSerif": QFont.StyleHint.SansSerif, "Serif": QFont.StyleHint.Serif,
        "TypeWriter": QFont.StyleHint.TypeWriter, "Monospace": QFont.StyleHint.Monospace,
        "Cursive": QFont.StyleHint.Cursive, "Fantasy": QFont.StyleHint.Fantasy, "System": QFont.StyleHint.System,
        "OldEnglish": QFont.StyleHint.OldEnglish,
    }

    def make(family, hint, weight, italic):
        # Every line is measured with an empty font engine cache, as in a fresh process. Qt's emoji
        # item pass takes a listed family whose emoji engine an earlier request of the same QFontDef
        # left in the QFontCache before it asks for a color font (`findMatchingFallback`,
        # qfontdatabase.cpp:2797), so without a color font the missing-glyph engine of an emoji
        # otherwise depends on the lines measured before. Adding and removing an application emoji
        # family clears the cache (`QFontCache::clear()`, qfontdatabase.cpp:2283, 2300) and nothing
        # else that lasts.
        QFontDatabase.addApplicationEmojiFontFamily("gen_linux_fonts_golden")
        QFontDatabase.removeApplicationEmojiFontFamily("gen_linux_fonts_golden")
        font = QFont(family)
        font.setPixelSize(PX)
        font.setStyleHint(hints[hint])
        font.setWeight(QFont.Weight(weight))
        font.setItalic(bool(italic))
        return font

    def primary_id(font):
        return face_id(QRawFont.fromFont(font))

    def runs(font, text):
        layout = QTextLayout(text, font)
        layout.beginLayout()
        line = layout.createLine()
        line.setLineWidth(100000)
        layout.endLayout()
        out = []
        pos = 0
        for ch in text:
            runs_here = layout.glyphRuns(pos, 1)
            if not runs_here:
                fid = "none"
            elif all(g == 0 for r in runs_here for g in r.glyphIndexes()):
                fid = "0"  # no engine has the character: which font draws the notdef is not observable
            else:
                fid = face_id(runs_here[0].rawFont())
            if out and out[-1][0] == fid:
                out[-1][1] += 1
            else:
                out.append([fid, 1])
            pos += 2 if ord(ch) > 0xFFFF else 1
        return " ".join("%s:%d" % (f, n) for f, n in out)

    def name(family):
        return (family or "").replace(" ", "_") or "_"

    print(fontset_line())
    families = list(QFontDatabase.families())
    seen = set()
    requests = []
    for family in families + list(EXTRA_FAMILIES):
        if family in seen:
            continue
        seen.add(family)
        requests.append(family)
    for family in requests:
        for weight, italic in ((400, 0), (700, 0), (400, 1), (700, 1)):
            font = make(family, "-", weight, italic)
            fid = primary_id(font)
            metrics = QFontMetricsF(font)
            parts = []
            for tid in ADV_TEXTS:
                parts.append("adv %s %d" % (tid, round(metrics.horizontalAdvance(TEXTS[tid]) * 64)))
            print("face %s - %d %d %s %s" % (name(family), weight, italic, fid, " ".join(parts)))
    for hint in HINTS[1:]:
        for family in MISSING_FOR_HINTS:
            for weight, italic in ((400, 0), (700, 1)):
                font = make(family, hint, weight, italic)
                metrics = QFontMetricsF(font)
                parts = []
                for tid in ADV_TEXTS:
                    parts.append("adv %s %d" % (tid, round(metrics.horizontalAdvance(TEXTS[tid]) * 64)))
                print("face %s %s %d %d %s %s" % (name(family), hint, weight, italic, primary_id(font), " ".join(parts)))
    for family in FALLBACK_FAMILIES:
        for hint in ("-", "SansSerif", "Serif", "Monospace"):
            if hint != "-" and family not in ("NoSuchFamilyQQQ", "DejaVu Sans"):
                continue
            for weight, italic in ((400, 0), (700, 1)):
                for tid in FALLBACK_TEXTS:
                    font = make(family, hint, weight, italic)
                    adv = round(QFontMetricsF(font).horizontalAdvance(TEXTS[tid]) * 64)
                    print("runs %s %s %d %d %s %d %s" % (name(family), hint, weight, italic, tid, adv, runs(font, TEXTS[tid])))


if __name__ == "__main__":
    main()
    # PySide6 on some Pythons (3.10 on ubuntu-22.04) aborts while the interpreter shuts down, after
    # everything has been written; leave without finalizing.
    sys.stdout.flush()
    os._exit(0)
