---
title: Git (Magit)
description: A Magit-style git porcelain — stage, commit, branch, and read diffs from a buffer.
---

# Git (Magit)

Macros includes a **Magit-style** git porcelain: an interactive status buffer where you stage hunks, write commits, branch, push, pull, stash, and read diffs. The porcelain — the sectioned view, the transient popups, the keymaps — is written **entirely in Scheme** on the editor's special-buffer and transient primitives. Underneath, git data comes from two sources: pure-Rust [**gitoxide** (`gix`)](https://github.com/GitoxideLabs/gitoxide) primitives for metadata reads (the HEAD commit, recent log, branches, tags, remotes, repo state, and blame) with no subprocess at all, falling back to shelling out (`call-process` to the `git` binary) for the working-tree scan, diffs, and mutating actions like stage / commit / push. The status buffer is a read-only faced buffer that's rebuilt on every refresh.

## Opening the status buffer

Run `magit-status` with <kbd>C-x g</kbd> (or <kbd>M-x</kbd> `magit-status`). You get the familiar sectioned view: untracked, unstaged, staged, and recent commits, with colored diffs and per-file status labels.

## Status buffer keys

### Navigation & folding

| Key | Action |
| --- | --- |
| <kbd>n</kbd> / <kbd>p</kbd> | move line by line |
| <kbd>M-n</kbd> / <kbd>M-p</kbd> | next / previous section |
| <kbd>Tab</kbd> | fold / unfold the section at point |
| <kbd>S-Tab</kbd> | cycle all sections |
| <kbd>Enter</kbd> | visit the file at point |
| <kbd>g</kbd> | refresh |
| <kbd>q</kbd> | close |

### Staging

| Key | Action |
| --- | --- |
| <kbd>s</kbd> / <kbd>S</kbd> | stage the item / stage all |
| <kbd>u</kbd> / <kbd>U</kbd> | unstage the item / unstage all |
| <kbd>k</kbd> | discard the change at point |

The "item" follows point: a hunk, a file, a selected region of files/lines — or, on a section header (<kbd>s</kbd> on **Untracked files** / **Unstaged changes**, <kbd>u</kbd> on **Staged changes**), the whole section.

### Transient popups

Each of these opens a transient menu (a popup of further keys), mirroring Magit:

| Key | Popup |
| --- | --- |
| <kbd>c</kbd> | commit |
| <kbd>b</kbd> | branch |
| <kbd>P</kbd> / <kbd>F</kbd> | push / pull |
| <kbd>f</kbd> | fetch |
| <kbd>l</kbd> | log |
| <kbd>z</kbd> | stash |
| <kbd>m</kbd> | merge |
| <kbd>r</kbd> | rebase |
| <kbd>t</kbd> | tag |
| <kbd>X</kbd> | reset |
| <kbd>M</kbd> | remote |
| <kbd>B</kbd> | blame |
| <kbd>?</kbd> | dispatch (all commands) |

Other handy keys: <kbd>i</kbd> add to `.gitignore`, <kbd>y</kbd> copy a ref, <kbd>$</kbd> show the git process output, <kbd>H</kbd> toggle history.

## Committing

Press <kbd>c</kbd> to open the commit popup, then start a commit. The message buffer is in `git-commit-mode`:

| Key | Action |
| --- | --- |
| <kbd>C-c C-c</kbd> | confirm and create the commit |
| <kbd>C-c C-k</kbd> | cancel |

## Log

The log popup (<kbd>l</kbd>) opens a `magit-log-mode` buffer. From there: <kbd>Enter</kbd> shows a commit, <kbd>A</kbd> cherry-picks, <kbd>v</kbd> reverts, <kbd>q</kbd> closes.

## Inline blame

Annotate the commit that last touched each line as faded virtual text at the end of the line (GitLens-style). This works in any file buffer — you don't need the status buffer open:

| Key | Action |
| --- | --- |
| <kbd>C-x v b</kbd> | toggle blame on every line |
| <kbd>C-x v B</kbd> | toggle blame on the current line only (follows the cursor) |
| <kbd>C-x v Enter</kbd> | show the commit that last touched this line |
| <kbd>C-x v y</kbd> | copy that commit's hash |

Each annotation shows the short hash, author, a humanized date ("3 weeks ago"), and the commit summary. Full-file mode (<kbd>C-x v b</kbd>) collapses runs of lines from the same commit to a single annotation and tints by age — recent edits pop, ancient code recedes. Current-line mode (<kbd>C-x v B</kbd>) shows one quiet grey annotation that re-places itself as point moves. All four commands are also available via <kbd>M-x</kbd>.

The status buffer's <kbd>B</kbd> is a separate view: it opens a full `git blame` of the file at point in a read-only buffer.

## Git gutter (diff-hl)

In any file buffer, Macros marks the lines you've changed since `HEAD` in the line-number gutter, the way Emacs's `diff-hl` / `git-gutter` do — a small colored marker at the gutter's left edge on each changed line:

| Marker | Meaning | Face |
| --- | --- | --- |
| green bar | line added since `HEAD` | `magit-gutter-added` |
| amber bar | line modified since `HEAD` | `magit-gutter-modified` |
| red mark | line(s) deleted just above this point | `magit-gutter-removed` |

The marks come from diffing the buffer's **live text** (not the on-disk file) against the file's blob as committed at `HEAD`, computed in pure Rust via gitoxide — no subprocess. A brand-new, untracked file reads as entirely added.

### Refreshing

The gutter recomputes when a buffer is saved (`after-save-hook`) or first opened (`find-file-hook`). To make it update edit-by-edit, add the refresh to a more frequent hook in your `init.scm`:

```scheme
(add-hook "post-command-hook" "magit-gutter-refresh")
```

You can also force a recompute at any time with <kbd>M-x</kbd> `magit-gutter-refresh`, and toggle the whole feature on/off for the session with <kbd>M-x</kbd> `magit-gutter-mode`.

### Customizing

The gutter is on by default. Tune it with `set-option`:

```scheme
(set-option "magit-gutter" #f)          ; disable the gutter entirely
(set-option "magit-gutter-width" 3)     ; bar width in px (1–8, default 2)
(set-option "magit-gutter-deleted" #f)  ; hide the deletion marks
```

Recolor the markers by redefining their faces (in `init.scm`, or in a theme so they survive a runtime `load-theme`):

```scheme
(set-face "magit-gutter-added" "#6cae3e")
(set-face "magit-gutter-modified" "#c7a23a")
(set-face "magit-gutter-removed" "#c1574b")
```

Like the [diagnostic gutter dots](/docs/diagnostics), the markers are only drawn when the line-number gutter itself is shown for that buffer.

## Customizing

`magit-status` is on <kbd>C-x g</kbd> by default. Rebind it, or any key inside the status buffer (the `"magit-status"` map), from your `init.scm`:

```scheme
;; Rebind the global entry point:
(define-key "fundamental" "ctrl-x g" 'magit-status)

;; Rebind keys inside the status buffer — e.g. make `C` open the commit popup:
(define-key "magit-status" "shift-c" 'magit-commit-popup)
```

## How it's built

Because Magit here is plain Scheme over a few primitives — `gix-*` (read metadata, log, and blame straight from gitoxide, no subprocess), `call-process` (run the `git` binary for the working-tree scan, diffs, and mutating actions), `render-buffer` (draw a faced read-only buffer), `buffer-line` / `goto-line` (find and restore the section at point), and `transient` (the popups) — you can read it, extend it, add your own popups, or change any binding. Folding works by regeneration: a refresh simply rebuilds the buffer text. See [Scripting with Steel](/docs/scripting).
