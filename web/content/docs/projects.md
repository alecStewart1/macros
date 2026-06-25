---
title: Projects & search
description: Projectile-style project navigation and ripgrep-backed find-file and grep.
---

# Projects & search

Macros follows the **projectile.el** pattern: a project is the directory tree above the file you're editing, found by walking up to a marker (like `.git`). On top of that, ripgrep's gitignore-aware walker powers fast file finding and project-wide grep — and it works in non-git directories too.

## Commands

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-c p f</kbd> | `projectile-find-file` | Find a file in the current project (Helm) |
| <kbd>C-c p p</kbd> | `projectile-switch-project` | Switch to another known project |
| <kbd>C-c p g</kbd> | `projectile-grep` | Live grep across the project (Helm) |
| <kbd>C-c p G</kbd> | `projectile-grep-buffer` | Grep into a `grep-mode` results buffer |

The project root is shown in the mode line.

### Find file

`projectile-find-file` lists every file under the project root using ripgrep's walker (so `.gitignore` is honored). It's a static [Helm](/docs/helm) source — the file list is walked once when the picker opens, then filtered in memory as you type.

### Switch project

`projectile-switch-project` lets you pick a known project's root, makes it current, and immediately drops you into its find-file picker.

### Grep

`projectile-grep` is a **live** [Helm](/docs/helm) picker — the `helm-ag` / `counsel-rg` experience. Type a regexp and matches narrow in the panel as you type; <kbd>Enter</kbd> jumps to the file at that line, and <kbd>C-SPC</kbd> multi-marks like any Helm source. The search re-runs on every keystroke (above two characters), backed by ripgrep's gitignore-aware content searcher.

When you'd rather keep a persistent, walkable result set, `projectile-grep-buffer` (<kbd>C-c p G</kbd>) prompts for a pattern and dumps the matches into a `grep-mode` buffer instead:

| Key | Action |
| --- | --- |
| <kbd>Enter</kbd> | jump to the match |
| <kbd>g</kbd> | re-run the grep |
| <kbd>q</kbd> | close |

Those buffer results also feed `next-error` / `previous-error` (<kbd>M-g n</kbd> / <kbd>M-g p</kbd>) — see [Diagnostics](/docs/diagnostics).

## Tuning the search

The ripgrep walker is configured through `search-*` primitives in your `init.scm`. Because the walker is gitignore-aware by default, you usually don't need to configure anything — but you can prune, whitelist, or loosen it:

```scheme
(search-exclude "target")                       ; skip a directory or glob
(search-exclude "*.min.js")
(search-include "*.rs")                          ; whitelist: only walk matching files
(set-search-option "hidden" #t)                 ; include dotfiles
(set-search-option "respect-gitignore" #f)      ; walk ignored files too
(set-search-option "max-depth" 8)               ; cap recursion depth
```

The root resolver (`project-root`) is itself a Scheme function — `walk-up-to-marker` — so you can teach Macros about project markers beyond `.git` if your repos use something else.
