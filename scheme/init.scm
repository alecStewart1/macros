;; Startup. Loaded after the keymaps; a good place for user customization
;; (set-face, load-theme, define-key, …).
;;
;; This is the BUILT-IN default config. Your personal config is loaded after it
;; (and can override anything here) from the first of:
;;   $XDG_CONFIG_HOME/macros/init.scm   (if XDG_CONFIG_HOME is set)
;;   ~/.config/macros/init.scm          (macOS + Linux)
;;   %APPDATA%/macros/init.scm          (Windows)
;; If none exists, the editor just uses these defaults. Your init.scm can split
;; itself across files with `(include "other.scm")`, resolved relative to the
;; config directory; `(load "/abs/path.scm")` works for absolute paths.

;; Select the default theme (see scheme/themes/*.scm).
(load-theme "macros-dark")

;; Editor options (defaults live in Rust; override them here). Values are typed:
;; pass a number, a boolean (#t / #f), or a string — a "string" works too and is
;; coerced to the option's declared type.
(set-option "font-family" "Lilex")           ; single family name; must be installed
(set-option "font-size" 13)
(set-option "line-height" 20)
(set-option "tab-width" 4)
;; Completion popup (corfu-style). Behaviour is in Rust; policy lives here.
(set-option "completion-style" "popup")     ; "popup" or "minibuffer"
(set-option "completion-auto" #t)            ; pop up automatically while typing
(set-option "completion-auto-prefix" 2)      ; min chars before auto-popup
(set-option "completion-max-rows" 10)        ; visible rows in the popup
(set-option "completion-show-detail" #t)     ; show signature/type next to name
(set-option "completion-show-kind" #t)       ; show kind tag (fn, var, kw, …)

;; Cursor shape: "auto" (block in evil normal/visual, bar otherwise), "bar", "block".
(set-option "cursor-style" "auto")
(set-option "cursor-width" 2)                ; bar width in px
;; Line-number gutter (Emacs display-line-numbers-mode). Per major mode; on by
;; default for every mode (the dired-sidebar tree opts out — see dired-sidebar.scm).
;; Toggle the current mode live with M-x line-numbers-mode, or override one mode:
;;   (set-option "line-numbers:dired-sidebar-mode" #f)
(set-option "line-numbers" #t)
;; Scroll bar on each pane's right edge (Emacs scroll-bar-mode). On by default.
;; Toggle live with M-x scroll-bar-mode.
(set-option "scroll-bar" #t)
;; Width (px) of the line between split windows. Also the drag-to-resize hit
;; area — raise it (e.g. 3) if the divider is hard to grab with the mouse.
(set-option "divider-width" 1)

;; Fast search (ripgrep as a library) — used by projectile find-file & grep.
;; .gitignore/.ignore are respected automatically; add extra rules here.
;; (search-exclude "target")        ; skip a directory or glob
;; (search-exclude "*.min.js")
;; (search-include "*.rs")          ; whitelist: only walk matching files
;; (set-search-option "hidden" #t)               ; include dotfiles
;; (set-search-option "respect-gitignore" #f)
;; (set-search-option "max-depth" 8)

;; Helm selection panel.
(set-option "helm-fuzzy" #t)                 ; fuzzy (nucleo, fzf-style) vs substring
(set-option "helm-height" 0.5)               ; fraction of the window height
(set-option "helm-bevel-top" 1)              ; bevel edge widths in px (per side)
(set-option "helm-bevel-right" 0)
(set-option "helm-bevel-bottom" 0)
(set-option "helm-bevel-left" 0)

;; Language servers are OPT-IN: none are configured out of the box except the
;; built-in (in-process) Scheme server. Declare the languages you want in
;; ~/.config/macros/init.scm with the ergonomic `(lsp-language …)` form; each
;; server then starts lazily when a matching file is opened, provided its binary
;; is on PATH. A ready-to-copy template for the common servers (the set Macros
;; used to configure automatically) is in the DEFAULT LANGUAGE CONFIGURATION
;; block at the end of lsp.scm; the examples below show the form. (Syntax
;; highlighting is separate — it comes from the bundled grammars and is always on.)

;; Language-server stderr is always captured into *lsp-log* — open it with
;; C-h l (M-x list-lsp-log); it streams live once open. Set this to "true" if you
;; want it to auto-pop in a split when a server starts (once per language).
(set-option "lsp-log-on-start" #f)

;; Tip: the lines above use the low-level primitives. In YOUR config
;; (~/.config/macros/init.scm, loaded after the built-ins) you can set a whole
;; language up at once with the ergonomic `lsp-language` form, including
;; format-on-save and feature toggles:
;;
;;   (lsp-language "rust"
;;     'extensions     '("rs")
;;     'command        "rust-analyzer"
;;     'format-on-save 'lsp)              ; LSP formatting on save
;;
;;   (lsp-language "go"
;;     'extensions     '("go")
;;     'command        "gopls"
;;     'format-on-save (list "gofmt")     ; or shell out to any formatter
;;     'inlay-hints    #t
;;     'semantic-tokens #t)
;;
;;   (lsp-language "python"
;;     'extensions     '("py")
;;     'command        "pylsp"
;;     'format-on-save (list "black" "-"))  ; black reads stdin with "-"
;;
;; `format-on-save` also accepts the name of your own Scheme function (a string)
;; if you want custom behaviour. before-save formatting runs synchronously; the
;; 'lsp variant formats via the server as part of the save.

;; Emacs keybindings by default. For Vim-style modal editing (vanilla evil:
;; plain Vim keys, no leader), add (evil-mode) to your ~/.config/macros/init.scm.

;; Third-party extensions: `package-install` clones a git repo into
;; <config-dir>/packages on first launch, optionally pins it to a commit/tag, and
;; loads it. Put these in YOUR ~/.config/macros/init.scm. M-x package-list /
;; package-update manage them. See the Packages doc for the full option list.
;;   (package-install "https://github.com/user/macros-foo"
;;     'ref    "v1.0"                       ; pin a commit/tag (recommended)
;;     'config (lambda () (set-option "foo-enabled" "true")))

(message "Macros ready — Steel runtime loaded")
