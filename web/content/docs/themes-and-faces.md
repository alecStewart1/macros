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
