---
title: Ediff (diff & merge)
description: View color-coded file diffs and interactively merge hunks between two buffers.
---

# Ediff

Compare files and merge changes between them, built on the editor's diff primitive (Emacs `ediff`). A diff opens as a read-only, color-coded view; the interactive merge tool lets you copy hunks back and forth between two open buffers.

## Diffing files

| Command | Action |
| --- | --- |
| `ediff` | Prompt for two files and show their diff |
| `ediff-current-with` | Diff the current file against another file you pick |

The diff opens in a read-only `diff-mode` buffer with added and removed lines colored. Press <kbd>q</kbd> to close it.

## Merging between buffers

`ediff-buffers` picks two open buffers and lists their differing hunks in an interactive `ediff-mode` buffer, where you can copy a hunk from one side into the other.

| Key | Command | Action |
| --- | --- | --- |
| <kbd>n</kbd> | `ediff-next-hunk` | Move to the next hunk |
| <kbd>p</kbd> | `ediff-prev-hunk` | Move to the previous hunk |
| <kbd>a</kbd> | `ediff-copy-a->b` | Copy the hunk at point from buffer A into B |
| <kbd>b</kbd> | `ediff-copy-b->a` | Copy the hunk at point from buffer B into A |
| <kbd>g</kbd> | | Refresh the diff |
| <kbd>q</kbd> | `kill-buffer` | Close |

The entry commands (`ediff`, `ediff-current-with`, `ediff-buffers`) have no default key — run them by name with <kbd>M-x</kbd>, or bind them in your `init.scm`. For a Git-focused diff of staged/unstaged changes, see [Git (Magit)](/docs/git).
