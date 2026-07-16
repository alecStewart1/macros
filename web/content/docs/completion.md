---
title: Completion
description: The corfu-style in-buffer completion popup and pluggable completion-at-point functions.
---

# Completion

Macros has a **corfu-style** in-buffer completion popup. It can appear automatically as you type or on demand, and it draws candidates from your own *completion-at-point functions* (CAPFs) and from the language server.

## Triggering completion

- **On demand:** <kbd>M-i</kbd> (`completion-at-point`). In modal editing, <kbd>Space c</kbd>.
- **Automatically:** when `completion-auto` is on (the default), the popup appears after you've typed `completion-auto-prefix` word characters.
- **On a trigger character:** the popup opens immediately — no word prefix — after a *trigger character* such as `.` for member access (`List.` → members). Trigger characters come from the language server plus a configurable per-language set (see [Options](#options)); this is what makes `Stdlib.` complete on the dot.

## Popup keys

These bindings are active **only while the popup is open**:

| Key | Action |
| --- | --- |
| <kbd>C-n</kbd> / <kbd>↓</kbd> | next candidate |
| <kbd>C-p</kbd> / <kbd>↑</kbd> | previous candidate |
| <kbd>Tab</kbd> / <kbd>Enter</kbd> | accept |
| <kbd>C-g</kbd> | abort |

## Options

Set these in your `init.scm` (defaults shown). Values are typed — numbers and booleans (`#t` / `#f`) — though a quoted string works too:

```scheme
(set-option "completion-style"       "popup")  ; "popup" or "minibuffer"
(set-option "completion-auto"        #t)       ; auto-popup while typing
(set-option "completion-auto-prefix" 1)        ; min chars before auto-popup
(set-option "completion-max-rows"    10)       ; visible rows
(set-option "completion-show-detail" #t)       ; show signature/type
(set-option "completion-show-kind"   #t)       ; show kind tag (fn, var, kw…)
(set-option "completion-capf"        #t)       ; set #f for LSP-only
```

**Trigger characters.** A trigger character auto-opens the popup with no word prefix. The language server advertises its own, and the editor adds `.` for every language by default — *added to*, not replacing, the server's set — so member access completes on the dot even under a server that advertises no trigger characters (ocamllsp lists none). Add more per language, or set `""` to rely on server-advertised triggers only:

```scheme
(set-option "completion-trigger-chars:rust" ".:")   ; each char triggers; "" disables
```

The key is `completion-trigger-chars:<language-id>` (a [per-language option](/docs/configuration#per-mode-options)); a bare `completion-trigger-chars` applies to every language.

## completion-at-point functions (CAPFs)

`completion-at-point` tries each registered Scheme CAPF in order, then falls back to the language server. A CAPF is a function of the prefix word that returns one of:

- a flat list of candidate strings (the word before point is replaced), or
- `(start end candidate-list)` to control the replaced byte range, or
- `'()` / `#f` to decline (the next CAPF, then LSP, is tried).

Each candidate is either a bare string or a `(text annotation)` pair, where the annotation renders dimmed beside the candidate (marginalia-style).

### Registering a CAPF

```scheme
;; Complete from a fixed word list whenever the prefix is non-empty.
(define (my-words prefix)
  (list "lambda" "let" "letrec"
        (list "define" "special form")))   ; annotated candidate

(add-completion-at-point-function "my-words")
```

To use the language server exclusively and skip the CAPF path entirely:

```scheme
(set-option "completion-capf" #f)
```

See [Language servers](/docs/lsp) for LSP-driven completion, and [Scripting with Steel](/docs/scripting) for the primitives a CAPF can call.
