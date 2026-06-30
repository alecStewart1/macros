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
(set-face "bracket" "#8a8a8a")           ; dimmed structural delimiters

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

;; Magit (status buffer) — Cursor Light palette. magit.scm's built-in defaults are
;; dark-theme colors (bright greens/yellows that vanish on a light background); a
;; runtime `load-theme` resets the palette, so these light-tuned overrides are what
;; keep the magit buffer readable in light mode. Tier logic is inverted vs. dark:
;; to RECEDE on white a color goes lighter/lower-contrast, to POP it goes darker.
(set-face "magit-section-heading" "#2e7d32") ; dark green heading
(set-face "magit-file" "#9a6700")            ; amber file names
(set-face "magit-key" "#206595")             ; blue keys (Cursor Light type blue)
(set-face "magit-added" "#4a7d3a")           ; +line body (medium green)
(set-face "magit-removed" "#b3324a")         ; -line body (medium red)
(set-face "magit-hunk" "#206595")            ; @@ hunk headers
(set-face "magit-context" "#777777")         ; unchanged context
(set-face "magit-group" "#2f8a7d")           ; funcname sub-heading (teal)
(set-face "magit-add-marker" "#1f8a2f")      ; leading + marker (pops darker)
(set-face "magit-remove-marker" "#c41030")   ; leading - marker (pops darker)
(set-face "magit-added-dim" "#8fb87f")       ; unchanged text on a +line (recedes lighter)
(set-face "magit-removed-dim" "#d99ba8")     ; unchanged text on a -line (recedes lighter)
(set-face "magit-word-added" "#1f7a1f")      ; changed span on a +line (pops darkest)
(set-face "magit-word-removed" "#c41030")    ; changed span on a -line (pops darkest)
(set-face "magit-label-modified" "#9a6700")
(set-face "magit-label-new"      "#2e7d32")
(set-face "magit-label-deleted"  "#c41030")
(set-face "magit-label-renamed"  "#206595")
(set-face "magit-label-other"    "#9a6700")
(set-face "magit-hash" "#8a6d00")            ; commit short hashes (dark goldenrod)
(set-face "magit-header" "#888888")          ; "Head:" label column
(set-face "magit-branch" "#206595")          ; branch / upstream names
(set-face "hl-line" "#00000010")             ; current-line band (translucent black)
(set-face "magit-gutter-added" "#3f8b2f")    ; git-gutter added bar
(set-face "magit-gutter-modified" "#b07d1a") ; git-gutter modified bar
(set-face "magit-gutter-removed" "#bf3340")  ; git-gutter deletion mark
