---
title: Themes & faces
description: Load themes, set faces, and control the line-number gutter and other chrome.
---

# Themes & faces

Macros styles text through **faces** (named foreground/background/weight bundles) and bundles them into **themes**. Both are Scheme, so you can load a theme, tweak individual faces, or write a theme of your own.

## Loading a theme

```scheme
(load-theme "macros-dark")   ; built-in themes live in scheme/themes/*.scm
```

Put `load-theme` in your `init.scm`. The two built-in themes are `"macros-light"` and `"macros-dark"`; the default config loads `"macros-dark"`.

## Setting faces

Override or define a face with `set-face`:

```scheme
;; Tweak a single face after loading a theme.
(set-face "comment" "#7a8a6a")
```

Faces are referenced by name throughout the editor — syntax categories, diff lines, headings, diagnostics severities, and the rainbow-delimiter palette are all faces. A theme that doesn't define a particular face falls back to the default foreground, so partial themes are fine.

## Face attributes

Beyond a foreground color, a face can carry extra attributes. Each is its own call, so set them after `set-face`:

```scheme
(set-face-bold "occur-match" #t)                  ; bold weight
(set-face-underline "link" #t)                    ; straight underline
(set-face-wavy "spell-error" #t)                  ; wavy underline (colors only the squiggle)
(set-face-background "plot-heat-3-bg" "#277f8e")  ; background painted behind the text
(set-face-font "plot-series-0" "Macros Plot")     ; font family for text under the face
```

A face with **only** a background (no `set-face` color) keeps the text color it lands on, so it can be layered under another face. The bundled **Macros Plot** font holds the [text chart](/docs/plotting) glyphs (braille, blocks, `■`) at exactly the editor font's cell width; the built-in themes point the `plot-*` faces at it.

Like colors, attributes are reset by `load-theme`, so a theme file must declare every attribute it relies on.

## The overlay-face layer

Some features paint **overlay faces** — sub-line color spans layered on top of the buffer. [Rainbow delimiters](/docs/rainbow-delimiters), LSP semantic tokens, [Org](/docs/org-mode) heading colors, and Magit diff accents all share this one layer. Two features that both want it in the same buffer will fight, so enable just one per buffer (e.g. disable rainbow for a buffer where you turn on semantic tokens).

## Line numbers & chrome

The display chrome is controlled with options (see [Configuration](/docs/configuration)):

```scheme
(set-option "line-numbers" #t)       ; gutter (per major mode)
(set-option "scroll-bar"   #t)
(set-option "cursor-style" "auto")   ; "auto" | "bar" | "block"
```

Line numbers are a per-mode option — toggle the current mode live with <kbd>M-x</kbd> `line-numbers-mode`, or scope it to one mode:

```scheme
(set-option "line-numbers:dired-sidebar-mode" #f)
```

`scroll-bar-mode` similarly toggles the scroll bar live.

## Rainbow palette

The rainbow-delimiter colors are registered as faces `rainbow-1` … `rainbow-N`, so a theme can style them and you can change the palette directly — see [Rainbow & tree-sitter](/docs/rainbow-delimiters).
