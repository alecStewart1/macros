---
title: Terminal
description: A full VT/ANSI terminal emulator embedded in a buffer.
---

# Terminal

Macros embeds a real terminal emulator (backed by **alacritty**'s VT engine). Unlike the line-oriented [shell](/docs/shell-and-compile#shell), this is a full ANSI/VT terminal — curses apps, REPLs, `vim`, `htop`, and color all work.

## Opening a terminal

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-c C-t</kbd> | `term` | Open a terminal running your login shell |
| | `ansi-term` | Alias for `term` |
| | `term-run` | Open a terminal running a specific program |

`term` (and `ansi-term`) launches your login shell in a read-only `term-mode` buffer. `term-run` prompts for a program and runs that instead.

## Keys go to the shell

While a terminal buffer is focused, **almost every key is sent straight to the shell** — including <kbd>C-c</kbd>, <kbd>C-r</kbd>, the arrow keys, and <kbd>Tab</kbd>. The mouse wheel scrolls the terminal's scrollback.

The one exception is the editor prefix <kbd>C-x</kbd>, which is reserved so window and buffer commands still work:

| Key | Action |
| --- | --- |
| <kbd>C-x o</kbd> | switch to another pane |
| <kbd>C-x k</kbd> | kill the terminal buffer |
| <kbd>C-x 2</kbd> / <kbd>C-x 3</kbd> | split, e.g. to keep code and a terminal side by side |

Two more ways out of the shell:

- **The alt leader.** Under evil-mode, <kbd>M-SPC</kbd> starts any chord you bound with `evil-leader-set` — <kbd>M-SPC n o</kbd> runs your <kbd>SPC n o</kbd>. See [Modal editing](/docs/modal-editing).
- **Pass-through keys.** List any other chords the terminal should hand to the editor:

```scheme
(set-option "terminal-passthrough-keys" "alt-o ctrl-]")
(define-key "fundamental" "alt-o" 'other-window)
```

## When the shell exits

A terminal whose program has exited releases the keyboard, so normal editor keys work in the leftover buffer. `terminal-exit-hook` runs with the terminal's buffer name, so you can clean up automatically:

```scheme
(define (my-close-dead-terminal name)
  (let ((w (get-buffer-window name)))
    (when (and (>= w 0) (> (length (window-list)) 1))
      (select-window w)
      (delete-window))
    (kill-buffer-named name)))
(add-hook "terminal-exit-hook" 'my-close-dead-terminal)
```

## Multiple terminals

Each terminal gets a distinct buffer name, so you can run several at once. `make-terminal` reuses an existing terminal buffer if the name is already taken, so binding a key to a named terminal gives you a toggle-style "pop to my terminal".

## Customizing

`term` has no global key in Emacs, so Macros picks <kbd>C-c C-t</kbd> as a sensible default — rebind it in your `init.scm` like any other key:

```scheme
(define-key "fundamental" "ctrl-c enter" 'term)
```
