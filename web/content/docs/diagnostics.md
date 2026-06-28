---
title: Diagnostics
description: A flycheck-style diagnostics list, gutter dots, and next-error navigation.
---

# Diagnostics

Macros surfaces LSP diagnostics two ways: a **gutter dot** beside each affected line, and a flycheck/flymake-style **`*diagnostics*` list** you can walk. Both are built on the `(diagnostics)` primitive, which records each problem as `line · column · severity · message`.

## Gutter dots

Lines carrying a diagnostic get a colored dot next to the line number — **red** for errors, **orange** for warnings, **blue** for info/hints. When a line has several, the worst severity wins. On by default; disable with:

```scheme
(set-option "diagnostics-gutter" #f)
```

## The diagnostics list

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-c e</kbd> | `list-diagnostics` | Open the `*diagnostics*` list |
| <kbd>Enter</kbd> | `diag-visit` | Jump to the source line |
| <kbd>n</kbd> / <kbd>p</kbd> | | Move within the list |
| <kbd>q</kbd> | `kill-buffer` | Close |

## next-error / previous-error

| Key | Command |
| --- | --- |
| <kbd>M-g n</kbd> | `next-error` |
| <kbd>M-g p</kbd> | `previous-error` |

These walk the **most recently used** error source — Macros unifies error navigation across diagnostics, [compilation](/docs/shell-and-compile), and grep, exactly like Emacs. Whichever list you last opened (`list-diagnostics`, `compile`, or a grep) claims `next-error`, so the same two keys step through whatever you're currently working on.

## Customizing

Toggle the gutter dots, or rebind the navigation keys, from your `init.scm`:

```scheme
(set-option "diagnostics-gutter" #t)               ; #f hides the gutter dots

(define-key "fundamental" "ctrl-c e" 'list-diagnostics)
(define-key "fundamental" "alt-g n" 'next-error)
(define-key "fundamental" "alt-g p" 'previous-error)
```
