---
title: Git (Magit)
description: A Magit-style git porcelain — stage, commit, branch, and read diffs from a buffer.
---

# Git (Magit)

Macros includes a **Magit-style** git porcelain: an interactive status buffer where you stage hunks, write commits, branch, push, pull, stash, and read diffs. It's built **entirely in Scheme** on the editor's special-buffer and transient primitives — the Rust side knows nothing about git. It just shells out (`call-process`), renders a read-only faced buffer, and rebuilds that buffer on every refresh.

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

## Customizing

`magit-status` is on <kbd>C-x g</kbd> by default. Rebind it, or any key inside the status buffer (the `"magit-status"` map), from your `init.scm`:

```scheme
;; Rebind the global entry point:
(define-key "fundamental" "ctrl-x g" "magit-status")

;; Rebind keys inside the status buffer — e.g. make `C` open the commit popup:
(define-key "magit-status" "shift-c" "magit-commit-popup")
```

## How it's built

Because Magit here is plain Scheme over a few primitives — `call-process` (run git), `render-buffer` (draw a faced read-only buffer), `buffer-line` / `goto-line` (find and restore the section at point), and `transient` (the popups) — you can read it, extend it, add your own popups, or change any binding. Folding works by regeneration: a refresh simply rebuilds the buffer text. See [Scripting with Steel](/docs/scripting).
