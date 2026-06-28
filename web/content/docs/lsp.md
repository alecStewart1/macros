---
title: Language servers (LSP)
description: Navigation, documentation, refactoring, and formatting via the Language Server Protocol.
---

# Language servers (LSP)

Macros speaks the **Language Server Protocol**. The protocol layer — sending requests, routing responses, jumping to locations, applying edits — is in Rust; the user-facing commands, the result-buffer keymaps, and the default keys are wired in Scheme, so you can rebind and extend everything.

## Setting up a language server

Language servers are **opt-in**: none are configured out of the box (the one exception is the built-in, in-process Scheme server, which needs no binary). You enable a language by declaring it with the `lsp-language` form in your `init.scm`; the server then starts lazily the first time you open a matching file — provided the server binary is on your `PATH`. (Syntax highlighting is separate — it comes from the bundled tree-sitter grammars and is always on, with or without a server.)

The smallest useful example — Rust, via `rust-analyzer`, formatting on save:

```scheme
(lsp-language "rust"
  'extensions     '("rs")
  'command        "rust-analyzer"
  'format-on-save 'lsp)          ; format through the server on save
```

### Enabling the common servers

Paste any of these into your `init.scm` and keep the ones whose server binaries are installed and on your `PATH`. This is the set Macros used to configure automatically; `format-on-save 'lsp` uses each server's own formatter (rustfmt, gofmt, clang-format, …) — no extra binary, and a harmless no-op if the server can't format.

```scheme
(lsp-language "rust"       'extensions '("rs")              'command "rust-analyzer"                 'format-on-save 'lsp)
(lsp-language "go"         'extensions '("go")              'command "gopls"                         'format-on-save 'lsp)
(lsp-language "c"          'extensions '("c" "h")           'command "clangd"                        'format-on-save 'lsp)
(lsp-language "cpp"        'extensions '("cpp" "cc" "hpp")  'command "clangd"                        'format-on-save 'lsp)
(lsp-language "python"     'extensions '("py")              'command "pylsp"                         'format-on-save 'lsp)

;; .ts and .tsx share one server but must announce different languageIds, or a
;; .tsx file opens as plain "typescript" and every JSX element is a syntax error.
(lsp-language "typescript"
  'extensions   '("ts" "tsx")
  'language-ids '(("ts" . "typescript") ("tsx" . "typescriptreact"))
  'command      "typescript-language-server --stdio"
  'format-on-save 'lsp)
(lsp-language "javascript"
  'extensions   '("js" "jsx")
  'language-ids '(("js" . "javascript") ("jsx" . "javascriptreact"))
  'command      "typescript-language-server --stdio"
  'format-on-save 'lsp)

;; JVM languages (highlighting is bundled; install the server to enable LSP):
(lsp-language "scala"      'extensions '("scala" "sc" "sbt")        'command "metals"      'format-on-save 'lsp)
(lsp-language "clojure"    'extensions '("clj" "cljs" "cljc" "edn") 'command "clojure-lsp" 'format-on-save 'lsp)
```

The same block lives (commented) in the `DEFAULT LANGUAGE CONFIGURATION` section at the end of `scheme/lsp.scm`.

`'command` is the server invocation; put any flags right in the string:

```scheme
(lsp-language "typescript"
  'extensions   '("ts" "tsx")
  'language-ids '(("tsx" . "typescriptreact"))   ; per-extension wire id
  'command      "typescript-language-server --stdio")
```

### Options

| Option | What it does |
| --- | --- |
| `'extensions` | File extensions that activate this language (a list) |
| `'command` | The server command line — flags allowed inline |
| `'format-on-save` | `'lsp` (format via the server), a shell formatter like `(list "black" "-")`, or the name of your own Scheme function |
| `'inlay-hints` | On by default; pass `#f` to turn the type/parameter hints off |
| `'semantic-tokens` | `#t` to color from the server's semantic tokens — turn [rainbow delimiters](/docs/rainbow-delimiters) off for that language so they don't fight over the overlay layer |
| `'code-lens` | `#t` to show code lenses |
| `'language-ids` | Per-extension `languageId` overrides, an alist of `(ext . id)` |

### More examples

```scheme
;; Python, formatted by black instead of the server ("-" makes black read stdin):
(lsp-language "python"
  'extensions     '("py")
  'command        "pylsp"
  'format-on-save (list "black" "-"))

;; Go with inlay hints and semantic-token coloring on:
(lsp-language "go"
  'extensions      '("go")
  'command         "gopls"
  'format-on-save  'lsp
  'inlay-hints     #t
  'semantic-tokens #t)

;; Zig via zls, with inlay hints off:
(lsp-language "zig"
  'extensions     '("zig")
  'command        "zls"
  'format-on-save 'lsp
  'inlay-hints    #f)
```

Declaring a language is idempotent — re-declaring it (e.g. to flip `'inlay-hints`) just replaces the previous definition, so you can refine a config by stating it again later in your `init.scm`.

## The built-in Scheme server

The in-process Scheme server needs no binary and runs automatically for `.scm` files. Editor config and extension code (anything under `~/.config/macros`, or a `*.macros.scm` file) get the `macros-scheme` variant, which additionally seeds the whole editor API. It provides **completion**, **"unknown function" diagnostics**, **go-to-definition** (<kbd>M-.</kbd>), **find-references** (<kbd>M-?</kbd>), **hover** (<kbd>C-c C-l h</kbd>), and **rename** (<kbd>C-c C-l n</kbd>).

Because command/keybinding references are `'symbols` rather than strings, the server sees them as real identifiers: <kbd>M-.</kbd> on `'magit-status` in a keybinding jumps to its `(define …)` (in your config or the bundled source), <kbd>M-?</kbd> lists its uses — quoted references included — and hover shows its docs. The leading quote is a word boundary, so the bare name is what resolves.

Rename is **scoped to the current document and to symbols it defines** — renaming a command you wrote in `init.scm` updates its definition, keybindings, and calls there (quoted references included). A symbol defined elsewhere (a bundled command, another file) is declined ("Cannot rename this symbol"), since the built-in server can't safely update across files. (In config buffers, <kbd>C-c C-d</kbd> / <kbd>C-c C-f</kbd> also give doc-at-point via the Scheme-side `macros-eldoc` / `macros-describe-point` — see [Scripting](/docs/scripting).)

## Navigation & refactoring commands

All are <kbd>M-x</kbd>-discoverable. The most common two have short bindings; the rest live under the <kbd>C-c C-l</kbd> prefix.

| Key | Command | Action |
| --- | --- | --- |
| <kbd>M-.</kbd> | `lsp-find-definition` | Go to definition |
| <kbd>M-?</kbd> | `lsp-find-references` | List all references |
| <kbd>M-*</kbd> | `xref-pop-marker` | Jump back |
| <kbd>C-c C-d</kbd> | `lsp-hover` | Documentation at point |
| <kbd>C-c C-l d</kbd> | `lsp-find-definition` | Definition |
| <kbd>C-c C-l D</kbd> | `lsp-find-declaration` | Declaration |
| <kbd>C-c C-l t</kbd> | `lsp-find-type-definition` | Type definition |
| <kbd>C-c C-l i</kbd> | `lsp-find-implementation` | Implementation(s) |
| <kbd>C-c C-l r</kbd> | `lsp-find-references` | References |
| <kbd>C-c C-l n</kbd> | `lsp-rename` | Rename everywhere |
| <kbd>C-c C-l h</kbd> | `lsp-hover` | Hover docs |
| <kbd>C-c C-l k</kbd> | `lsp-signature-help` | Signature of the call at point |
| <kbd>C-c C-l s</kbd> | `lsp-document-symbols` | Symbols in this buffer |
| <kbd>C-c C-l S</kbd> | `lsp-workspace-symbol` | Query symbols across the project |
| <kbd>C-c C-l f</kbd> | `lsp-format-buffer` | Format the buffer |
| <kbd>C-c C-l a</kbd> | `lsp-code-action` | Code actions / quick fixes |

### More under C-c C-l

| Key | Command |
| --- | --- |
| <kbd>C-c C-l H</kbd> | `lsp-document-highlight` |
| <kbd>C-c C-l I</kbd> | `lsp-inlay-hints` |
| <kbd>C-c C-l L</kbd> | `lsp-code-lens` |
| <kbd>C-c C-l e</kbd> / <kbd>C-c C-l E</kbd> | `lsp-expand-selection` / `lsp-contract-selection` |
| <kbd>C-c C-l z</kbd> | `lsp-fold-all` |
| <kbd>C-c C-l c</kbd> / <kbd>C-c C-l C</kbd> | call hierarchy incoming / outgoing |
| <kbd>C-c C-l T</kbd> | `lsp-semantic-tokens-apply` |
| <kbd>C-c C-l o</kbd> | `lsp-open-link` |

## Result buffers

References and symbol queries open read-only list buffers (`*lsp-xref*`, `*lsp-symbols*`). They behave like every list buffer in Macros:

| Key | Action |
| --- | --- |
| <kbd>Enter</kbd> | visit the target for the current row |
| <kbd>n</kbd> / <kbd>p</kbd> | move down / up |
| <kbd>q</kbd> | close |

Hover docs open in an `*lsp-doc*` buffer (<kbd>q</kbd> to dismiss). Code actions open an actions list where <kbd>Enter</kbd> applies the selected action.

## Rename

`lsp-rename` validates the position with `prepareRename`, prompts you (pre-filled with the current name), and applies the resulting workspace edit across every file.

## Logging

The language server's stderr is captured. View it with <kbd>C-h l</kbd> (`list-lsp-log`). To log server startup, set:

```scheme
(set-option "lsp-log-on-start" #t)
```

## Diagnostics & semantic tokens

LSP diagnostics drive the [diagnostics](/docs/diagnostics) list and gutter dots. Semantic-token highlighting shares the overlay-face layer with [rainbow delimiters](/docs/rainbow-delimiters) — enable one per buffer at a time so they don't fight over that layer.
