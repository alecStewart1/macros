---
title: Text charts
description: Sparklines, bar charts, line and scatter plots, histograms, and heatmaps drawn as colored text in any buffer.
---

# Text charts

`scheme/plot.scm` draws charts out of characters — braille dots, block elements, and box-drawing lines — colored with faces. Because a chart is just text, it works anywhere text does: inline in a dashboard row, in an Org or Markdown file, in a REPL, or in a buffer of its own. Run <kbd>M-x</kbd> `plot-demo` to see every chart type.

```
 12.0 ┤      ⢀⡠⠊⠉⠢⡀
  8.0 ┤   ⢀⠔⠁    ⠈⠢⡀      ⡠⠒⠢⡀
  4.0 ┤ ⡠⠊          ⠈⠢⣀⡠⠔⠁   ⠈⠢
  0.0 ┼─────────────────────────
```

## Chart functions

Each function takes data and then optional settings as a flat list of `'key value` pairs, and returns a **plot**.

```scheme
(plot-sparkline (list 1 3 2 5 8 6))                       ; ▁▃▂▅█▆
(plot-bars (list (list "rust" 48210) (list "scheme" 15302)) 'width 30)
(plot-lines (list (list "cpu" cpu-samples) (list "mem" mem-samples))
            'width 60 'height 10 'title "load")
(plot-scatter (list (list "runs" (list (list 1 4.2) (list 2 3.9)))))
(plot-histogram latencies 'bins 12 'height 6)
(plot-heatmap matrix 'row-labels (list "mon" "tue" "wed"))
```

| Function | Data | Settings |
|---|---|---|
| `plot-sparkline` | a list of numbers | `'min` `'max` `'face` |
| `plot-bars` | `(label value)` items | `'width` `'max` `'face` `'colors` `'values` |
| `plot-lines`, `plot-scatter` | numbers (x = index), or `((name data) …)` where data is numbers or `(x y)` pairs | `'width` `'height` `'mode` `'x-min` `'x-max` `'y-min` `'y-max` `'title` `'legend` |
| `plot-histogram` | a list of numbers | `'bins` `'height` `'bin-width` `'min` `'max` `'face` `'title` |
| `plot-heatmap` | a list of rows of numbers | `'mode` (`'cell` or `'half`) `'cell-width` `'min` `'max` `'row-labels` `'col-labels` `'scale` `'title` |

`plot-sparkline-string` returns just the glyphs, for building your own line of text.

Line and scatter plots draw in braille by default (2 × 4 dots per character); `'mode 'quadrant` uses 2 × 2 blocks instead. Where two series cross in one character cell, the series drawn later wins the color.

Heatmaps default to `'cell` mode: each value is a run of spaces on a background color, which looks the same with any font and line height. `'half` mode packs two rows of data into each line using `▀` with separate foreground and background colors.

## Putting a plot in a buffer

```scheme
(plot-insert p)                 ; insert at point and color it
(plot-show p "*load*")          ; a buffer of its own
(plot-section-emit p)           ; inside a section-begin … section-render build
```

To place a plot yourself, `(plot-text p)` gives its text and `(plot-spans p base)` gives `(start end face)` color ranges for that text inserted at byte offset `base` — the shape `overlay-set-faces` takes.

A plot is a list of rows, each `(text spans)` with spans as byte offsets into that row, so plots compose: prepend a label with `plot-row-text` and shift the spans by the label's `utf8-length`.

## Faces

| Face | Used for |
|---|---|
| `plot-series-0` … `plot-series-5` | series and bar colors, cycled in order |
| `plot-axis`, `plot-label`, `plot-title` | axes, tick and value labels, titles |
| `plot-heat-0` … `plot-heat-7` | heatmap colors, low to high (foreground) |
| `plot-heat-0-bg` … `plot-heat-7-bg` | the same ramp as backgrounds |

The series and heat faces use the bundled **Macros Plot** font through `set-face-font`, so chart columns stay aligned with the editor font. See [Themes & faces](/docs/themes-and-faces#face-attributes).

## Low-level primitives

The chart functions are built on a few Rust primitives you can call directly:

- `(textplot-render width height mode ops)` draws a list of operations on a `width × height` character grid and returns one `(text spans)` row per line. `mode` is `braille`, `block`, `quadrant`, or `half`. Operations use dot coordinates (top-left origin, y grows downward): `("point" x y face)`, `("line" x0 y0 x1 y1 face)`, `("rect" x0 y0 x1 y1 face)`, `("polyline" face (x0 y0 x1 y1 …))`, and `("text" col row string face)` in character cells.
- `(textplot-sparkline values lo hi)` and `(textplot-hbar value max width)` return glyph strings.
- `(textplot-ticks lo hi n)` returns about `n` evenly spaced `(value label)` axis ticks.
- `(textplot-nice-range lo hi n)` widens `(lo hi)` to whole tick steps, so both axis ends get a label. Line, scatter and histogram axes do this unless you pass `'y-min` or `'y-max`.
- `(textplot-format v)` formats a number compactly: `950`, `12.3k`, `3.4M`.
