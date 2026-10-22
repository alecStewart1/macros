---
title: Abbreviations
description: Emacs-style abbrevs — type a short word, expand it into something longer on demand.
---

# Abbreviations

Emacs-style abbreviations (`abbrev.el`). An abbrev is a short word that expands to something longer — `teh` → `the` — when you ask it to. Expansion is **always explicit**: there is no typing-time auto-expansion, so nothing ever changes behind your back while you type.

## Expanding

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-x '</kbd> | `expand-abbrev` | Replace the word before point with its expansion |

Case works like Emacs: type `TEH` and you get `THE`, type `Teh` and you get `The`, otherwise the expansion is inserted exactly as defined. A prefix predicate or syntax-context rule can silently decline to expand (see below).

When leaving insert mode in [evil](/docs/modal-editing) or [helix](/docs/helix) you can also expand automatically:

```scheme
(set-abbrev-expand-on-insert-exit #t)
```

The expansion then happens silently on <kbd>esc</kbd> — see [Clashing with snippets](#clashing-with-snippets) before turning this on.

## Defining abbrevs

From your `init.scm`:

```scheme
(define-abbrev 'global-abbrev-table "teh" "the")        ; every mode
(define-abbrev 'rust-mode-abbrev-table "fnr" "fn rec")  ; rust-mode only
(define-global-abbrev "btw" "by the way")               ; shorthand for global
```

Every mode has an abbrev table (`<mode>-abbrev-table`, created on demand) and `global-abbrev-table` is shared by all modes. Lookup order is the local table first, then the global one.

A hook runs after a successful expansion — a procedure or a quoted function name, called with the expansion:

```scheme
(define (my-hook expansion) (message "..."))
(define-abbrev 'global-abbrev-table "brb" "be right back" 'my-hook)
```

You can also declare a whole table at once (handy for mode groups):

```scheme
(define-abbrev-table 'rust-mode-abbrev-table
  '(("fnr" "fn rec")
    ("pdbg" "println!(\"{:?}\", $0)"))
  "Rust abbrevs")
```

Tables can inherit from other tables with `:parents`; listed tables are tried (in order) after the local one and before the global one:

```scheme
(define-abbrev-table 'rust-mode-abbrev-table '()
  nil '(:parents '(prog-mode-abbrev-table)))
```

## Properties

Pass a property list as the last argument:

```scheme
(define-abbrev 'global-abbrev-table "ie" "i.e." #f
               '(:case-fixed #t))
```

- `:case-fixed` — insert the expansion exactly as defined; no case folding or capitalization.
- `:enable-function` — a predicate (procedure or `'symbol`, called with the abbrev) consulted before expanding; returning `#f` declines silently.
- `:ts-context` — expand only inside a matching tree-sitter node kind, e.g. `"@comment"` or `"@comment @string"` (the `@` is optional; `"comment"` matches `line_comment`, `block_comment`, …). The enclosing node chain is cached with a short debounce, so the check can be one edit cycle stale, and in a buffer without a tree-sitter grammar the abbrev never expands.
- `:system` — see the next section.

## System abbreviations

An abbrev defined with `:system #t` is a *system* abbreviation, like in Emacs:

```scheme
(define-abbrev 'global-abbrev-table "sl" "sort lines" #f '(:system #t))
```

- It is **never saved** to your abbrev file — define system abbrevs in `init.scm`, which runs at every startup anyway.
- It **will not overwrite** a user abbrev of the same name. If you have defined `sl` yourself, the system definition is skipped with a message in the echo area. Pass `:system 'force` to override your abbrev anyway.

Abbrevs declared through `define-abbrev-table` are system abbrevs by default — that is why they live in your config and never pollute the abbrev file. A definition that names its own `:system` keeps that value, so `(:system #f)` opts a single entry out to a normal savable abbrev.

## Saving

Your abbrevs auto-save on exit to `abbrev-file-name` (`~/.config/macros/abbrevs.scm` by default), silently — no messages, and no file at all until you have at least one savable abbrev. The file is Scheme data: exactly the `(define-abbrev …)` forms that rebuild the tables, so you can read, edit, or version it like any config.

```scheme
(write-abbrev-file)            ; save now
(read-abbrev-file)             ; reload
(set-save-abbrevs #f)          ; disable auto-save
(set-abbrev-file-name "~/sync/abbrevs.scm")
```

Abbrevs whose hook or predicate is an anonymous procedure can't be serialized; they are skipped with a comment naming them. Use quoted function names for anything that must persist.

## Completion

`abbrev-mode` (M-x) adds your abbrevs as a completion source in the [completion popup](/docs/completion): type a prefix and the expansions show up as candidates. The local table shadows the global one, and `set-abbrev-capf-min-prefix` sets the shortest prefix that completion answers (default 2).

## Clashing with snippets

Abbrevs and [snippets](/docs/snippets) occupy the same territory: both replace the word before point with something longer, and an abbrev expansion can even contain snippet syntax like `$0`. Which system owns which word is up to you — Macros doesn't arbitrate — so keep a few things in mind:

- Keep short trigger words in **one** system only; a word defined as both expands by whichever runs first.
- `set-abbrev-expand-on-insert-exit` rebinds <kbd>esc</kbd> in the evil/helix insert keymaps while enabled. If you navigate snippet fields with <kbd>esc</kbd> or use `C-c C-f`/`C-c C-b` heavily, leave it off and expand with <kbd>C-x '</kbd> instead.
- Abbrev completion and snippet completion are both completion-at-point functions; the order you enable them in `init.scm` decides their priority in the popup.
