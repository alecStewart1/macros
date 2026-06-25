---
title: Modal editing (evil)
description: Vim-style modal editing — motions, operators, and text objects, the way evil-mode works in Emacs.
---

# Modal editing (evil)

Macros ships with **evil-mode**: Vim-style modal editing built on the editor's keymaps. It mirrors GNU Emacs's evil-mode — plain Vim modal editing, **no leader key**. It is **not on by default**; Macros uses standard Emacs bindings unless you opt in.

Enable it from your `init.scm` (see [Configuration](/docs/configuration)):

```scheme
(evil-mode)     ; opt in to Vim-style modal editing
(evil-disable)  ; turn it back off
```

The Rust side only decides *when* plain keys are commands (Normal/Visual states) versus text (Insert state); every binding is Scheme and fully overridable.

## States

- **Normal** — keys are commands. This is where you start.
- **Insert** — keys insert text. Enter with <kbd>i</kbd> / <kbd>a</kbd> / <kbd>o</kbd> (and capitals); leave with <kbd>Esc</kbd>.
- **Visual** — character selection for operators. Enter with <kbd>v</kbd>.

<kbd>Esc</kbd> (`keyboard-quit`) always returns you to Normal.

## Normal-state motions

| Key | Moves |
| --- | --- |
| <kbd>h</kbd> <kbd>j</kbd> <kbd>k</kbd> <kbd>l</kbd> | left / down / up / right |
| <kbd>w</kbd> / <kbd>b</kbd> | forward / backward a word |
| <kbd>0</kbd> / <kbd>$</kbd> | start / end of line |
| <kbd>g g</kbd> / <kbd>G</kbd> | start / end of buffer |

## Entering insert state

| Key | Action |
| --- | --- |
| <kbd>i</kbd> / <kbd>I</kbd> | insert before cursor / at first non-blank |
| <kbd>a</kbd> / <kbd>A</kbd> | append after cursor / at end of line |
| <kbd>o</kbd> / <kbd>O</kbd> | open a line below / above |

## Changing text

| Key | Action |
| --- | --- |
| <kbd>x</kbd> | delete the character under the cursor |
| <kbd>d d</kbd> | delete the line |
| <kbd>y y</kbd> | yank the line (also copies to the system clipboard) |
| <kbd>y w</kbd> / <kbd>y $</kbd> | yank a word / to end of line |
| <kbd>p</kbd> | paste |
| <kbd>u</kbd> | undo |

Normal-mode yanks copy to both the kill ring **and** the system clipboard, and restore the cursor to where the yank started — so <kbd>y y</kbd> then <kbd>cmd-v</kbd> in another app works.

## Search

| Key | Action |
| --- | --- |
| <kbd>/</kbd> / <kbd>?</kbd> | search forward / backward |
| <kbd>n</kbd> / <kbd>N</kbd> | next / previous match |

## Ex commands

Press <kbd>:</kbd> to enter an Ex command. Substitution works as you'd expect:

```
:%s/old/new/g
```

## Scrolling

| Key | Action |
| --- | --- |
| <kbd>C-d</kbd> / <kbd>C-u</kbd> | half-page down / up |

## No leader key by default

Like vanilla evil-mode in Emacs, Macros' evil has **no <kbd>Space</kbd> leader**. A leader menu is a Doom/Spacemacs convention, not part of evil itself, so it's left to you. Adding one is a few lines in your `init.scm`:

```scheme
;; Roll your own Spacemacs-style leader. evil-bind-key takes the command
;; function directly (a string command name works too).
(evil-bind-key "evil-normal" "space space" helm-execute-command)
(evil-bind-key "evil-normal" "space f f"   helm-find-files)
(evil-bind-key "evil-normal" "space b b"   helm-buffers)
```

## Customizing

evil bindings live in the `"evil-normal"`, `"evil-insert"`, and `"evil-visual"` maps. Rebind exactly like any other key:

```scheme
(define-key "evil-normal" "shift-q" "kill-buffer")
```

Because it's all Scheme, you can add operators, text objects, and leader sub-maps of your own. See [Scripting with Steel](/docs/scripting).
