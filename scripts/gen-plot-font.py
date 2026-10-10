#!/usr/bin/env python3
"""Generate assets/fonts/MacrosPlot-Regular.ttf — the glyphs text charts use.

Lilex (the editor font) has no braille block and no U+25A0, so those glyphs
fell back to system fonts with other advance widths (Apple Braille is 0.684 em
against Lilex's 0.600) and chart columns drifted out of line with their axes.
This font draws every chart glyph from scratch at exactly Lilex's cell: 600
units wide on a 1000 upm grid, ascent 1025 / descent 275. The plot faces name
it via `set-face-font` (scheme/themes/*.scm), so those runs shape with it
directly instead of going through font fallback.

Covered: space, U+2800–28FF braille, ▁▂▃▄▅▆▇█ ▏▎▍▌▋▊▉ ▐ ▀ the quadrant
blocks, and ■. All outlines are original geometry (rectangles and dots).

Requires fontTools:  python3 -m pip install fonttools
Usage:               python3 scripts/gen-plot-font.py
"""

import math
import pathlib

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

UPM = 1000
ADVANCE = 600
ASCENT = 1025
DESCENT = -275
HEIGHT = ASCENT - DESCENT  # 1300: the whole cell, top to bottom

OUT = pathlib.Path(__file__).resolve().parent.parent / "assets/fonts/MacrosPlot-Regular.ttf"


def rect(pen, x0, y0, x1, y1):
    """A clockwise (TrueType outer) rectangle."""
    pen.moveTo((x0, y0))
    pen.lineTo((x0, y1))
    pen.lineTo((x1, y1))
    pen.lineTo((x1, y0))
    pen.closePath()


def dot(pen, cx, cy, r, sides=16):
    """A clockwise polygonal dot (a circle at chart sizes)."""
    for i in range(sides):
        a = -2 * math.pi * i / sides
        p = (round(cx + r * math.cos(a)), round(cy + r * math.sin(a)))
        if i == 0:
            pen.moveTo(p)
        else:
            pen.lineTo(p)
    pen.closePath()


def glyph(draw):
    pen = TTGlyphPen(None)
    draw(pen)
    return pen.glyph()


def empty(pen):
    pass


# Braille dot centers: 2 columns × 4 rows spread evenly over the cell, so a
# dot pattern tiles across neighboring cells at a constant pitch.
COLS = [ADVANCE / 4, ADVANCE * 3 / 4]
ROWS = [ASCENT - HEIGHT / 8 - k * HEIGHT / 4 for k in range(4)]
# Unicode dot numbering → (column, row).
BRAILLE_DOTS = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)]


def braille(mask):
    def draw(pen):
        for bit, (c, r) in enumerate(BRAILLE_DOTS):
            if mask & (1 << bit):
                dot(pen, COLS[c], ROWS[r], 85)
    return draw


def quadrants(tl, tr, bl, br):
    mid_x, mid_y = ADVANCE / 2, DESCENT + HEIGHT / 2

    def draw(pen):
        if tl:
            rect(pen, 0, mid_y, mid_x, ASCENT)
        if tr:
            rect(pen, mid_x, mid_y, ADVANCE, ASCENT)
        if bl:
            rect(pen, 0, DESCENT, mid_x, mid_y)
        if br:
            rect(pen, mid_x, DESCENT, ADVANCE, mid_y)
    return draw


glyphs = {".notdef": empty, "space": empty, "m": empty}
# gpui refuses to load a font with no 'm' (it measures text with it), so the
# font carries an outline-less 'm' at the cell advance. Plot faces only ever
# cover chart glyphs, so it never renders.
cmap = {0x20: "space", ord("m"): "m"}


def add(cp, draw):
    name = f"uni{cp:04X}"
    glyphs[name] = draw
    cmap[cp] = name


for mask in range(256):
    add(0x2800 + mask, braille(mask))
# Lower eighths ▁…█ (U+2581–2588) and left eighths ▉…▏ (U+2589–258F).
for k in range(1, 9):
    add(0x2580 + k, lambda pen, k=k: rect(pen, 0, DESCENT, ADVANCE, DESCENT + HEIGHT * k / 8))
for k in range(1, 8):
    add(0x2590 - k, lambda pen, k=k: rect(pen, 0, DESCENT, ADVANCE * k / 8, ASCENT))
add(0x2580, quadrants(1, 1, 0, 0))  # ▀
add(0x2590, quadrants(0, 1, 0, 1))  # ▐
# Quadrant blocks (U+2596–259F) by (tl, tr, bl, br).
for cp, q in {
    0x2596: (0, 0, 1, 0), 0x2597: (0, 0, 0, 1), 0x2598: (1, 0, 0, 0),
    0x2599: (1, 0, 1, 1), 0x259A: (1, 0, 0, 1), 0x259B: (1, 1, 1, 0),
    0x259C: (1, 1, 0, 1), 0x259D: (0, 1, 0, 0), 0x259E: (0, 1, 1, 0),
    0x259F: (0, 1, 1, 1),
}.items():
    add(cp, quadrants(*q))
add(0x25A0, lambda pen: rect(pen, 100, 60, 500, 460))  # ■, on the x-height band

order = list(glyphs)
fb = FontBuilder(UPM, isTTF=True)
fb.setupGlyphOrder(order)
fb.setupCharacterMap(cmap)
fb.setupGlyf({name: glyph(draw) for name, draw in glyphs.items()})
fb.setupHorizontalMetrics({name: (ADVANCE, 0) for name in order})
fb.setupHorizontalHeader(ascent=ASCENT, descent=DESCENT, lineGap=0)
fb.setupNameTable({
    "familyName": "Macros Plot",
    "styleName": "Regular",
    "uniqueFontIdentifier": "MacrosPlot-Regular",
    "fullName": "Macros Plot Regular",
    "psName": "MacrosPlot-Regular",
    "version": "Version 1.000",
    "licenseDescription": "MIT — part of Macros (https://github.com/macros-app/macros)",
})
fb.setupOS2(
    sTypoAscender=ASCENT, sTypoDescender=DESCENT, sTypoLineGap=0,
    usWinAscent=ASCENT, usWinDescent=-DESCENT, xAvgCharWidth=ADVANCE,
    version=4, fsSelection=0x40 | 0x80,  # REGULAR | USE_TYPO_METRICS, as Lilex sets
)
fb.setupPost(isFixedPitch=1)
fb.save(str(OUT))
print(f"wrote {OUT} ({len(cmap)} chars)")
