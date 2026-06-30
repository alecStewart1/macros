---
title: Tutorial
description: The interactive, in-editor tutorial — learn by doing.
---

# Tutorial

The fastest way to learn Macros is the **interactive tutorial**, the same idea as GNU Emacs's `TUTORIAL`. Open it with:

| Key | Command |
| --- | --- |
| <kbd>C-h t</kbd> | `help-with-tutorial` |

## How it works

The tutorial opens a normal, **editable** buffer — you read the text and practice the commands right there, on your own copy. Lines beginning with `>>` tell you a command to try at that spot. Because it's a real buffer, nothing is simulated: when the tutorial says to delete a word or split a window, you actually do it.

It stays faithful to the GNU Emacs tutorial, but every key it teaches is a binding that genuinely exists in Macros (see [Keybindings](/docs/keybindings)).

## Notation it uses

- <kbd>C-&lt;chr&gt;</kbd> — hold **Control** and press the character.
- <kbd>M-&lt;chr&gt;</kbd> — hold **Meta** (Alt/Option) and press the character, or press <kbd>Esc</kbd> then the character.

## What it covers

- Basic cursor motion and scrolling.
- Deleting, killing (cutting), and yanking (pasting).
- The mark and the region.
- Files and buffers — opening, saving, switching.
- Windows — splitting and moving between panes.
- Getting more help.

> Macros uses standard Emacs bindings by default. If you've opted into modal editing — [evil](/docs/modal-editing) or [helix](/docs/helix) — remember you move in Normal state with <kbd>h</kbd>/<kbd>j</kbd>/<kbd>k</kbd>/<kbd>l</kbd> and type in Insert state. The tutorial's Emacs-style motion keys (<kbd>C-f</kbd>, <kbd>C-n</kbd>, …) work in every state.

When you're done, explore further with <kbd>M-x</kbd>, <kbd>C-h f</kbd>, and <kbd>C-h a</kbd> — see [Getting help](/docs/getting-help).
