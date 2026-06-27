---
title: Dired sidebar
description: An expandable dired tree in a side window, built on dired-subtree.
---

# Dired sidebar

The **dired sidebar** is a [Dired](/docs/dired) buffer pinned to a side window: an expandable directory tree where you both *navigate* and *operate* on files. Directories expand and collapse inline (the **dired-subtree** behavior), selecting a file shows it in the main editor while the tree stays put, and the usual dired file operations — mark, delete, rename, copy, mkdir — work on the entry under point.

It's the bundled file-tree, built on the reusable **dired-subtree** rendering engine (`render-buffer`, `set-line-icon`, and overlays) — entirely Scheme, no Rust support specific to it.

## Opening the sidebar

| Key | Command |
| --- | --- |
| <kbd>C-c t</kbd> | `dired-sidebar` |

## Keys

### Navigation

| Key | Command | Action |
| --- | --- | --- |
| <kbd>Enter</kbd> / <kbd>Tab</kbd> | `dired-sidebar-visit` | Expand/collapse a directory, or open a file in the main pane |
| <kbd>mouse-1</kbd> | `dired-sidebar-visit` | Same, with the mouse |
| <kbd>n</kbd> / <kbd>p</kbd> | | Move down / up |
| <kbd>g</kbd> | `dired-sidebar-refresh` | Re-read the tree |
| <kbd>q</kbd> | `dired-sidebar-quit` | Close the sidebar (and its pane) |

### File operations

| Key | Command | Action |
| --- | --- | --- |
| <kbd>d</kbd> | `dired-sidebar-mark` | Flag the file under point for deletion (shows a `D`) |
| <kbd>u</kbd> | `dired-sidebar-unmark` | Remove the flag |
| <kbd>x</kbd> | `dired-sidebar-do-delete` | Delete the flagged files (with confirmation) |
| <kbd>R</kbd> | `dired-sidebar-rename` | Rename the file under point |
| <kbd>C</kbd> | `dired-sidebar-copy` | Copy the file under point |
| <kbd>+</kbd> | `dired-sidebar-mkdir` | Create a directory (in the directory under point) |

As in dired, deletion is a two-step flag-then-execute: mark with <kbd>d</kbd>, then run <kbd>x</kbd>. These keys resolve the same way under evil-mode — the sidebar is a special buffer, so its only evil overlay is `j`/`k`, and the rest fall through to the mode's own keymap.

## Live refresh

The tree refreshes automatically when files are created, renamed, or deleted in the directories it shows — it watches the root and every expanded subdirectory and re-renders on change, in place, without stealing focus from the pane you're editing in.

## Icons

Each file is iconed by extension using Lucide SVG icons (`assets/icons/<name>.svg`). The extension → icon mapping is plain Scheme (`dired-subtree-icon`), so you can customize it freely in your config.

The tree opts out of the line-number gutter (it sets `line-numbers:dired-sidebar-mode` to `false`) — a good example of the [per-mode option](/docs/configuration#per-mode-options) pattern.

## Customizing

Its look is set with [per-mode options](/docs/configuration#per-mode-options) scoped to `dired-sidebar-mode`, which you can override in your `init.scm`:

```scheme
(set-option "line-numbers:dired-sidebar-mode" #f)       ; no line numbers (default)
(set-option "hl-line:dired-sidebar-mode"      #t)       ; highlight the current row
(set-option "word-wrap:dired-sidebar-mode"    #f)       ; truncate long names

(define-key "fundamental" "ctrl-c t" "dired-sidebar")   ; rebind the toggle
```

## Dired sidebar vs. Dired

The sidebar is a persistent, expandable tree you keep open beside your code, with file operations built in. The flat [Dired](/docs/dired) buffer (<kbd>C-x d</kbd>) is one directory at a time, focused on bulk file operations — the two complement each other.

Prefer the classic Treemacs sidebar? It now ships as a separate, opt-in package (`macros-treemacs`) you can install with [`package-install`](/docs/packages).
