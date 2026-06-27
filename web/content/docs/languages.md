---
title: Languages & modes
description: Add syntax highlighting for a file type, bundle a new tree-sitter grammar, and build a major mode.
---

# Languages & modes

Support for a language in Macros has two independent layers:

1. **Syntax highlighting** — driven by [tree-sitter](https://tree-sitter.github.io/). A grammar parses the buffer and a *highlights query* tags nodes with capture names, which map to theme faces.
2. **A major mode** — a keymap plus commands and behavior (folding, navigation, editing helpers), activated when a matching file opens.

You can do either without the other: a file type can get highlighting with no custom mode, and a mode can add behavior on top of the built-in highlighting.

One thing to know up front: **tree-sitter grammars are compiled into the binary.** Adding highlighting for a file type whose grammar is *already bundled* is pure `init.scm` config. Adding a *brand-new* grammar is a small Rust change and a rebuild (covered below).

## How highlighting works

A grammar's highlights query tags nodes with standard capture names. The highlighter maps each capture to a theme face — by the capture's dotted prefix, so `constant.builtin` and `string.special` resolve to `constant` and `string`. The recognized faces are:

`comment` · `string` · `keyword` · `function` · `number` · `constant` · `type` · `operator` · `punctuation` · `variable`

Because the mapping is by capture name, any standard tree-sitter `highlights.scm` query colors correctly with no per-language wiring. To restyle, set those faces in your theme — see [Themes & faces](/docs/themes-and-faces).

## Highlighting a file type (bundled grammar)

These grammars ship in the binary:

`scheme` · `rust` · `python` · `javascript` · `typescript` · `tsx` · `json` · `c` · `cpp` · `go` · `java` · `scala` · `clojure` · `ruby` · `lua` · `zig` · `r` · `ocaml` · `haskell` · `bash` · `html` · `css` · `yaml` · `toml` · `markdown` · `latex` · `typst` · `mermaid`

Map a file extension to one of them with `set-extension`, and highlighting (and the LSP language id) follows automatically:

```scheme
;; Highlight .conf files using the bundled TOML grammar.
(set-extension "conf" "toml")
```

When the file type also has a language server, configure both at once with the `lsp-language` form (see [Language servers](/docs/lsp)) — its `'extensions` already calls `set-extension` for you:

```scheme
(lsp-language "kotlin"
  'extensions     '("kt" "kts")
  'command        "kotlin-language-server"
  'format-on-save 'lsp)
```

> `lsp-language` wires the *server* and the language id, but highlighting still needs a bundled grammar for that id. If none is bundled (Kotlin isn't), add it as below.

## Adding a new tree-sitter grammar

Because grammars are compiled in, a brand-new one is a three-step Rust change, then a rebuild. Say you want Kotlin:

1. **Add the grammar crate** to `Cargo.toml`:

   ```toml
   tree-sitter-kotlin = "0.3"
   ```

2. **Register it** in `grammar()` in `src/highlight.rs` — one arm mapping the language id to its `LANGUAGE` and highlights query:

   ```rust
   "kotlin" => (tree_sitter_kotlin::LANGUAGE, tree_sitter_kotlin::HIGHLIGHTS_QUERY),
   ```

3. **Map the extension** in `language_for_path` in `src/editor/mod.rs`:

   ```rust
   "kt" | "kts" => "kotlin",
   ```

Then add `"kotlin"` to the `BUNDLED_LANGUAGES` list in `src/highlight.rs`. That list is the single source of truth: the grammar-load tests iterate it (so a query the grammar version rejects fails loudly instead of silently disabling highlighting), and Scheme reads it via `(bundled-languages)` to generate the mode command below.

Rebuild and Kotlin files highlight. This is exactly how Zig, R, OCaml, Haskell, Scala, and Clojure were added — all now bundled, so their files highlight with no config. (OCaml's grammar ships split implementation/interface parsers like TypeScript, so its entry uses `LANGUAGE_OCAML`.)

The capture names in the grammar's own `highlights.scm` flow straight into the faces above, so you usually need no query of your own.

## Building a major mode

A major mode is a keymap named after the mode, plus commands, activated on `find-file-hook`. The minimal recipe:

```scheme
;; env-mode: a tiny major mode for `.env` files (highlighted as shell).

;; 1. Highlight the extension with a bundled grammar.
(set-extension "env" "bash")

;; 2. Commands.
(define (env-uncomment-line)
  ;; … edit the current line …
  (message "uncommented"))

;; 3. A keymap named after the mode; bind the commands.
(define-key "env-mode" "ctrl-c ctrl-c" "env-uncomment-line")

;; 4. Make commands discoverable from M-x.
(register-command "env-uncomment-line")

;; 5. Activate the mode when a matching file opens.
(define (env--maybe-activate)
  (when (ends-with? (current-buffer) ".env")
    (set-major-mode "env-mode")))
(add-hook "find-file-hook" "env--maybe-activate")
```

`set-major-mode` installs the keymap whose name you pass, so the mode name and the keymap name must match. It is also highlighting-aware: if the mode name matches a bundled grammar, switching to it attaches that grammar (and, via the `fundamental` `enable-lsp` hook, its language server *if one is configured* — see [Language servers](/docs/lsp)) and runs the mode's hooks. Bindings can use [`bind-key`](/docs/keybindings#binding-a-function-directly), which takes the command as a `'name` symbol (or a bare function value) rather than a command-name string.

The bundled modes are the best templates: `scheme/markdown.scm` is a compact tree-sitter-backed mode (the grammar highlights; the mode adds heading navigation and folding), and `scheme/org.scm` is a full mode with its own parser and fontification.

## Language modes and entry hooks

Every bundled grammar also gets a major mode named after it, entered automatically when a matching file opens — a `.rs` file lands in `rust` mode, a `.scala` file in `scala` mode. Each is an M-x command too (`rust-mode`, `scala-mode`, `clojure-mode`, …), so you can switch any buffer's mode by hand with no per-language wiring.

To configure what happens **on entering a mode** — the analog of Emacs's `rust-mode-hook` — register a function under the mode name. The hook runs on both entry paths (opening a file of that type, or `M-x rust-mode`):

```scheme
(define (my-rust-setup)
  (set-option "word-wrap:rust" #f)                  ; don't wrap long lines in Rust
  (define-key "rust" "ctrl-c ctrl-t" "lsp-hover"))

(add-hook "rust" 'my-rust-setup)
```

`add-hook` accepts a command-name string, a `'symbol`, or an anonymous `(lambda () …)`. Two differences from Emacs worth knowing: the hook key is the **mode name** (`"rust"`, not `rust-mode-hook`), with `fundamental` as the catch-all that runs for every buffer; and the registry is **global** (shared by all buffers in that mode) rather than buffer-local, so hooks should be idempotent — *set* buffer-local state, don't *append* to it.

## Custom fontification from Scheme

When the standard tree-sitter highlighting isn't enough — you want depth-based coloring, semantic rules, or a language with no grammar — paint faces yourself.

**Query the parse tree.** `(tree-sitter-query "<query>" "callback")` runs a tree-sitter query over the active buffer's tree and calls your Scheme function with the captures as newline-separated, tab-delimited records (`name⇥start⇥end⇥kind`). Paint each with `overlay-set-face`:

```scheme
(define (color-types records)
  (for-each
    (lambda (rec)
      (let ((p (split-many rec "\t")))   ; (name start end kind)
        (overlay-set-face (string->number (nth p 1))
                          (string->number (nth p 2))
                          "type")))
    (non-empty-lines records)))

(tree-sitter-query "(type_identifier) @t" "color-types")
```

`scheme/rainbow.scm` is the full worked example (depth-colored brackets via a query plus `overlay-set-face`), and re-runs on `find-file-hook` and `after-change-functions` so it stays live as you type.

**Ask what you're inside.** `(tree-sitter-node-at byte "callback")` reports the chain of nodes covering a byte (innermost first) — useful to, say, skip an action when point sits inside a string or comment node.

**No grammar?** Fontify with regexes and `put-text-property` / `overlay-set-face`, the way `scheme/org.scm` colors headings and TODO keywords without any tree-sitter grammar.
