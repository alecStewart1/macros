---
title: Text charts
description: Sparklines, bar charts, line and scatter plots, function and normal-distribution plots with shaded areas, histograms, and heatmaps drawn as colored text in any buffer.
---

# Text charts

`scheme/plot.scm` draws charts out of characters — braille dots, block elements, and box-drawing lines — colored with faces. Because a chart is just text, it works anywhere text does: inline in a dashboard row, in an Org or Markdown file, in a REPL, or in a buffer of its own. In Org, a `scheme` source block that returns a chart writes it as the block's result, and <kbd>C-c " g</kbd> charts a table. See [Org mode](/docs/org-mode#charting-a-table). Run <kbd>M-x</kbd> `plot-demo` to see every chart type.

```
12 ┤      ⢀⡠⠊⠉⠢⡀
   │   ⢀⠔⠁    ⠈⠢⡀      ⡠⠒⠢⡀
 6 ┤ ⡠⠊          ⠈⠢⣀⡠⠔⠁   ⠈⠢
 0 ┤⠊
   └─────────────────────────
    0                      24
```

## Chart functions

Each function takes data and then optional settings as a flat list of `'key value` pairs, and returns a **plot**.

```scheme
(plot-sparkline (list 1 3 2 5 8 6))                       ; ▁▃▂▅█▆
(plot-bars (list (list "rust" 48210) (list "scheme" 15302)) 'width 30)
(plot-lines (list (list "cpu" cpu-samples) (list "mem" mem-samples))
            'width 60 'height 10 'title "load")
(plot-scatter (list (list "runs" (list (list 1 4.2) (list 2 3.9)))))
(plot-function sin -6.3 6.3 'shade (list 0 3.14))
(plot-normal 0 1 'shade (list -1.96 1.96))
(plot-histogram latencies 'bins 12 'height 6)
(plot-heatmap matrix 'row-labels (list "mon" "tue" "wed"))
```

| Function | Data | Settings |
|---|---|---|
| `plot-sparkline` | a list of numbers | `'min` `'max` `'face` |
| `plot-bars` | `(label value)` items | `'width` `'max` `'face` `'colors` `'values` |
| `plot-lines`, `plot-scatter` | numbers (x = index), or `((name data) …)` where data is numbers or `(x y)` pairs | `'width` `'height` `'mode` `'x-min` `'x-max` `'y-min` `'y-max` `'title` `'legend` |
| `plot-function` | a function and an x range `lo hi`, or `((name f) …)` for several | `'width` `'height` `'y-min` `'y-max` `'x-ticks` `'shade` `'shade-face` `'title` `'legend` |
| `plot-normal` | a mean and standard deviation | everything `plot-function` takes, plus `'x-min` `'x-max` |
| `plot-histogram` | a list of numbers | `'bins` `'height` `'bin-width` `'min` `'max` `'face` `'title` |
| `plot-heatmap` | a list of rows of numbers | `'mode` (`'cell` or `'half`) `'cell-width` `'min` `'max` `'row-labels` `'col-labels` `'scale` `'title` |

`plot-sparkline-string` returns just the glyphs, for building your own line of text.

Line and scatter plots draw in braille by default (2 × 4 dots per character); `'mode 'quadrant` uses 2 × 2 blocks instead. Where two series cross in one character cell, the series drawn later wins the color.

## Functions and shaded areas

`plot-function` draws `y = f(x)` between two x values, sampling once per braille dot column. Pass one function, or a list of `(name f)` pairs to draw several with a legend:

```scheme
(plot-function (list (list "sin x" sin) (list "x/4" (lambda (x) (/ x 4.0))))
               -6.3 6.3 'x-ticks (list -6 -3 0 3 6))
```

`'shade` fills the area between the first function and the line y = 0 over an x range. Give one range `(a b)` or a list of them; a range can carry its own face as a third item, `(a b "my-face")`. Where the curve is below zero the fill runs down to it. Ranges past the ends of the plot stop at the edge, so a large number works as "to infinity":

```
 1 ┤   ⡠⠔⠉⠉⠑⠤⡀                 ⣠⣶⣿⣿⣶⣄
   │ ⢀⠔⠁     ⠘⢄              ⢠⣾⣿⣿⣿⣿⣿⣿⣷⡄
   │⢠⠊         ⠣⡀           ⣰⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣆           ⢀
 0 ┤⠁           ⠑⡄        ⢀⠜⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠣⡀        ⢠⠊
   │             ⠈⢆      ⡠⠊              ⠑⢄      ⡔⠁
-1 ┤               ⠑⠤⣀⣀⠤⠊                 ⠈⠒⢄⣀⣀⠔⠊
   └─┬──────────┬──────────┬───────────┬──────────┬─
    -6         -3          0           3          6
```

That's `(plot-function sin -6.3 6.3 'width 48 'height 6 'shade (list 0 3.14) 'x-ticks (list -6 -3 0 3 6))`. Where a function returns something that isn't a real number — `#f`, a division by zero, the square root of a negative — the curve has a gap there instead of a spike. `'x-ticks` labels the x values you list, each marked `┬` on the axis; without it, only the two ends are labeled.

### Normal distributions

`(plot-normal mean sd)` plots the normal distribution's bell curve from 4 standard deviations below the mean to 4 above, with a tick at every whole standard deviation. When you shade ranges, the title shows how much of the distribution they cover:

```
     N(μ=0, σ=1)   shaded: 95.0%
0.4 ┤                     ⣠⣶⣿⣿⣶⣄
    │                   ⢀⣼⣿⣿⣿⣿⣿⣿⣷⡀
    │                  ⢠⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣄
    │                 ⣰⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦
0.2 ┤               ⢀⣼⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⡀
    │              ⣠⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣄
    │           ⢀⣠⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣄⡀
0.0 ┤⣀⣀⣀⣀⣀⣀⣀⠤⠤⠔⠊⠁⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡇⠈⠑⠢⠤⠤⣀⣀⣀⣀⣀⣀⣀
    └┬─────┬─────┬─────┬─────┬────┬─────┬─────┬─────┬
     -4   -3    -2    -1     0    1     2     3     4
```

```scheme
(plot-normal 0 1 'shade (list -1.96 1.96))                        ; the middle 95%
(plot-normal 100 15 'shade (list (list -1000 70.6) (list 129.4 1000)))  ; both tails, 5.0%
(plot-normal 0 1 'shade (list -1 1) 'title "within one σ")        ; your own title
```

Passing your own `'title` replaces the generated one entirely, including the shaded percentage. Overlapping ranges are counted twice in the percentage. The density and cumulative probability are available on their own:

- `(plot-normal-pdf x mean sd)` — the height of the curve at `x`.
- `(plot-normal-cdf x mean sd)` — the probability of a value at or below `x`, accurate to about 1.5 × 10⁻⁷. The probability of a range is `(- (plot-normal-cdf b m s) (plot-normal-cdf a m s))`.

Shaded plots are always drawn in braille. The shade has its own face, `plot-shade`, a dimmer version of the first series color, so the curve stays visible on top of it.

## Heatmaps

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
| `plot-shade` | the area under a curve in `plot-function` and `plot-normal` |
| `plot-heat-0` … `plot-heat-7` | heatmap colors, low to high (foreground) |
| `plot-heat-0-bg` … `plot-heat-7-bg` | the same ramp as backgrounds |

The series, shade, and heat faces use the bundled **Macros Plot** font through `set-face-font`, so chart columns stay aligned with the editor font. See [Themes & faces](/docs/themes-and-faces#face-attributes).

## Low-level primitives

The chart functions are built on a few Rust primitives you can call directly:

- `(textplot-render width height mode ops)` draws a list of operations on a `width × height` character grid and returns one `(text spans)` row per line. `mode` is `braille`, `block`, `quadrant`, or `half`. Operations use dot coordinates (top-left origin, y grows downward): `("point" x y face)`, `("line" x0 y0 x1 y1 face)`, `("rect" x0 y0 x1 y1 face)`, `("polyline" face (x0 y0 x1 y1 …))`, and `("text" col row string face)` in character cells.
- `(textplot-sparkline values lo hi)` and `(textplot-hbar value max width)` return glyph strings.
- `(textplot-ticks lo hi n)` returns about `n` evenly spaced `(value label)` axis ticks.
- `(textplot-nice-range lo hi n)` widens `(lo hi)` to whole tick steps, so both axis ends get a label. Line, scatter and function plots do this unless you pass `'y-min` or `'y-max`; histogram count axes always do.
- `(textplot-format v)` formats a number compactly: `950`, `12.3k`, `3.4M`.
