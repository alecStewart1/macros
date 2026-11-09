---
title: User options
description: Declare customizable Scheme variables with defoption — docstring, type, and default become part of C-h v and M-x customize-option.
---

# User options

`defoption` is the Emacs `defcustom`-style way to declare a Scheme variable that users may set. Like `defcustom`, it does not (and cannot) prevent `set!` — what it buys is **declared metadata**: a docstring, a type, and the standard value, wired into the help system so "what can I tune" is introspectable instead of tribal knowledge.

```scheme
(defoption *spell-program* "aspell"
  "External spell-checker program run by spell-mode's pipe backend."
  :type 'string)
```

That one form defines `*spell-program*` to `"aspell"` and registers it. `C-h v` (`describe-variable`) then shows the declared type, the default, the current value, and a *(changed from the default)* note when they differ; `M-x customize-option` (also `C-h O`) lists **only declared options** and sets one interactively.

## Semantics

- **VALUE is the default**, as in Emacs `defvar`: it is a constant expression, and your `init.scm` — which loads *after* the bundled modules — overrides it by re-defining the name.
- **`:type`** is metadata for the help layer: `'boolean`, `'integer`, `'float`, `'string`, `'symbol`, `'list`, `'alist`, `'plist`, or `'any` (the default when omitted). It is a *descriptor* of the shape the value can take — displayed by `describe-variable`, but **not enforced**: nothing validates or coerces the value at `set!` or `customize-option` time, so the label is a promise by the author, not a checked invariant.
- The docstring shows up in `C-h v` and as the dimmed annotation in `M-x` pickers (tagged `user option`).

## Declared vs. everything else

| Kind | Looks like | Where it shows up |
|------|-----------|-------------------|
| Editor option (Rust) | `(set-option "font-size" 16)` | `describe-option`, persisted |
| Declared user option | `(defoption *x* ...)` | `describe-variable` (with type/default), `customize-option` |
| Other `*star*` global | `(define *x* ...)` | `describe-variable` as a plain global |
| Internal state | `(define mod--state ...)` | nowhere — the `--` prefix keeps it out of the pickers |

Bundled modules follow this convention: anything you should tune is either an [editor option](configuration) or a declared user option; `--`-prefixed names are private.

## For module authors

Declare every variable a user might reasonably set with `defoption`; keep scratch state, caches, and handles under a `mod--` name. A `defoption` in a bundled module needs `defoption` in its `;; requires:` header. Value defaults must be constant expressions, and (a Steel expander limitation) cannot contain dotted pairs — build those with `cons` if needed.
