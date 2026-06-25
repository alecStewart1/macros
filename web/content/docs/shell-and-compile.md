---
title: Shell & compile
description: Run commands into a compilation buffer, walk errors, grep, and use the basic shell.
---

# Shell & compile

Macros integrates with external processes the Emacs way: a **`compile`** command that runs a build into a read-only buffer and parses its errors, plus error navigation that jumps straight to the offending source line, and a basic interactive shell.

## compile

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-c c</kbd> | `compile` | Prompt for a command and run it |
| | `recompile` | Re-run the last compile (also <kbd>g</kbd> in the buffer) |

Output streams into a `*compilation*` buffer. When the process finishes, Macros parses each line for an error location and faces it. It recognizes the common compiler and grep formats (the first matching pattern wins, like Emacs's `compilation-error-regexp-alist`).

### In the compilation buffer

| Key | Action |
| --- | --- |
| <kbd>Enter</kbd> | visit the source for the error on this line |
| <kbd>g</kbd> | recompile |
| <kbd>n</kbd> / <kbd>p</kbd> | move |
| <kbd>q</kbd> | close |

## Walking errors anywhere

| Key | Command |
| --- | --- |
| <kbd>M-g n</kbd> | `next-error` |
| <kbd>M-g p</kbd> | `previous-error` |

`next-error` / `previous-error` step through the parsed error list without needing the compilation buffer focused. They're shared with [diagnostics](/docs/diagnostics) and grep: the most recently used source claims them.

## grep

Project grep is described under [Projects & search](/docs/projects). Its results buffer (`grep-mode`) is wired to the same `compile-visit` / `next-error` machinery, so jumping to a match works identically.

## Shell

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-c s</kbd> | `shell` | Open a basic interactive shell buffer |
| <kbd>Enter</kbd> | `shell-send-input` | Send the current input line |

The `shell` command gives you a simple line-oriented shell inside a buffer. For a **full** terminal — ANSI/VT, curses apps, REPLs, `htop` — use the [embedded terminal](/docs/terminal) instead.

## Running processes from Scheme

All of this is built on `call-process`, which you can call directly in your own commands:

```scheme
(define (current-branch)
  (call-process "git" '("rev-parse" "--abbrev-ref" "HEAD")))
```

See [Scripting with Steel](/docs/scripting) for more on `call-process` and the async `make-process`.
