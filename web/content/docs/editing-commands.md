---
title: Editing commands
description: Region and kill-ring editing, search and replace, occur, sorting, and case commands.
---

# Editing commands

Beyond raw motion, Macros provides the core Emacs editing commands — region-aware where Emacs is, and each command's edits coalesce into a single undo group so they undo as a unit.

## The region and the kill ring

Set the mark with <kbd>C-Space</kbd>, then move point; the text between mark and point is the **region**. Commands that say "region" act on it (or fall back sensibly when there's no active region).

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-Space</kbd> | `set-mark` | Start a selection |
| <kbd>C-w</kbd> | `kill-region` | Cut the region to the kill ring |
| <kbd>M-w</kbd> | `kill-ring-save` | Copy the region |
| <kbd>C-y</kbd> | `yank` | Paste the latest kill |
| <kbd>M-y</kbd> | `yank-pop` | Cycle through earlier kills (after a yank) |
| <kbd>C-x C-x</kbd> | `exchange-point-and-mark` | Swap point and mark |
| <kbd>C-x C-h</kbd> / <kbd>cmd-a</kbd> | `mark-whole-buffer` | Select all |

The kill ring and the system clipboard are kept in sync by the clipboard commands (<kbd>cmd-c</kbd>/<kbd>x</kbd>/<kbd>v</kbd>), so copying in Macros and pasting in another app just works.

## Killing & transposing

| Key | Command |
| --- | --- |
| <kbd>C-k</kbd> | `kill-line` |
| <kbd>M-d</kbd> / <kbd>M-Backspace</kbd> | `kill-word` / `backward-kill-word` |
| <kbd>M-z</kbd> | `zap-to-char` |
| <kbd>C-t</kbd> | `transpose-chars` |
| <kbd>C-x C-t</kbd> | `transpose-lines` |
| <kbd>M-^</kbd> | `join-line` |
| <kbd>C-o</kbd> | `open-line` |

## Search

Incremental search, Emacs-style:

| Key | Command |
| --- | --- |
| <kbd>C-s</kbd> | `isearch-forward` |
| <kbd>C-r</kbd> | `isearch-backward` |

Type to extend the match; <kbd>C-s</kbd> / <kbd>C-r</kbd> again to jump to the next/previous hit; <kbd>C-g</kbd> to cancel and return to where you started.

## Replace

`replace-string` performs a literal, whole-buffer replacement:

```
M-%   →  replace-string
```

It prompts for the search text and the replacement and runs over the buffer; the edits form one undo group.

## Occur

<kbd>C-c o</kbd> (`occur`) prompts for a pattern and opens an `*occur*` buffer listing every matching line. Move with <kbd>n</kbd> / <kbd>p</kbd>, press <kbd>Enter</kbd> to jump to a match in the source, <kbd>q</kbd> to close.

## Comment toggling

<kbd>M-;</kbd> (`comment-dwim`) toggles a line comment on the current line (or region). The comment prefix is a buffer-local string set per mode (`comment-string`), defaulting to `// `; modes like `steel-mode` set their own.

## Sorting & case

These act on the region, or the whole buffer when there's no region:

- `sort-lines` — sort the lines.
- `upcase-region` / `downcase-region` / `capitalize-region` — case conversion.

Run them by name with <kbd>M-x</kbd>, or bind them in your `init.scm`.

## Undo & redo

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-/</kbd> | `undo` | Undo the last change |
| <kbd>C-?</kbd> | `redo` | Redo |

Undo is **branching** (Emacs `undo-tree`): undoing and then making a new edit starts a branch rather than discarding the redone edits, so no work is ever lost. <kbd>M-x</kbd> `vundo` opens a read-only view of the undo tree (<kbd>g</kbd> refreshes, <kbd>q</kbd> closes); navigate the history itself with <kbd>C-/</kbd> and <kbd>C-?</kbd>.

## Expanding the selection

Grow the region outward along tree-sitter syntax boundaries — word, then expression, statement, block (Emacs `expand-region`). It works in any buffer with a tree-sitter grammar, no language server needed.

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-=</kbd> | `er/expand` | Widen the selection to the enclosing node |
| <kbd>M-=</kbd> | `er/contract` | Step back to the previous, narrower selection |

In evil visual state, <kbd>+</kbd> and <kbd>-</kbd> do the same.

## Multiple cursors

Edit in many places at once (Emacs `mc.el`). Add secondary cursors, then typed text, <kbd>Enter</kbd>, and <kbd>Backspace</kbd> are replicated at every cursor.

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-c C-n</kbd> | `mc-add-cursor-below` | Add a cursor on the next line |
| <kbd>C-c C-p</kbd> | `mc-add-cursor-above` | Add a cursor on the previous line |
| <kbd>C-c C-d</kbd> | `mc-mark-next-like-this` | Add a cursor at the next occurrence of the word at point |
| <kbd>C-g</kbd> | `mc-clear` | Collapse back to a single cursor |

Plain motions move only the primary cursor — add the cursors first, then type or delete.

## Jumping to a character

Jump anywhere on screen in a couple of keystrokes (Emacs `avy` / `ace-jump`).

| Key | Command | Action |
| --- | --- | --- |
| <kbd>C-c j</kbd> | `avy-goto-char` | Type a character; every visible occurrence is tagged with a hint letter — press the letter to jump |

In evil normal state this is <kbd>g s</kbd>. <kbd>C-g</kbd> cancels the jump.

## The buffer list

<kbd>C-x C-b</kbd> (`ibuffer`) opens a buffer manager (Emacs `ibuffer`) listing every open buffer. <kbd>Enter</kbd> switches to the buffer on the line, <kbd>d</kbd> or <kbd>x</kbd> kills it, <kbd>g</kbd> refreshes. (To pick a buffer through Helm instead, use <kbd>C-x b</kbd>.)
