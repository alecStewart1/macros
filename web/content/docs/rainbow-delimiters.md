---
title: Rainbow & tree-sitter
description: Depth-colored brackets driven by the live tree-sitter parse tree.
---

# Rainbow & tree-sitter

Macros colors nested brackets by their **nesting depth** — rainbow delimiters — using the live **tree-sitter** parse tree rather than raw characters. Because it asks the grammar for bracket *nodes*, brackets inside strings and comments are skipped automatically. That makes it correct for non-Lisp languages (C, JS, Rust, …) as well as S-expression languages.

It's **on by default**: it paints when a file opens and repaints (debounced) as you type.

## Configuration

All settable from your `init.scm` (loaded after the built-in defaults):

```scheme
;; Palette — any length; brackets cycle through it by depth.
(rainbow-set-colors '("#ff5555" "#50fa7b" "#bd93f9"))

;; Live-refresh debounce, in milliseconds.
(rainbow-set-delay 150)

;; Turn it off entirely.
(rainbow-disable)
```

Interactively: <kbd>M-x</kbd> `rainbow-delimiters` toggles it, and `rainbow-refresh` repaints the current buffer.

The palette is registered as theme faces `rainbow-1` … `rainbow-N`, so a [theme](/docs/themes-and-faces) can also define these colors.

## Sharing the overlay layer

Rainbow uses **overlay faces**, the same layer used by LSP semantic tokens, [Org](/docs/org-mode) headings, and Magit diff accents. Two features painting that layer in one buffer will conflict — so if you enable semantic tokens for a language, disable rainbow for that buffer (or vice versa). Rainbow already skips `.org` buffers so it never fights Org fontification.

## The tree-sitter bridge

Rainbow is built on a general bridge that exposes the parse tree to Scheme:

- `tree-sitter-query` — run a query against the buffer's parse tree (async, with a callback).
- `tree-sitter-node-at` — get the syntax node at a position.

These let you write your own structural features — structural motion, context-aware indentation, custom highlighting — on the same incremental parse tree the editor maintains. `rainbow.scm` in the source is a compact, readable example to start from. For adding highlighting to a new file type, bundling a grammar, and building a major mode, see [Languages & modes](/docs/languages); for the primitives, [Scripting with Steel](/docs/scripting).
