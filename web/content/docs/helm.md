---
title: Helm
description: The generic selection / narrowing framework behind M-x, buffer switching, and more.
---

# Helm

Helm is Macros' **generic selection framework** — the narrowing UI you see when you run `M-x`, switch buffers, or find a project file. The behavior (the panel, matching, marking) is in Rust; the *sources*, *actions*, and *applications* are written in Scheme, so you can build your own pickers on top of it.

## Where you meet Helm

- **`M-x`** — `helm-execute-command`, a source over every command.
- **Buffer switching** — `helm-buffers` (also aliased to the Emacs names), with a marginalia-style "special" hint on buffers like `*scratch*`.
- **Project find-file** and **switch-project** — see [Projects & search](/docs/projects).
- **`helm-apropos`** (<kbd>C-h C-a</kbd>) — live search over every loaded definition's doc-comment, with jump-to-source; see [Getting help](/docs/getting-help).

## Session keys

These are active only during a Helm session:

| Key | Action |
| --- | --- |
| <kbd>↑</kbd> / <kbd>↓</kbd> | move the selection |
| <kbd>Enter</kbd> | run the default action on the selection |
| <kbd>Tab</kbd> | complete / descend |
| <kbd>C-Space</kbd> | mark the candidate for multi-select |
| <kbd>M-a</kbd> | open the action menu (for sources with more than one action) |
| <kbd>C-g</kbd> | cancel |

With one or more candidates marked via <kbd>C-Space</kbd>, <kbd>Enter</kbd> runs the action on **every** marked candidate.

## Matching & appearance

```scheme
(set-option "helm-fuzzy"  #t)    ; fuzzy (nucleo, fzf-style) vs substring
(set-option "helm-height" 0.5)   ; fraction of the window height
;; Bevel edges (px per side):
(set-option "helm-bevel-top"    1)
(set-option "helm-bevel-right"  0)
(set-option "helm-bevel-bottom" 0)
(set-option "helm-bevel-left"   0)
```

## Annotations (marginalia)

A candidate may be a bare string or a `(candidate annotation)` pair. The annotation renders dimmed and right-aligned in the panel; only the candidate string is matched on and passed to the action. Dynamic sources can return the same pair form. For example:

```scheme
(list (list "ada" "engineer")
      "alan"
      (list "grace" "admiral"))
```

## Building your own picker

The buffer switcher is a complete example: a Helm *application* written in Scheme over two primitives — `(buffer-names)` lists the open buffers and `switch-to-buffer` is the default action; a second action (reached with <kbd>M-a</kbd>) kills the selected buffer. You can build pickers over any list the same way. See [Scripting with Steel](/docs/scripting).
