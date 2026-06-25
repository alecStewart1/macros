;; Light theme — the "Cursor Light" palette (github.com/vpoltora/cursor-light.nvim).
;; Syntax token colors are taken from that Neovim theme's palette (red keywords,
;; orange functions, purple strings, blue types, magenta numbers); the chrome is
;; mapped from its UI colors. A theme is just a set of faces.

;; UI / chrome
(set-face "background" "#fcfcfc")        ; editor.background
(set-face "foreground" "#141414")        ; editor.foreground
(set-face "cursor" "#141414")
(set-face "region" "#e8e8e8")            ; selection (alpha-blended in Cursor)
(set-face "mode-line-bg" "#f3f3f3")      ; statusBar.background
(set-face "mode-line-fg" "#555555")      ; darker foreground
(set-face "minibuffer-prompt" "#3c7cab") ; link / accent blue
(set-face "dim" "#999999")               ; line numbers
;; Window title bar — defaults to the window background; override for contrast.
(set-face "title-bar-bg" "#fcfcfc")
(set-face "title-bar-fg" "#999999")

;; Completion popup (corfu-style)
(set-face "completion-bg" "#f3f3f3")             ; floating windows background
(set-face "completion-fg" "#141414")
(set-face "completion-selected-bg" "#dddddd")    ; border-focus gray
(set-face "completion-selected-fg" "#141414")
(set-face "completion-border" "#dddddd")
(set-face "completion-kind" "#206595")

;; Helm panel bevel (light top/left edge, dark bottom/right edge). Set both to
;; the panel background for a flat look.
(set-face "helm-bevel-light" "#ffffff")
(set-face "helm-bevel-shadow" "#cccccc")

;; Syntax (font-lock) — Cursor Light token colors
(set-face "comment" "#6f6f6f")
(set-face "string" "#9e94d5")
(set-face "keyword" "#b31b3f")
(set-face "function" "#db704b")
(set-face "number" "#b8448b")
(set-face "constant" "#206595")
(set-face "type" "#206595")
(set-face "operator" "#141414")
(set-face "punctuation" "#141414")
(set-face "variable" "#141414")

;; LSP feature faces (document highlight, inlay hints, code lens)
(set-face "lsp-highlight" "#d4e3e8")     ; search-match teal
(set-face "lsp-inlay-hint" "#999999")
(set-face "lsp-code-lens" "#999999")

;; Semantic-token faces (opt-in via 'semantic-tokens) — mirror the syntax palette,
;; with Cursor Light's blue types/namespaces, purple-blue properties, green macros.
(set-face "lsp-sem-namespace" "#206595")
(set-face "lsp-sem-type" "#206595")
(set-face "lsp-sem-class" "#206595")
(set-face "lsp-sem-struct" "#206595")
(set-face "lsp-sem-interface" "#206595")
(set-face "lsp-sem-enum" "#206595")
(set-face "lsp-sem-typeParameter" "#206595")
(set-face "lsp-sem-function" "#db704b")
(set-face "lsp-sem-method" "#db704b")
(set-face "lsp-sem-macro" "#1f8a65")
(set-face "lsp-sem-keyword" "#b31b3f")
(set-face "lsp-sem-variable" "#141414")
(set-face "lsp-sem-parameter" "#141414")
(set-face "lsp-sem-property" "#6049b3")
(set-face "lsp-sem-string" "#9e94d5")
(set-face "lsp-sem-number" "#b8448b")
(set-face "lsp-sem-comment" "#6f6f6f")
(set-face "lsp-sem-operator" "#141414")
