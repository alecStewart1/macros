;; tutorial.scm — an interactive Emacs-style tutorial (help-with-tutorial, C-h t).
;;
;; Like Emacs's TUTORIAL, this opens a normal *editable* buffer: you read the
;; text and practice the commands right here, on this copy. Lines beginning with
;; ">>" tell you a command to try at that spot. Loaded after help.scm so the
;; help-mode/C-h prefix already exists.

;; The tutorial body. Kept faithful to the GNU Emacs TUTORIAL, but every key it
;; teaches is a binding that actually exists in Macros (see fundamental.scm).
;; Conventions: C-<chr> = hold Control + <chr>; M-<chr> = hold Meta (Alt/Option)
;; + <chr>, or press ESC then <chr>.
(define *tutorial-text* "\
MACROS TUTORIAL.  See the end for copying conditions.

Welcome to Macros — an Emacs-style editor.  Macros commands generally
involve the CONTROL key (sometimes
labeled CTRL) or the META key (labeled ALT, or Option on a Mac).  Rather than
write that out in full each time, we use the following abbreviations:

 C-<chr>  means hold the CONTROL key while typing the character <chr>.
          Thus, C-f would be: hold the CONTROL key and type f.
 M-<chr>  means hold the META (ALT or Option) key while typing <chr>.
          If there is no META key, you can type ESC, release it, then <chr>.

IMPORTANT: to end this tutorial, type C-x C-c (kill Macros) or C-x k (just kill
this buffer).  Don't worry: you cannot break anything by experimenting below.

The lines that begin \">>\" are directions for you to try a command.  For
instance:

>> Now type C-l (control-L) to clear the screen and redraw it, centering the
   line the cursor is on.  (If your terminal scrolls, this is handy.)

Note: typing C-l a second and third time moves the current line to the top
and bottom of the window.


* BASIC CURSOR CONTROL
----------------------

Moving from screenful to screenful is useful, but how do you reposition the
cursor to a specific place on the screen?

There are several ways you can do this.  The most basic is to use the commands
C-p, C-n, C-b, and C-f.  Each of these moves the cursor one row or column in a
particular direction:

                          Previous line, C-p
                                  :
                                  :
     Backward, C-b .... Current cursor position .... Forward, C-f
                                  :
                                  :
                            Next line, C-n

>> Move the cursor to the line in the diagram above using C-n or C-p.  Then
   type C-l to see the whole diagram centered in the screen.

You'll probably find it easy to think of these by letter: P for previous, N for
next, B for backward, and F for forward.  These are the basic cursor-positioning
commands, and you'll use them ALL the time, so learn them now.

>> Type a few C-n's to bring the cursor down to this line.

>> Move into the line with C-f's and then up with C-p's.  See what C-p does
   when the cursor is in the middle of the line.

Two other simple cursor-motion commands are M-b (backward-word) and M-f
(forward-word), which move backward or forward by whole words.

>> Type a few M-f's and M-b's, mixed in with some C-f's and C-b's, so you can
   see what they do to and between words.

C-a and C-e move to the beginning and end of a line.

>> Try a couple of C-a's, and then a couple of C-e's.  Notice that repeating
   C-a or C-e at the same place does nothing more.

Two other very important cursor-motion commands are M-< (Meta Less-than), which
moves to the beginning of the whole text, and M-> (Meta Greater-than), which
moves to the end of the whole text.

>> Try M-< now, to move to the beginning of the tutorial.  Then use C-n
   repeatedly to move back here.

The cursor's position in the text is also called \"point\".  Most commands
operate relative to point.


* IF MACROS STOPS RESPONDING
---------------------------

If Macros is waiting for you in the middle of a multi-key command (like C-x ...)
and you change your mind, you can quit with the command C-g.

>> Type C-g now.  The echo area at the bottom should show \"Quit\".

C-g also stops a command that is taking too long, and clears a partial key
sequence that you started by mistake.


* WINDOWS
---------

Macros can have several panes (\"windows\"), each showing its own text.

>> Type C-x 1 (digit one) to delete every window except the one the cursor is
   in, so you get a single full-size window again.  (This won't lose any text.)

The command is Control-x followed by the digit 1.  C-x is a \"prefix\": several
commands begin with it.

>> Try C-x 2 to split the window in two, then C-x o (other-window) to hop the
   cursor between them.  When done, C-x 1 collapses back to one window.


* INSERTING AND DELETING
------------------------

To insert text, just type it.  Characters you can see, like A, 7, *, etc., are
inserted as you type them.  Type <Return> (the Enter key) to insert a newline.

To delete the character just before the cursor, type <Backspace>.  To delete
the character the cursor is ON (after it), type C-d.

>> Do some typing on this line, then use <Backspace> to delete it again.
   Don't worry about changing this file; you won't affect the original tutorial.

>> Now start typing text until you reach the right margin, and keep typing.
   When a line gets too long, it continues onto the next screen row.

>> Use C-d's to delete characters at the cursor, until the line fits again.

When you delete more than one character at a time, it is handy to delete a
whole word at once:

 M-<Backspace>  Cut the word just BEFORE the cursor (backward-kill-word)
 M-d            Cut the next word AFTER the cursor (kill-word)

>> Type a few words, then try M-<Backspace> and M-d on them.

C-k cuts (\"kills\") from the cursor to the end of the line.

>> Put the cursor at the start of this line and type C-k.  Type C-k again to
   kill the newline too.

Notice that <Backspace>, C-d, M-d, M-<Backspace>, and C-k all save the killed
text so it can be brought back.  Killing something big saves it; deleting a
single character does not.


* THE KILL RING AND YANKING
---------------------------

Killed text is saved on the \"kill ring\".  To bring it back, type C-y (yank).

>> Try it: type C-k to kill a line, then C-y to bring it back.

C-y yanks the most recently killed text.  To replace it with text you killed
earlier, type M-y after a C-y to cycle through the ring.

You can also mark a region and copy it without deleting:

 C-<Space>   Set the mark here (one end of a region)
 C-w         Cut (kill) the region between mark and cursor
 M-w         Copy the region to the kill ring, leaving the text in place

>> Type C-<Space> here, move down a few lines with C-n, then C-w to cut the
   region.  Bring it back with C-y.


* UNDO
------

If you change text and then decide it was a mistake, undo the change with C-/
(or C-x C-u; both run undo).

>> Kill this line with C-k, then type C-/ to bring it back.

Repeating undo keeps undoing earlier changes.


* FILES
-------

To make your edits to a file permanent, you must \"visit\" the file, edit it,
and save it.

To visit a file, type C-x C-f (find-file), then type the file name in the
minibuffer at the bottom of the screen, and press <Return>.

>> Type C-x C-f.  Then type C-g to cancel the prompt (we don't actually want to
   open a file right now).

When you have made changes, save them with C-x C-s (save-buffer).  The text is
copied into the file, and the buffer is marked as no longer modified.


* BUFFERS
---------

Each file you visit is held in its own \"buffer\".  Even buffers that aren't
files (like this tutorial, or *Messages*) live in buffers.

>> Type C-x b (switch-buffer).  A list pops up; type the name of a buffer to
   switch to it, or C-g to cancel.

>> Type C-x C-b (list-buffers) to see all your buffers in one list.

>> Move between buffers in order with C-x C-<Left> and C-x C-<Right>.

>> Type C-x k (kill-buffer) to remove a buffer when you're done with it.


* SEARCHING
-----------

Macros can search incrementally for a string.  \"Incremental\" means the search
happens as you type: it jumps to the first match while you're still typing.

The command to start a forward search is C-s, and C-r starts a backward search.
BUT WAIT!  Don't try them yet.

When you type C-s you'll see the string \"I-search\" appear in the echo area at
the bottom.  This tells you Macros is waiting for you to type what to search for.
<Return> ends the search and leaves the cursor on the match.

>> Now type C-s to start a search.  SLOWLY, one letter at a time, type the word
   \"cursor\", pausing after each letter to notice where the cursor goes.  You
   have now searched for \"cursor\", one character at a time!

>> Type C-s again, to search for the next occurrence of \"cursor\".

>> Now type <Backspace> four times and see how the cursor moves back to earlier
   matches as you shorten the search string.

>> Type <Return> to finish the search and leave the cursor on the match.

If you search and the string is not found, the echo area shows \" [failing]\".
C-g cancels a search, returning the cursor to where it was when you started.

>> Type C-s to begin a search, type \"xyzzy\" (which probably isn't here), see
   the \" [failing]\" hint, then type C-g and watch the cursor jump back.

To repeat a search for the same string, type C-s C-s: the first C-s starts a
search, and the second recalls the string you searched for last time.

To search backward, use C-r in place of C-s.  Within a search, C-r reverses the
direction and C-s sets it forward again.


* EXTENDING THE COMMAND SET (M-x)
---------------------------------

There are many, many more commands than there are keys to bind them to.  To run
any command by name, type M-x, then the command name, then <Return>.  A
fuzzy-matching list helps you find it.

>> Type M-x and start typing \"apropos\".  Pick apropos and press <Return>;
   then type a word like \"line\" to list commands whose names contain it.

Some useful commands you can reach this way:

 M-x replace-string    Replace all occurrences of one string with another.
 M-x occur             List every line in the buffer matching a pattern.
 M-x comment-dwim      Comment or uncomment the current line/region (M-;).


* GETTING MORE HELP
-------------------

All help in Macros starts with the C-h prefix.  The most useful ones:

 C-h k <key>   Describe what a key does (describe-key).
 C-h b         List every key binding in effect (describe-bindings).
 C-h a         Search command names for a word (apropos).
 C-h t         Start this tutorial again (help-with-tutorial).

>> Type C-h k C-p to see that C-p runs the command next-line... er, previous
   line.  (Try C-h k on a few keys you've learned.)

>> Type C-h b to see the full list of bindings.  Type q to close that buffer.


* MORE FEATURES
---------------

Macros has much more: LSP-powered code navigation, a magit-style git interface,
projectile project search, dired, multiple windows, keyboard macros, themes,
and a full Steel (Scheme) extension language.  Explore with C-h a and M-x.

This is the end of the tutorial.

To delete this buffer, type C-x k <Return>.  To quit Macros, type C-x C-c.


* COPYING
---------

This tutorial descends from a long line of Emacs tutorials.  The original GNU
Emacs tutorial is Copyright (C) the Free Software Foundation and distributed
under the GNU GPL; this adaptation follows Macros' own license.
")

;; help-with-tutorial (C-h t): open a fresh, editable copy of the tutorial and
;; put point at the top so the reader starts from the beginning. Rebuilding from
;; scratch each time gives a clean slate (Emacs prompts to reuse; we just reset).
(define (help-with-tutorial)
  (kill-buffer-named "*TUTORIAL*")
  (append-to-buffer "*TUTORIAL*" *tutorial-text*)
  (goto-char 0)
  (message "Welcome to the Macros tutorial. Try the commands marked with >>."))

(define-key "fundamental" "ctrl-h t" "help-with-tutorial")
(register-command "help-with-tutorial")
