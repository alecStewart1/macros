---
title: Treemacs
description: An expandable file-tree sidebar with icons.
---

# Treemacs

Treemacs is a **file-tree sidebar**: a recursive, expandable directory view living in its own pane, with per-extension icons. It's built on `render-buffer`, `set-line-icon`, and `open-file-other-window` — selecting a file shows it in the main editor while the tree stays put.

## Opening the sidebar

| Key | Command |
| --- | --- |
| <kbd>C-c t</kbd> | `treemacs` |

## Keys

| Key | Command | Action |
| --- | --- | --- |
| <kbd>Enter</kbd> / <kbd>Tab</kbd> | `treemacs-visit` | Expand/collapse a directory, or open a file in the main pane |
| <kbd>mouse-1</kbd> | `treemacs-visit` | Same, with the mouse |
| <kbd>n</kbd> / <kbd>p</kbd> | | Move down / up |
| <kbd>g</kbd> | `treemacs-refresh` | Re-read the tree |
| <kbd>q</kbd> | `treemacs-quit` | Close the sidebar (and its pane) |

## Icons

Each file is iconed by extension using Lucide SVG icons (`assets/icons/<name>.svg`). The extension → icon mapping is plain Scheme, so you can customize it freely in your config.

The sidebar opts out of the line-number gutter (it sets `line-numbers:treemacs-mode` to `false`) — a good example of the [per-mode option](/docs/configuration#per-mode-options) pattern.

## Customizing

The sidebar is on <kbd>C-c t</kbd>. Its look is set with [per-mode options](/docs/configuration#per-mode-options) scoped to `treemacs-mode`, which you can override in your `init.scm`:

```scheme
(set-option "line-numbers:treemacs-mode" #f)       ; no line numbers (default)
(set-option "hl-line:treemacs-mode"      #t)       ; highlight the current row
(set-option "word-wrap:treemacs-mode"    #f)       ; truncate long names

(define-key "fundamental" "ctrl-c t" "treemacs")   ; rebind the toggle
```

## Treemacs vs. Dired

Treemacs is for **navigation** — a persistent tree you keep open beside your code. For file *operations* (mark, delete, rename, copy) reach for [Dired](/docs/dired).
