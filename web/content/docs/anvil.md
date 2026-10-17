---
title: Anvil REPL
description: An interactive Steel REPL backed by a second, independent interpreter — evaluate forms in an isolated, interruptible image.
---

# Anvil REPL

**Anvil** is an interactive Steel REPL — the SLIME-style way to develop Steel. It runs your code in a *second, fully independent* Steel interpreter, separate from the editor's own config/scripting engine. Forms you evaluate in Anvil live in their own image: experiments never leak into the running editor, the image can be restarted from scratch, and a runaway loop can be **interrupted** instead of freezing the editor.

> The name: you hammer Steel into shape on an anvil.

## Anvil vs. `eval-buffer`

Macros already has [in-editor evaluation](/docs/scripting): <kbd>M-:</kbd> (`eval-expression`), and in `macros-steel` (config) buffers <kbd>C-c C-e</kbd> (`eval-last-sexp`), <kbd>C-M-x</kbd> (`eval-defun`) and <kbd>C-c C-b</kbd> (`eval-buffer`). Those run against the editor's **own** engine — that's exactly what you want for live-editing your config, because a `define` immediately changes the running editor. In plain `steel` buffers the same keys evaluate in Anvil instead.

Anvil is the opposite trade-off, for when you want a clean room:

| | In-editor eval | Anvil REPL |
|---|---|---|
| Engine | The editor's config engine | A separate, dedicated interpreter |
| State | Shared with the live editor | Isolated; restartable |
| Runaway loop | Wedges editor scripting | Interruptible (<kbd>C-c C-c</kbd>) |
| Editor primitives | Available (`insert`, `define-key`, …) | Not registered — pure values |
| Printed output | Goes to the editor | Captured and shown with the result |
| Introspection | Static (LSP, compile-only) | Live image (`apropos`, `doc`, `macroexpand`) |

Use in-editor eval to hack on your config; use Anvil to test pure Steel code in isolation without risking the running editor.

## Opening the REPL

<kbd>M-x</kbd> `anvil-repl` opens the `*anvil-repl*` buffer. Type a form and press <kbd>RET</kbd>. If the form is complete, it's evaluated and the result is written below it as a `;; => …` comment line. If it isn't complete yet (an open paren or string), <kbd>RET</kbd> starts an indented continuation line, so multi-line definitions work naturally:

```scheme
(+ 1 2)
;; => 3
(define (square x)
  (* x x))
;; => (no value)
(map square '(1 2 3 4))
;; => (1 4 9 16)
```

Everything the REPL writes is a `;;` comment. That includes captured output, errors (`;; error: …`) and every line of a multi-line value, so the transcript is never mistaken for code. Results are inserted just above the input you're typing. You can start the next form while a slow one is still running and its output won't land in the middle of your typing.

- <kbd>M-p</kbd> / <kbd>M-n</kbd> step back and forth through the forms you've submitted.
- <kbd>RET</kbd> on an earlier form in the transcript copies it down to the input, ready to edit and resubmit, the way SLIME does.

From a Steel source buffer you can pop straight to the REPL with <kbd>C-c C-z</kbd>.

## Interrupting a runaway evaluation

If you evaluate something that never returns — an infinite loop or unbounded recursion — press <kbd>C-c C-c</kbd> to stop it. The evaluation unwinds with an error and the REPL stays alive for the next form:

```scheme
(define (spin) (spin))
(spin)            ; ... hangs ... press C-c C-c
;; error: Interrupted by user
```

This works for *any* Steel loop, including raw tail recursion like the above. Steel's VM checks an interrupt flag on every instruction, so there's nowhere for a loop to hide.

The one thing an interrupt can't reach is code blocked inside a native call that runs no instructions, such as `(time/sleep-ms 60000)` or a read from stdin. For those, use <kbd>C-c C-k</kbd> (below). It abandons the stuck evaluation and gives you a fresh image immediately.

## Restarting the image

<kbd>C-c C-k</kbd> (`anvil-repl-reset`) throws away the interpreter and builds a fresh one. Every `define` you made is gone, as if you'd just started Anvil. Use it to clear accumulated state, to confirm code works from a clean slate, or to get out of an evaluation stuck in a blocking call. The interpreter is built in the background on first use, so Anvil costs nothing until you use it. If the interpreter itself panics, the error is reported and the image restarts automatically.

## Evaluating from a source buffer

While editing a `.scm` file (`steel` or `macros-steel` mode), you can send code into the Anvil image and see the result echoed in the minibuffer, without leaving your buffer:

- <kbd>C-x C-e</kbd> / <kbd>C-c C-e</kbd> (in `steel`) — `anvil-eval-last-sexp`: evaluate the expression before point, or the active region.
- <kbd>C-c C-n</kbd> — `anvil-eval-region`: evaluate the active region.
- <kbd>C-M-x</kbd> (in `steel`) — `anvil-eval-defun`: evaluate the **top-level form at point** (no need to select it). In `macros-steel`, <kbd>C-M-x</kbd> is the live `eval-defun`; use <kbd>M-x</kbd> `anvil-eval-defun` there.
- <kbd>C-c C-b</kbd> (in `steel`) — `anvil-eval-buffer`.
- <kbd>C-c C-l</kbd> — `anvil-load-file`: `(load …)` the file you're visiting into the image — the SLIME "compile/load file" feel. Because it loads by path, relative `load`/`require` inside the file resolve against the file's directory.
- <kbd>M-x</kbd> `anvil-eval-buffer` — evaluate the whole buffer's text (works even on an unsaved buffer).

```scheme
;; Point anywhere inside this form, then C-M-x:
(define (square x) (* x x))
;; minibuffer: anvil => #<function:square>
```

### Captured output

`(display …)`, `(displayln …)`, and friends are **captured** and shown with the result, instead of vanishing to the process's stdout. So print-debugging works as you'd expect:

```scheme
(begin (displayln "computing…") (+ 2 3))
;; anvil:
;; computing…
;; 5
```

Each evaluation starts with a fresh output buffer, so you only ever see *this* form's output.

## Introspecting the live image

These commands answer against the **actual running image** — its real bindings, docstrings, and macros — not a static analysis of your source. (That distinction matters: the [LSP](/docs/lsp) completion/diagnostics you get while editing come from a separate compile-only engine; these come from Anvil itself.)

- <kbd>C-c C-d</kbd> — `anvil-doc`: show the docstring for the symbol at point. In `macros-steel` buffers (your config), <kbd>C-c C-d</kbd> stays bound to the editor-API docs (`macros-eldoc`); use <kbd>M-x</kbd> `anvil-doc` there.
- <kbd>C-c C-s</kbd> — `anvil-signature`: the one-line signature for the symbol at point, in the echo area.
- <kbd>C-c C-x</kbd> — `anvil-macroexpand`: macroexpand the region (or the form at point) and show the expansion. Uses the *un-optimized* expansion, so you see the real macro shape — `(when #t 1)` reveals its `if`, rather than being folded to `1`.
- <kbd>C-c C-a</kbd> — `anvil-apropos`: prompt for a pattern and list every live binding whose name contains it. Because it queries the image, anything you've just `define`d shows up immediately.

Results that don't fit the minibuffer (doc, macroexpand, apropos) are rendered into an `*anvil*` buffer.

```scheme
(define my-helper 42)
;; C-c C-a, type "my-hel" → *anvil* lists:  my-helper
```

### Signatures while you type

In the REPL, the echo area shows the live image's signature for the function whose arguments you're typing. Type `(string-append ` and its documented signature appears. Your own `define`s show up too, as bound but undocumented. Lookups are skipped while an evaluation is running, so they never queue behind it.

### Completion

Inside the `*anvil-repl*` buffer, <kbd>M-i</kbd> (`completion-at-point`) completes the symbol at point **against the live image** — not a static analysis. So it offers every built-in *and* everything you've `define`d this session, the moment you define it:

```scheme
(define (frobnicate x) (* x 2))
;; later, type  (frob  then M-i  →  frobnicate
```

This is the SLIME model: completion asks the running interpreter, the same way `anvil-apropos` does, rather than the editor's compile-only [LSP](/docs/lsp). Completion is symbol-aware, so hyphenated/punctuated names like `string->number` and `set!` complete as one token. The popup narrows as you keep typing.

> **Not here:** an interactive debugger with restarts. Steel unwinds on error rather than offering a condition/restart system, so a runaway or failing form surfaces as an error string (and stays interruptible), not a debugger stack.

## Command & key reference

| Key | Command | Action |
|---|---|---|
| <kbd>M-x</kbd> `anvil-repl` | `anvil-repl` | Open / switch to the `*anvil-repl*` buffer |
| <kbd>RET</kbd> (in REPL) | `anvil-send-input` | Evaluate the input once it's complete; otherwise continue it on a new line |
| <kbd>M-p</kbd> / <kbd>M-n</kbd> (in REPL) | `anvil-history-previous` / `-next` | Recall earlier input |
| <kbd>C-c C-c</kbd> (in REPL) | `anvil-repl-interrupt` | Interrupt the current evaluation |
| <kbd>C-c C-k</kbd> (in REPL) | `anvil-repl-reset` | Restart the image (fresh interpreter) |
| <kbd>M-i</kbd> (in REPL) | `completion-at-point` | Complete the symbol at point against the live image |
| <kbd>C-c C-z</kbd> (in `steel`) | `anvil-repl` | Pop to the REPL from a source buffer |
| <kbd>C-c C-n</kbd> (in `steel`) | `anvil-eval-region` | Evaluate the region in the Anvil image |
| <kbd>C-x C-e</kbd> / <kbd>C-c C-e</kbd> (in `steel`) | `anvil-eval-last-sexp` | Evaluate the expression before point (or the region) |
| <kbd>C-M-x</kbd> (in `steel`) | `anvil-eval-defun` | Evaluate the top-level form at point |
| <kbd>C-c C-b</kbd> (in `steel`) | `anvil-eval-buffer` | Evaluate the whole buffer |
| <kbd>C-c C-l</kbd> (in `steel`) | `anvil-load-file` | `(load …)` the visited file into the image |
| <kbd>C-c C-d</kbd> (in `steel`) | `anvil-doc` | Docstring for the symbol at point |
| <kbd>C-c C-s</kbd> (in `steel`) | `anvil-signature` | One-line signature for the symbol at point |
| <kbd>C-c C-x</kbd> (in `steel`) | `anvil-macroexpand` | Macroexpand the region / form at point |
| <kbd>C-c C-a</kbd> (in `steel`) | `anvil-apropos` | List live image bindings matching a pattern |

The source-buffer commands are bound in both the `steel` and `macros-steel` modes, except <kbd>C-c C-d</kbd> (see above). All of these are ordinary Steel commands defined in the bundled runtime — see [Scripting with Steel](/docs/scripting) to rebind them or build on `anvil-eval`.
