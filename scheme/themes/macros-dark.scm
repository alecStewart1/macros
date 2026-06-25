;; Dark theme — the "Cursor Dark" palette (github.com/CedricVerlinden/cursor-dark).
;; Editor/syntax token colors are taken from that VS Code theme; the chrome
;; (mode line, completion popup, selection) is mapped from its workbench colors.
;; A theme is just a set of faces: (set-face "<face>" "#rrggbb[aa]").

;; UI / chrome
(set-face "background" "#1a1a1a")        ; editor.background
(set-face "foreground" "#d8dee9")        ; editor.foreground
(set-face "cursor" "#ffffff")            ; editorCursor.foreground
(set-face "region" "#40404099")          ; editor.selectionBackground (translucent gray)
(set-face "mode-line-bg" "#141414")      ; statusBar.background
(set-face "mode-line-fg" "#cccccc")      ; statusBar.foreground (full opacity for legibility)
(set-face "minibuffer-prompt" "#87c3ff")
(set-face "dim" "#505050")               ; editorLineNumber.foreground
;; Window title bar — defaults to the window background; override for contrast.
(set-face "title-bar-bg" "#141414")      ; titleBar.activeBackground
(set-face "title-bar-fg" "#cccccc82")    ; titleBar.activeForeground

;; Completion popup (corfu-style)
(set-face "completion-bg" "#141414")             ; editorSuggestWidget.background
(set-face "completion-fg" "#d8dee9")
(set-face "completion-selected-bg" "#404040")    ; editorSuggestWidget.selectedBackground
(set-face "completion-selected-fg" "#ffffff")
(set-face "completion-border" "#2a2a2a")         ; editorSuggestWidget.border
(set-face "completion-kind" "#efb080")

;; Helm panel bevel (light top/left edge, dark bottom/right edge). Set both to
;; the panel background for a flat look.
(set-face "helm-bevel-light" "#2a2a2a")
(set-face "helm-bevel-shadow" "#000000")

;; Syntax (font-lock) — Cursor Dark token colors
(set-face "comment" "#6d6d6d")
(set-face "string" "#e394dc")
(set-face "keyword" "#83d6c5")
(set-face "function" "#e3c893")
(set-face "number" "#ebc88d")
(set-face "constant" "#ebc88d")
(set-face "type" "#efb080")
(set-face "operator" "#d6d6dd")
(set-face "punctuation" "#d6d6dd")
(set-face "variable" "#d6d6dd")

;; LSP feature faces (document highlight, inlay hints, code lens)
(set-face "lsp-highlight" "#ffffff21")   ; editor.wordHighlightBackground
(set-face "lsp-inlay-hint" "#505050")    ; editorInlayHint.foreground
(set-face "lsp-code-lens" "#505050")     ; editorCodeLens.foreground

;; Semantic-token faces (opt-in via 'semantic-tokens) — mirror the syntax palette,
;; with Cursor Dark's distinct blue namespaces and purple properties.
(set-face "lsp-sem-namespace" "#87c3ff")
(set-face "lsp-sem-type" "#efb080")
(set-face "lsp-sem-class" "#efb080")
(set-face "lsp-sem-struct" "#efb080")
(set-face "lsp-sem-interface" "#efb080")
(set-face "lsp-sem-enum" "#efb080")
(set-face "lsp-sem-typeParameter" "#efb080")
(set-face "lsp-sem-function" "#e3c893")
(set-face "lsp-sem-method" "#e3c893")
(set-face "lsp-sem-macro" "#e3c893")
(set-face "lsp-sem-keyword" "#83d6c5")
(set-face "lsp-sem-variable" "#d6d6dd")
(set-face "lsp-sem-parameter" "#d6d6dd")
(set-face "lsp-sem-property" "#aa9bf5")
(set-face "lsp-sem-string" "#e394dc")
(set-face "lsp-sem-number" "#ebc88d")
(set-face "lsp-sem-comment" "#6d6d6d")
(set-face "lsp-sem-operator" "#d6d6dd")
