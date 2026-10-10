---
title: Example config
description: A real, lived-in ~/.config/macros/init.scm — a Doom/Spacemacs-style evil + SPC-leader setup with per-mode LSP, Org tweaks, and installed packages, annotated piece by piece.
---

# Example config

[Configuration](/docs/configuration) covers the mechanics — where `init.scm` lives, how it loads, the options you can set. This page is the other half: a **complete, real config** you can read top to bottom and crib from.

It's a working `~/.config/macros/init.scm` — a Doom/Spacemacs-style `evil-mode` + `SPC`-leader setup ported over from an Emacs/`evil-leader` config. It turns on modal editing, wires a Spacemacs-style leader, defines a handful of personal commands, sets up per-major-mode LSP and Org bindings, and installs two packages.

> The paths in here (`~/workspace/org/…`) are personal — change them to yours. A leading `~/` expands to your home directory and works anywhere macros opens a file, so you don't need to spell out `/Users/you/…` in full.

## The whole file

```scheme
(set-option "font-family" "JetBrains Mono")
(set-option "auto-revert" #t)

;; Drop the built-in TODO vocabulary and define my own set of labels.
(set! *org-todo-keywords* '())
(set! *org-custom-keywords* (list "TODO" "BUG" "FEATURE" "IN-PROGRESS" "DONE"))

;; Optional: give some/all a color (omitted labels inherit the title color).
(set! *org-custom-keyword-colors*
  (list (cons "TODO"        "#fe8019")
        (cons "BUG"         "#fb4934")
        (cons "FEATURE"     "#83a598")
        (cons "IN-PROGRESS" "#fabd2f")
        (cons "DONE"        "#b8bb26")))
(org--register-custom-keyword-faces)

;; These labels now join the built-in C-c C-t rotation automatically. The
;; org-toggle-* commands below add keys that jump STRAIGHT to one label
;; (and toggle it off when pressed again).
(define (org-toggle-bug)     (org-set-keyword "BUG"))
(define (org-toggle-feature) (org-set-keyword "FEATURE"))
(define (org-toggle-idea)    (org-set-keyword "IDEA"))
(bind-key "org-mode" "ctrl-c ctrl-b" 'org-toggle-bug)
(bind-key "org-mode" "ctrl-c ctrl-f" 'org-toggle-feature)
(bind-key "org-mode" "ctrl-c ctrl-i" 'org-toggle-idea)

;;; ============================================================================
;;; EVIL MODE + SPC LEADER
;;; ============================================================================
;; Opt in to Vim modal editing. SPC is used as a Doom/Spacemacs-style leader by
;; binding "space …" chords in the evil-normal keymap (global) and in the
;; per-major-mode evil keymaps.
(evil-mode)

;; --- helper commands the leader binds to ------------------------------------
(define (my/scratch) (switch-to-buffer "*scratch*"))
(define (my/notes)   (open-file "~/workspace/org/notes.org"))

;; Helm picker over my org directory.
(define (my/org-files)
  (helm-from "~/workspace/org/"
             (list (list "Files" 'find-file-candidates 'open-file))))

;; Kill every buffer, then land on *scratch*.
(define (my/kill-all-buffers)
  (for-each (lambda (b) (kill-buffer-named b)) (buffer-names))
  (switch-to-buffer "*scratch*"))

;; Open this config.
(define (my/edit-config)
  (open-file "~/.config/macros/init.scm"))

;; Font size +/-. font-size is a string option, so read/coerce it, then write back.
(define (my/font-size-num)
  (let ((v (get-option "font-size")))
    (cond ((number? v) v)
          ((string? v) (or (string->number v) 17))
          (else 17))))
(define (my/font-inc) (set-option "font-size" (number->string (+ (my/font-size-num) 1))))
(define (my/font-dec) (set-option "font-size" (number->string (- (my/font-size-num) 1))))

;; Insert an R source block and drop the cursor on the empty body line.
(define (my/org-insert-src-r)
  (insert "#+BEGIN_SRC R :session :results none\n\n#+END_SRC\n")
  (previous-line) (previous-line))

;; Leader key — SPC by default. Change it here (before the bindings below) and
;; every leader chord moves with it, e.g. (evil-set-leader "comma").
(evil-set-leader "space")

;; --- global leader bindings -------------------------------------------------
;; Config
(evil-leader-set "c o" 'my/edit-config)

;; Project
(evil-leader-set "v u" 'compile)

;; Buffer / window management
(evil-leader-set "n k" 'my/kill-all-buffers)
(evil-leader-set "n n" 'next-buffer)
(evil-leader-set "n p" 'previous-buffer)
(evil-leader-set "n o" 'delete-other-windows)
(evil-leader-set "n d" 'kill-buffer-and-window)
(evil-leader-set "n b" 'helm-mini)
(evil-leader-set "n r" 'my/scratch)
(evil-leader-set "n a" 'my/notes)
(evil-leader-set "n m" 'my/org-files)
(evil-leader-set "n f" 'projectile-find-file)
(evil-leader-set "n g" 'projectile-grep)
(evil-leader-set "n t" 'dired-sidebar)
(evil-leader-set "n l" 'helm-buffers-list)

;; Window splits
(evil-leader-set "_" 'split-window-below)
(evil-leader-set "|" 'split-window-right)

;; Scrolling
(evil-leader-set "j" 'evil-scroll-down)
(evil-leader-set "k" 'evil-scroll-up)

;; Git / Magit
(evil-leader-set "m s" 'magit)
(evil-leader-set "m b" 'magit-blame-toggle)
(evil-leader-set "m f" 'magit-log-file)

;; Font size
(evil-leader-set "=" 'my/font-inc)
(evil-leader-set "-" 'my/font-dec)

;; --- per-major-mode leader: LSP ---------------------------------------------
(evil-leader-set-for-modes
  '("rust" "go" "typescript" "steel")
  (list (cons "g g" 'lsp-find-definition)
        (cons "g l" 'lsp-find-references)
        (cons "g p" 'xref-pop-marker)
        (cons "g r" 'lsp-rename)
        (cons "t"   'lsp-hover)
        (cons "g u" 'enable-lsp)))

;; --- per-major-mode leader: Org ---------------------------------------------
(evil-leader-set-for-modes
  '("org-mode")
  (list (cons "r r" 'org-babel-execute)
        (cons "u"   'org-todo)
        (cons "i"   'my/org-insert-src-r)))

;;; ============================================================================
;;; PACKAGES
;;; ============================================================================
;; macros-agent-shell — an ACP chat client + built-in Steel agent. MACROS_AGENT
;; picks the backend and MUST be set before the package loads. "claude" wraps a
;; local logged-in `claude` via the claude-code-acp adapter (no API key). Prereq:
;; `npm install -g @zed-industries/claude-code-acp` then `claude /login`.
(setenv "MACROS_AGENT" "claude")
(package-install "https://github.com/macros-app/agent-shell.git"
  'name "macros-agent-shell")          ; entry file: macros-agent-shell.scm
(evil-leader-set "m a" 'agent-shell)   ; SPC m a — start / focus agent-shell

;; macros-k8s — a magit-style kubectl dashboard: foldable, color-coded sections
;; for nodes / deployments / pods / services, with describe / logs / shell / scale
;; / delete actions. Requires kubectl on PATH. The 'config thunk runs after the
;; package loads — here it makes `s` shell into pods with /bin/sh.
(package-install "https://github.com/macros-app/k8s-dashboard.git"
  'name "macros-k8s"                  ; entry file: macros-k8s.scm
  'config (lambda () (set-option "k8s-shell" "/bin/sh")))
;; Launch with M-x k8s-dashboard.
```

That's the entire file. The rest of this page walks through what each part is doing.

## Options up top

```scheme
(set-option "font-family" "JetBrains Mono")
(set-option "auto-revert" #t)
```

Two overrides of the [built-in defaults](/docs/configuration#editor-options): a different font, and `auto-revert` turned on so buffers re-read themselves when their file changes on disk (a `git checkout`, a formatter, another editor). Everything else is left at its default — you only write the lines you want to change.

## Customizing Org TODO keywords

```scheme
(set! *org-todo-keywords* '())
(set! *org-custom-keywords* (list "TODO" "BUG" "FEATURE" "IN-PROGRESS" "DONE"))

(set! *org-custom-keyword-colors*
  (list (cons "TODO"        "#fe8019")
        (cons "BUG"         "#fb4934")
        (cons "FEATURE"     "#83a598")
        (cons "IN-PROGRESS" "#fabd2f")
        (cons "DONE"        "#b8bb26")))
(org--register-custom-keyword-faces)
```

This drops the stock TODO vocabulary and replaces it with a personal set, each with its own color. After `org--register-custom-keyword-faces`, those labels join the <kbd>C-c C-t</kbd> cycle automatically. The `org-toggle-*` commands below add keys that jump *straight* to one label:

```scheme
(define (org-toggle-bug)     (org-set-keyword "BUG"))
(bind-key "org-mode" "ctrl-c ctrl-b" 'org-toggle-bug)
```

`bind-key`'s first argument is the keymap — `"org-mode"` scopes the binding to Org buffers only. The command is given as a `'name` symbol (the command's name, quoted). See [Org mode](/docs/org-mode) for the full feature set.

## Turning on modal editing

```scheme
(evil-mode)
```

One line opts the whole editor into Vim-style modal editing. This is vanilla `evil` — Normal/Insert/Visual states, `hjkl`, operators, text objects — with no leader of its own. The leader is layered on top below. See [Modal editing](/docs/modal-editing).

## Personal commands

Before binding any keys, the config defines the commands those keys will call. A command is just a Scheme function — and once defined, it's also callable by name from <kbd>M-x</kbd>.

```scheme
(define (my/scratch) (switch-to-buffer "*scratch*"))
(define (my/notes)   (open-file "~/workspace/org/notes.org"))
```

A leading `~/` expands to your home directory, so `open-file` (and the file pickers below) resolve it the same way the shell would.

Some commands are richer. `my/org-files` opens a [Helm](/docs/helm) picker scoped to one directory:

```scheme
(define (my/org-files)
  (helm-from "~/workspace/org/"
             (list (list "Files" 'find-file-candidates 'open-file))))
```

`my/kill-all-buffers` iterates every buffer, kills it, then lands on `*scratch*`:

```scheme
(define (my/kill-all-buffers)
  (for-each (lambda (b) (kill-buffer-named b)) (buffer-names))
  (switch-to-buffer "*scratch*"))
```

And `my/font-inc` / `my/font-dec` bump the font size at runtime. Because `font-size` is stored as a string option, the helper reads it, coerces it to a number, and writes the new value back as a string:

```scheme
(define (my/font-size-num)
  (let ((v (get-option "font-size")))
    (cond ((number? v) v)
          ((string? v) (or (string->number v) 17))
          (else 17))))
(define (my/font-inc) (set-option "font-size" (number->string (+ (my/font-size-num) 1))))
```

These are ordinary Steel — `let`, `cond`, `for-each`, list operations. [Scripting with Steel](/docs/scripting) is the full reference; the [Anvil REPL](/docs/anvil) lets you prototype them live.

## The SPC leader

```scheme
(evil-set-leader "space")
```

`evil-set-leader` picks the leader key — `SPC` here. Every `evil-leader-set` chord below hangs off it. Change this one line (e.g. `(evil-set-leader "comma")`) and the entire leader map moves with it.

Global leader bindings map a chord to a command *by name* (a `'name` symbol, so your own `my/…` commands work exactly like built-ins):

```scheme
(evil-leader-set "c o" 'my/edit-config)     ; SPC c o — open this config
(evil-leader-set "n f" 'projectile-find-file)
(evil-leader-set "n g" 'projectile-grep)
(evil-leader-set "m s" 'magit)              ; SPC m s — git status
(evil-leader-set "=" 'my/font-inc)          ; SPC =   — bigger font
```

So <kbd>SPC n f</kbd> finds a file in the project and <kbd>SPC m s</kbd> opens [Magit](/docs/git). The grouping is Spacemacs-flavored — `n` for buffers/windows, `m` for git and package commands, `g` (per-mode, below) for code navigation.

## Per-mode leader bindings

`evil-leader-set-for-modes` binds leader chords that only exist in certain major modes — the analogue of `evil-leader/set-key-for-mode`. The LSP block gives every code mode the same navigation keys:

```scheme
(evil-leader-set-for-modes
  '("rust" "go" "typescript" "steel")
  (list (cons "g g" 'lsp-find-definition)
        (cons "g l" 'lsp-find-references)
        (cons "g r" 'lsp-rename)
        (cons "t"   'lsp-hover)
        (cons "g u" 'enable-lsp)))
```

In any Rust/Go/TypeScript/Steel buffer, <kbd>SPC g g</kbd> jumps to definition, <kbd>SPC g r</kbd> renames, <kbd>SPC t</kbd> shows hover docs. See [Language servers](/docs/lsp). The Org block does the same for Org-only actions:

```scheme
(evil-leader-set-for-modes
  '("org-mode")
  (list (cons "r r" 'org-babel-execute)
        (cons "u"   'org-todo)
        (cons "i"   'my/org-insert-src-r)))
```

## Installing packages

The last section pulls in two [packages](/docs/packages) with `package-install`, which clones a git repo on first launch (only the first launch touches the network) and loads it:

```scheme
(setenv "MACROS_AGENT" "claude")
(package-install "https://github.com/macros-app/agent-shell.git"
  'name "macros-agent-shell")          ; entry file: macros-agent-shell.scm
(evil-leader-set "m a" 'agent-shell)   ; SPC m a — start / focus agent-shell
```

`agent-shell` is an ACP chat client. `MACROS_AGENT` must be set *before* the package loads — hence `setenv` on the line above — so it's read as the package initializes. Installing a package also registers its commands, so `'agent-shell` binds to <kbd>SPC m a</kbd> just like any built-in.

The second package takes a `'config` thunk — a `(lambda () …)` that runs *after* the entry file loads, the right place for settings that call into the package:

```scheme
(package-install "https://github.com/macros-app/k8s-dashboard.git"
  'name "macros-k8s"                  ; entry file: macros-k8s.scm
  'config (lambda () (set-option "k8s-shell" "/bin/sh")))
```

`package-install` derives the package name — and from it the entry file, `<name>.scm` — from the URL, so a repo named `k8s-dashboard` would load `k8s-dashboard.scm`. This package's entry is `macros-k8s.scm`, so `'name "macros-k8s"` says so (agent-shell above does the same). Launch it with <kbd>M-x</kbd> `k8s-dashboard`. When you're developing a package locally instead of installing from git, swap `package-install` for `package-install-local` and point it at the working tree — a path (a leading `~/` is expanded), no clone. See [Packages](/docs/packages) for `'ref` pinning, `'depends`, and the local workflow.

## Where to go next

- **[Configuration](/docs/configuration)** — the reference: config paths, the full option list, splitting your config across files.
- **[Keybindings](/docs/keybindings)** — every default binding and the `bind-key` form.
- **[Modal editing](/docs/modal-editing)** — the evil layer this config builds on.
- **[Scripting with Steel](/docs/scripting)** — write commands like the `my/…` ones above.
- **[Packages](/docs/packages)** — install third-party Steel extensions with `package-install`.
