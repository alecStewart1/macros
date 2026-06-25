---
title: Snippets
description: Template expansion with tab-stops — type a key, expand it, and tab between the fields.
---

# Snippets

Expand short keys into templates with fillable fields (Emacs `YASnippet`). Type a snippet key and expand it: the key is replaced with the template, point lands on the first field (selected, so typing overwrites it), and you tab between fields.

## Expanding & navigating fields

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-c y</kbd> | `yas-expand` | Expand the snippet key before point |
| <kbd>C-c C-f</kbd> | `snippet-next-field` | Jump to the next field |
| <kbd>C-c C-b</kbd> | `snippet-prev-field` | Jump to the previous field |

A handful of Rust-style snippets ship by default: `fn`, `for`, `if`, `while`, and `let`.

## Defining your own

Add or replace snippets from your `init.scm` with `yas-define-snippet`:

```scheme
(yas-define-snippet "doc"
  "/// ${1:summary}\n/// $0")
```

The template syntax:

- `$1`, `$2`, … — ordered tab-stops you move between with <kbd>C-c C-f</kbd> / <kbd>C-c C-b</kbd>.
- `${1:default}` — a tab-stop pre-filled with placeholder text (selected on arrival, so typing replaces it).
- `$0` — where the cursor ends up after the last field.
