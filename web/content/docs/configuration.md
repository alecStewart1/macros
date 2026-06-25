---
title: Configuration
description: Where your config lives, how it loads, and the editor options you can set.
---

# Configuration

Macros has no settings panel. Your configuration is a **Steel program** — `init.scm` — evaluated at startup. It can set options, rebind keys, define commands, load themes, and call any of the editor's primitives.

## Config file location

The built-in defaults load first (they ship inside the binary). Then your personal `init.scm` loads and can override anything. Macros looks for it in the first of:

| Platform | Path |
| --- | --- |
| macOS / Linux | `~/.config/macros/init.scm` |
| with `$XDG_CONFIG_HOME` set | `$XDG_CONFIG_HOME/macros/init.scm` |
| Windows | `%APPDATA%\macros\init.scm` |

If none exists, the editor just uses its defaults. Create the file and start adding to it.

### Splitting your config across files

```scheme
;; Resolved relative to the config directory:
(include "keys.scm")
(include "lsp.scm")

;; Absolute paths work too:
(load "/Users/me/dotfiles/macros/extra.scm")
```

## Editor options

Set options with `(set-option "name" value)`. Values are **typed** — pass a number, a boolean (`#t` / `#f`), or a string, and the value is coerced to the option's type when you set it. (A quoted `"string"` works for any option too, so older configs keep running.) These are the defaults, set in the built-in config:

```scheme
;; Theme — see Themes & faces
(load-theme "macros-dark")

;; Typography
(set-option "font-size"   13)
(set-option "line-height" 20)
(set-option "tab-width"   4)

;; Completion popup (corfu-style) — see Completion
(set-option "completion-style"       "popup")  ; "popup" or "minibuffer"
(set-option "completion-auto"        #t)       ; pop up while typing
(set-option "completion-auto-prefix" 2)        ; min chars before auto-popup
(set-option "completion-max-rows"    10)
(set-option "completion-show-detail" #t)       ; signature/type beside name
(set-option "completion-show-kind"   #t)       ; kind tag (fn, var, kw, …)

;; Cursor
(set-option "cursor-style" "auto")  ; "auto" | "bar" | "block"
(set-option "cursor-width" 2)       ; bar width in px

;; Gutter & chrome
(set-option "line-numbers"  #t)
(set-option "scroll-bar"    #t)
(set-option "divider-width" 1)      ; width of the split-window divider

;; Helm selection UI — see Helm
(set-option "helm-fuzzy"  #t)       ; fuzzy (nucleo) vs substring matching
(set-option "helm-height" 0.5)      ; fraction of the window height
```

### Reading an option

Read an option back from Scheme with `(get-option "name")`. It returns the current value as a native boolean / number / string, or `#f` if the option isn't set:

```scheme
(when (get-option "completion-auto")
  (message "auto-completion is on"))
```

### Per-mode options

Append `:mode-name` to scope an option to one major mode. For example, the treemacs sidebar turns its line-number gutter off:

```scheme
(set-option "line-numbers:treemacs-mode" #f)
```

## Session state

Some state persists across restarts automatically, no configuration required:

- **Cursor position** — when you reopen a file, point jumps back to where you left it (Emacs `save-place-mode`).
- **Recent files** — the recent-files ring survives a restart (Emacs `recentf`).

Both are stored in your config directory, in `~/.config/macros/saveplace` and `~/.config/macros/recentf`, and updated as you save.

## Turning on modal editing

Macros uses standard Emacs keybindings by default. For Vim-style modal editing, enable `evil-mode` in your config:

```scheme
(evil-mode)
;; ...and (evil-disable) turns it back off.
```

This is vanilla `evil-mode` — plain Vim keys, no leader. See [Modal editing](/docs/modal-editing) for the full picture.

## A starter init.scm

```scheme
;; ~/.config/macros/init.scm
(load-theme "macros-dark")
(set-option "font-size" 14)

;; A personal binding: jump to definition.
(bind-key "fundamental" "ctrl-c d" lsp-find-definition)

;; A command of your own, callable with M-x insert-date.
(define (insert-date)
  (insert-string (call-process "date" '("+%Y-%m-%d"))))
```

Reload by restarting, or evaluate expressions live with <kbd>M-:</kbd> (`eval-expression`). For the full API, see [Scripting with Steel](/docs/scripting).
