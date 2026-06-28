---
title: Packages
description: Install third-party Steel extensions straight from a git repository with package-install.
---

# Packages

`package-install` is a tiny git-based package manager. Point it at a repository and Macros clones it into your config directory on first launch, optionally pins it to a commit or tag, and loads its entry file — so your `init.scm` can pull in extensions other people have written. Later launches skip the clone and just load, so startup stays fast and works offline.

It builds on existing primitives (git via `call-process`, the file predicates, and Steel's `load`), so there's nothing to enable — `package-install` is available in your `init.scm`.

## Installing a package

The minimal form is just a URL:

```scheme
;; Clone to <config-dir>/packages/macros-rainbow on first run, then load its
;; entry file — macros-rainbow.scm, the default (= the package name).
(package-install "https://github.com/user/macros-rainbow")
```

Packages live under your config directory — `~/.config/macros/packages/` (or `$XDG_CONFIG_HOME/macros/packages`, `%APPDATA%/macros/packages` on Windows). The exact path is available from Scheme as `(packages-directory)`.

## Pinning a version

Track a specific commit, tag, or branch with `'ref`. **Pinning is recommended** — it makes your config reproducible and keeps an upstream change from silently altering your editor:

```scheme
(package-install "https://github.com/user/macros-rainbow"
  'ref "v0.3.1")            ; commit, tag, or branch, checked out after clone
```

The clone happens only when the package directory is absent, so only the *first* launch touches the network. A pinned `'ref` is re-checked-out on every launch, so editing it in `init.scm` re-pins on the next start.

## Options

`package-install` takes a URL followed by quoted-symbol options (the same style as `lsp-language`):

| Option | Meaning |
| --- | --- |
| `'name` | Install name and clone-directory name. Defaults to the repo name derived from the URL (last path segment, minus `.git`). |
| `'ref` | Commit, tag, or branch to check out after cloning. |
| `'entry` | File to load, relative to the repo root. Defaults to `"<name>.scm"`. |
| `'depends` | List of package names that must already be installed earlier in `init.scm`. |
| `'config` | A `(lambda () …)` run *after* the entry loads — put your settings here. |

### A custom entry file

```scheme
(package-install "https://github.com/user/macros-bigmode"
  'ref   "a1b2c3d"
  'entry "init.scm")       ; load this instead of <name>.scm
```

### Configuring a package after it loads

The `'config` thunk runs once the entry file has loaded, so any commands or
options the package defines are available:

```scheme
(package-install "https://github.com/user/macros-snippets"
  'ref    "v1.0"
  'config (lambda ()
            (define-key "prog" "ctrl-c ctrl-s" 'snippet-expand)
            (set-option "snippet-dir" "~/.snippets")))
```

Note that the entry file **and** `'config` run on **every launch**, not just the
first install — only the git clone is once-only. They configure the live editor,
which starts fresh each session, so keep them idempotent (defining commands,
binding keys, and setting options all re-run cleanly).

### Dependencies

`'depends` enforces **ordering**: each named package must have been installed by an earlier `package-install` in your `init.scm`. Nothing is auto-fetched — if a dependency is missing, the package is skipped with a message rather than installed implicitly. Declare dependencies first:

```scheme
;; Declare the dependency first…
(package-install "https://github.com/user/macros-lib-async"
  'name "async")

;; …then the package that needs it. `async` is guaranteed loaded before
;; macros-http's entry and config run.
(package-install "https://github.com/user/macros-http"
  'ref     "v2.1.0"
  'depends '("async")
  'config  (lambda () (set-option "http-timeout" "30")))
```

## Managing installed packages

| Command | What it does |
| --- | --- |
| `M-x package-list` | Echo the installed packages. |
| `M-x package-update` | Fast-forward every installed package to its upstream and report. |

`package-update` advances branch-tracking installs; a package pinned to a commit or tag is re-pinned from your `init.scm` on the next launch, and a pinned checkout that can't fast-forward is reported as failed and left untouched.

To **remove** a package, delete its directory under `(packages-directory)` and remove its `package-install` line from `init.scm`.

## Creating a package

A package is just a git repository containing Steel that uses the editor's scripting API. There's no manifest and no build step.

### Layout and the entry point

The **default entry point is a file named after the package, at the repo root**: `<name>.scm`, where `<name>` is the package name — the last path segment of the repo URL with `.git` removed, or the installer's `'name` override. So a repo at `github.com/you/macros-foo` is loaded from `macros-foo.scm`:

```
macros-foo/
  macros-foo.scm      ← loaded by default
  README.md
```

Name the entry file anything you like and have users point at it with `'entry "init.scm"`, but matching the repo name is the zero-config convention. A minimal entry:

```scheme
;; macros-foo.scm — define a command and bind it. define-command defines the
;; command AND makes it M-x-discoverable in one form.
(define-command (foo-hello)
  (message "Hello from macros-foo!"))
(define-key "global" "ctrl-c f" 'foo-hello) ; optional default binding
```

### What you can do in the entry

The entry is `load`ed into the **shared global engine**, so it has the same API your `init.scm` does — `define-key`, `set-option`, `set-face`, `add-hook`, `register-command`, the buffer/point read bridges, `call-process`, tree-sitter and LSP requests, and everything else in the [Scripting](/docs/scripting) reference. Writing a package is writing config that happens to live in someone else's repo.

A few conventions that make a package well-behaved:

- **Namespace your globals.** Top-level `(define …)` land in the one shared global scope, so prefix your identifiers (`foo-…`, and `foo--…` for internals) to avoid colliding with the editor or other packages.
- **Register interactive commands** with `register-command` so they appear in `M-x`.
- **Document with a `;;` comment** immediately above each `(define …)` — the editor scrapes it when your package loads, so your functions and variables get help text for free and become findable through `describe-function`, `apropos`, and the live `helm-apropos` search (<kbd>C-h C-a</kbd>), whose <kbd>Enter</kbd> jumps straight to your source. See [Getting help](/docs/getting-help).
- **Stay idempotent.** Your entry (and the user's `'config` thunk) runs on *every* launch, not just install — see [Configuring a package after it loads](#configuring-a-package-after-it-loads). Defining commands, binding keys, and setting options are all naturally re-runnable; avoid one-time side effects like appending to a file.
- **Don't bind keys in `"global"` aggressively** — prefer a mode keymap or leave bindings to the user's `'config`, so installing your package doesn't clobber their keys.

### Multi-file packages

Steel's `load` resolves relative paths against the process working directory, **not** the entry file — so `(load "helper.scm")` from your entry will not reliably find a sibling. Use `*package-directory*`, which is bound to your package's own directory while the entry loads. Capture it at load time:

```scheme
;; macros-foo.scm
(define foo-dir *package-directory*)          ; capture immediately
(load (path-join foo-dir "helper.scm"))       ; now resolves correctly
(load (path-join foo-dir "commands.scm"))
```

Read `*package-directory*` only during loading (capture it into a local like `foo-dir` above). Don't read it later from inside a command — by then it holds whichever package loaded last.

### Publishing

Push to any git host the user's `git` can clone. **Tag releases** (`v1.0.0`, …) so users can pin a stable version with `'ref`; without a tag they can still pin a commit SHA or track a branch. The `macros-` repo prefix is conventional but not required.

## How loading is isolated

Each package's entry file (and its `'config` thunk) is loaded inside an error
handler, so a single broken package reports its error to `*Messages*` rather
than aborting the rest of your `init.scm`. A package is only marked loaded — and
thus visible to a later `'depends` — once its entry loads without error.

## A note on trust

`package-install` runs code from a URL, exactly like any package manager. Install
from sources you trust, and prefer pinning a `'ref` so you know exactly which
commit you're running.
