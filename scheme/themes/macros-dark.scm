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
(set-face "ghost" "#5f6672")             ; inline suggestion (ghost text): dimmer than comments, cool tint
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
(set-face "bracket" "#7d818a")           ; dimmed structural delimiters

;; LSP feature faces (document highlight, inlay hints, code lens)
(set-face "lsp-highlight" "#ffffff21")   ; editor.wordHighlightBackground
(set-face "lsp-inlay-hint" "#505050")    ; editorInlayHint.foreground
(set-face "lsp-code-lens" "#505050")     ; editorCodeLens.foreground

;; Diagnostics (gutter dots + underlines) — VS Code dark severity colors.
(set-face "diagnostic-error" "#f14c4c")
(set-face "diagnostic-warning" "#cca700")
(set-face "diagnostic-info" "#3794ff")
(set-face "diagnostic-hint" "#3794ff")

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

;; Magit (status buffer). magit.scm sets these same defaults at load time, so a
;; theme that omits them still works; defining them here keeps the magit buffer
;; colorized after a runtime `load-theme` (which resets the palette first).
(set-face "magit-section-heading" "#a6e22e")
(set-face "magit-file" "#e6db74")
(set-face "magit-key" "#66d9ef")
(set-face "magit-added" "#7a9a3a")        ; +line body
(set-face "magit-removed" "#b34a5f")      ; -line body
(set-face "magit-hunk" "#66d9ef")         ; @@ hunk headers
(set-face "magit-context" "#888888")      ; unchanged context
(set-face "magit-group" "#7fb0a8")        ; funcname sub-heading
(set-face "magit-add-marker" "#a6e22e")   ; leading + marker
(set-face "magit-remove-marker" "#f92672"); leading - marker
(set-face "magit-added-dim" "#4e5e34")    ; unchanged text on a modified +line
(set-face "magit-removed-dim" "#6e4651")  ; unchanged text on a modified -line
(set-face "magit-word-added" "#cdee69")   ; changed span on a +line
(set-face "magit-word-removed" "#fc6a8a") ; changed span on a -line
(set-face "magit-label-modified" "#e2c08d")
(set-face "magit-label-new"      "#a6e22e")
(set-face "magit-label-deleted"  "#f92672")
(set-face "magit-label-renamed"  "#66d9ef")
(set-face "magit-label-other"    "#e6db74")
(set-face "magit-hash" "#b58900")         ; commit short hashes
(set-face "magit-header" "#9a9a9a")       ; "Head:" label column
(set-face "magit-branch" "#66d9ef")       ; branch / upstream names
(set-face "hl-line" "#ffffff14")          ; current-line band (translucent white)
(set-face "magit-gutter-added" "#6cae3e")    ; git-gutter added bar
(set-face "magit-gutter-modified" "#c7a23a") ; git-gutter modified bar
(set-face "magit-gutter-removed" "#c1574b")  ; git-gutter deletion mark

;; spell.scm — misspelled-word squiggle (wavy is an attribute, so it is
;; re-declared here: load-theme wipes attributes along with colors).
(set-face "spell-error" "#f14c4c")
(set-face-wavy "spell-error" #t)

;; occur.scm — *occur* / *grep* result buffers (header, dimmed file:line
;; prefixes, bold match highlight; bold re-declared like the wavy above).
(set-face "occur-header" "#87c3ff")
(set-face "occur-prefix" "#6d6d6d")
(set-face "occur-match" "#ebc88d")
(set-face-bold "occur-match" #t)
