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
(set-face "ghost" "#a3a9b5")             ; inline suggestion (ghost text): lighter than comments, cool tint
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

;; Diagnostics (gutter dots + underlines) — VS Code light severity colors,
;; darkened for contrast on a white background.
(set-face "diagnostic-error" "#e51400")
(set-face "diagnostic-warning" "#bf8803")
(set-face "diagnostic-info" "#1a85ff")
(set-face "diagnostic-hint" "#1a85ff")

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

;; Org / tabulated / projectile — light-tuned. The modules' load-time defaults are
;; dark-theme colors, and `load-theme` wipes every face first, so these must live
;; here or they vanish (or turn unreadable) on a runtime theme switch.
(set-face "org-level-1" "#2e7d32")
(set-face "org-level-2" "#206595")
(set-face "org-level-3" "#8f3f71")
(set-face "org-level-4" "#9a6700")
(set-face "org-level-5" "#b35a00")
(set-face "org-level-6" "#5f7a1a")
(set-face "org-level-7" "#b3324a")
(set-face "org-level-8" "#2f6f8a")
(set-face "org-todo" "#c41030")
(set-face "org-done" "#2e7d32")
(set-face "org-tag" "#206595")
(set-face "org-priority" "#8f3f71")
(set-face "org-meta" "#6f6f6f")
(set-face "org-drawer" "#8a8a8a")
(set-face "org-block" "#5a4a2a")
(set-face "org-table" "#206595")
(set-face "org-link" "#1a5fa0")
(set-face "org-date" "#8f3f71")
(set-face "org-bold" "#141414")
(set-face "org-italic" "#333333")
(set-face "org-verbatim" "#4a7d3a")
(set-face "org-code" "#4a7d3a")
(set-face "org-checkbox" "#9a6700")
(set-face "org-checkbox-done" "#2e7d32")
(set-face "org-strike" "#777777")
(set-face "org-underline" "#206595")
(set-face "org-footnote" "#8f3f71")
(set-face "tabulated-header" "#206595")
(set-face "projectile-modeline" "#206595")

;; spell.scm — misspelled-word squiggle (wavy is an attribute, so it is
;; re-declared here: load-theme wipes attributes along with colors).
(set-face "spell-error" "#e51400")
(set-face-wavy "spell-error" #t)

;; dap.scm / ui-dap.scm — debugger faces (breakpoint gutter dots, the stopped
;; line, inline locals, the *dap* panel).
(set-face "dap-breakpoint"         "#e51400")  ; verified breakpoint dot
(set-face "dap-breakpoint-pending" "#b08d8d")  ; not (yet) bound by the adapter
(set-face "dap-stopped-line"       "#b3591f")  ; line execution stopped on
(set-face "dap-inline"             "#8f8f6a")  ; inline locals after the stopped line
(set-face "dap-heading"            "#1a63a8")  ; panel section headings
(set-face "dap-dim"                "#9a9a9a")  ; panel de-emphasis

;; occur.scm — *occur* / *grep* result buffers (header, dimmed file:line
;; prefixes, bold match highlight; bold re-declared like the wavy above).
(set-face "occur-header" "#206595")
(set-face "occur-prefix" "#8a8a8a")
(set-face "occur-match" "#9a6700")
(set-face-bold "occur-match" #t)

;; plot.scm — text charts. Series colors cycle 0…5; heat-0…7 is a viridis-like
;; ramp, each step both a foreground (half-block glyphs) and a `-bg`
;; background (heatmap cells) via set-face-background.
(set-face "plot-series-0" "#206595")
(set-face "plot-series-1" "#db704b")
(set-face "plot-series-2" "#1f8a70")
(set-face "plot-series-3" "#9e4fc0")
(set-face "plot-series-4" "#9a6700")
(set-face "plot-series-5" "#b31b3f")
;; Area under a curve (plot-function / plot-normal 'shade): a dimmer
;; plot-series-0, so the curve drawn over it stays legible.
(set-face "plot-shade" "#a9c8e4")
(set-face "plot-axis" "#b0b0b0")
(set-face "plot-label" "#6f6f6f")
(set-face "plot-title" "#141414")
(set-face-bold "plot-title" #t)
(set-face "plot-heat-0" "#440154")
(set-face "plot-heat-1" "#46327e")
(set-face "plot-heat-2" "#365c8d")
(set-face "plot-heat-3" "#277f8e")
(set-face "plot-heat-4" "#1fa187")
(set-face "plot-heat-5" "#4ac16d")
(set-face "plot-heat-6" "#a0da39")
(set-face "plot-heat-7" "#fde725")
(set-face-background "plot-heat-0-bg" "#440154")
(set-face-background "plot-heat-1-bg" "#46327e")
(set-face-background "plot-heat-2-bg" "#365c8d")
(set-face-background "plot-heat-3-bg" "#277f8e")
(set-face-background "plot-heat-4-bg" "#1fa187")
(set-face-background "plot-heat-5-bg" "#4ac16d")
(set-face-background "plot-heat-6-bg" "#a0da39")
(set-face-background "plot-heat-7-bg" "#fde725")
;; Chart glyphs shape with the bundled "Macros Plot" font: Lilex has no
;; braille, and system fallbacks are wider than its cell (see
;; scripts/gen-plot-font.py).
(set-face-font "plot-series-0" "Macros Plot")
(set-face-font "plot-series-1" "Macros Plot")
(set-face-font "plot-series-2" "Macros Plot")
(set-face-font "plot-series-3" "Macros Plot")
(set-face-font "plot-series-4" "Macros Plot")
(set-face-font "plot-series-5" "Macros Plot")
(set-face-font "plot-shade" "Macros Plot")
(set-face-font "plot-heat-0" "Macros Plot")
(set-face-font "plot-heat-1" "Macros Plot")
(set-face-font "plot-heat-2" "Macros Plot")
(set-face-font "plot-heat-3" "Macros Plot")
(set-face-font "plot-heat-4" "Macros Plot")
(set-face-font "plot-heat-5" "Macros Plot")
(set-face-font "plot-heat-6" "Macros Plot")
(set-face-font "plot-heat-7" "Macros Plot")

;; Embedded terminal (term / ansi-term) — the 16 ANSI system colors, darkened
;; for a light background (yellow/white especially would vanish otherwise).
;; 0–7 normal, 8–15 bright; unset ones fall back to a VS Code-style default.
(set-face "terminal-color-0"  "#141414")  ; black
(set-face "terminal-color-1"  "#c72e2e")  ; red
(set-face "terminal-color-2"  "#1f7a3a")  ; green
(set-face "terminal-color-3"  "#a86d00")  ; yellow
(set-face "terminal-color-4"  "#206595")  ; blue
(set-face "terminal-color-5"  "#8a3fa8")  ; magenta
(set-face "terminal-color-6"  "#0f7f8a")  ; cyan
(set-face "terminal-color-7"  "#5c5c5c")  ; white
(set-face "terminal-color-8"  "#999999")  ; bright black (fish autosuggestions)
(set-face "terminal-color-9"  "#e51400")  ; bright red
(set-face "terminal-color-10" "#2e9c4f")  ; bright green
(set-face "terminal-color-11" "#bf8803")  ; bright yellow
(set-face "terminal-color-12" "#1a85ff")  ; bright blue
(set-face "terminal-color-13" "#b052c9")  ; bright magenta
(set-face "terminal-color-14" "#13a3b0")  ; bright cyan
(set-face "terminal-color-15" "#2a2a2a")  ; bright white
