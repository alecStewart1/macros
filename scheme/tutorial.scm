;; tutorial.scm — an interactive, hands-on tutorial (help-with-tutorial, C-h t).
;;
;; This opens a normal *editable* buffer: you read the text and practice the
;; commands right here, on this copy. Lines that begin with ">>" tell you a
;; command to try at that spot. Loaded after help.scm so the C-h prefix exists.
;;
;; The text is original to Macros. Every key it teaches is a binding that
;; actually exists (see fundamental.scm / undo-tree.scm). The lessons follow a
;; vimtutor-style progression: move, then how to leave, then delete (small to
;; large), undo, paste, navigate and search, files, and finally selecting and
;; copying text.
;;
;; Conventions used in the text: C-<chr> = hold Control + <chr>; M-<chr> = hold
;; Meta (Alt, or Option on a Mac) + <chr>, or press Esc, release, then <chr>.
(define *tutorial-text* "\
MACROS TUTORIAL

Welcome.  This is a hands-on tour of Macros.  You are reading it inside an
ordinary, editable buffer, so you can practice every command right here, on this
text, as you go.  Nothing you do is permanent: this copy is rebuilt fresh each
time you open the tutorial (C-h t), so experiment freely.

A couple of keys are written in shorthand:

 C-x   hold Control and press x.
 M-x   hold Meta and press x.  Meta is Alt on most keyboards, or Option on a
       Mac.  No Meta key?  Press Esc, let go, then the next key.

Some commands are two keystrokes in a row, like C-x C-s.  Just type them in
order.  To type ordinary text, simply type it; Backspace removes the character
before the cursor.

Lines that begin with >> are things to try.  When you reach one, do what it says
before reading on.  Like this:

>> Press the Down arrow a few times, until the cursor sits on this line.


* LESSON 1.1: MOVING AROUND
---------------------------
Macros calls the cursor's position 'point', and almost every command acts from
there.  The four arrow keys move it the obvious way:

   Up      previous line
   Down    next line
   Left    one character back
   Right   one character forward

>> Spend a moment steering.  Move Up and Down through this paragraph, then Left
   and Right along a line.  Notice that point sits between characters.

If you would rather keep your hands on the home row, Macros gives you the same
four moves without reaching for the arrows:

   C-p   up    (previous line)        C-n   down  (next line)
   C-b   left  (back one char)        C-f   right (forward one char)

>> Walk down and up these lines with C-n and C-p, then slide across one with C-f
   and C-b.  They do exactly what the arrows do; use whichever you prefer.


* LESSON 1.2: LEAVING AND CANCELLING
------------------------------------
Before going further, learn how to get out of trouble.  If a command seems
half-finished, or you started one by accident, press C-g.  It cancels the
half-typed command and returns you to a known state.

>> Type C-x to begin a command, then change your mind and press C-g.  The echo
   area at the bottom confirms the quit.

When you are done with a buffer, C-x k closes it.  To quit Macros entirely, type
C-x C-c.  (You do not need to leave now; this is just so you are never stuck.)


* LESSON 1.3: DELETING TEXT
---------------------------
Macros can delete in bites of any size.  Smallest first:

   Backspace      delete the character before point
   C-d            delete the character under point
   M-Backspace    delete the whole word before point

>> Type a few junk words on this line, then clean them up with Backspace, C-d,
   and M-Backspace.

To remove a line at a time, use C-k.  It deletes from point to the end of the
line; press it a second time and it removes the now-empty line too, pulling the
following line up.

>> Put point at the very start of the next line, then press C-k twice:
This is a throwaway practice line.  Kill it and watch the text close up.

Deleting more than a character is called 'killing', and it is not destructive:
the text is set aside so you can get it back.  That is the next two lessons.


* LESSON 1.4: UNDO AND REDO
---------------------------
Made a mess?  Undo it with C-/.  Each press steps one change further back.

>> Kill this line with C-k, then press C-/ to bring it back.

Changed your mind about an undo?  Redo with C-?, which re-applies the change you
just took back.

>> Undo a few of your edits with C-/, then walk forward through them with C-?.

Undo in Macros never loses work: even edits you undo and then type over are kept
on a branch you can return to.  (If you prefer, C-x C-u also undoes.)


* LESSON 1.5: PASTING (PUTTING TEXT BACK)
-----------------------------------------
Whatever you kill is saved.  To paste the most recent kill at point, press C-y
(this is called a 'yank').

>> Kill the next line with C-k (point at its start), then press C-y to paste it
   straight back where it was:
Kill me, then yank me back, and I should return unchanged.

A yank is a copy, so you can repeat it: move elsewhere and press C-y again to
drop another copy.  Macros keeps a history of your kills.  Right after a yank,
press M-y to swap in the kill before it, and again to keep stepping back.

>> Kill two different lines with C-k, press C-y to paste the most recent, then
   M-y to replace it with the earlier one.

That is the core loop: move, delete, undo, paste.  Everything below makes you
faster and helps you work across a whole project.


* LESSON 2.1: MOVING FASTER
---------------------------
Stepping one line or character at a time gets old.  These jump farther:

   C-a   start of line        C-e   end of line
   M-f   forward one word     M-b   backward one word
   M-<   top of the buffer    M->   bottom of the buffer

>> Try C-a and C-e to snap to the ends of this line.  Then sweep across a
   sentence with a few M-f's and M-b's.  Finish with M-< and M-> to leap to the
   top and bottom of the tutorial (then come back here).


* LESSON 2.2: FINDING TEXT
--------------------------
C-s searches forward as you type ('incremental' search): it jumps to the first
match while you are still typing, and refines as you add letters.  C-s again
finds the next match; Return leaves point on the match; C-g cancels and returns
point to where you started.  C-r searches backward.

>> Press C-s and slowly type the word region, letter by letter, watching point
   chase the match.  Press C-s once more to find the next one, then Return.


* LESSON 3.1: FILES
-------------------
To keep your edits, you visit a file, change it, and save it.

   C-x C-f   open (visit) a file by name
   C-x C-s   save the current buffer back to its file

>> Press C-x C-f to start opening a file, then C-g to cancel the prompt (we are
   not really opening one right now).


* LESSON 3.2: BUFFERS
---------------------
Every file you open lives in its own 'buffer'.  Buffers that are not files (this
tutorial, the *Messages* log) are buffers too.

   C-x b     switch to another buffer by name
   C-x C-b   list every open buffer
   C-x k     kill (close) a buffer you are done with

>> Press C-x C-b to see your buffers, then C-x b to switch between them (C-g
   cancels the prompt).


* LESSON 3.3: WINDOWS
---------------------
Macros can show several panes at once; it calls them 'windows'.

   C-x 2   split the current window into two
   C-x o   move point to the other window
   C-x 1   collapse back to a single window (this loses no text)

>> Try C-x 2 to split, C-x o to hop between the panes, and C-x 1 to return to
   one window.


* LESSON 4.1: SELECTING AND COPYING (A REGION)
----------------------------------------------
So far you have worked one line at a time.  To act on several lines at once,
mark a 'region': set one end, move point to the other, and the span between them
is selected.

   C-Space   set the mark here (one end of the region)
   (move)    point becomes the other end as you move
   M-w       copy the region  (leaves the text in place, ready to yank)
   C-w       cut the region   (kills it; yank it back with C-y)

>> Put point at the start of this paragraph and press C-Space to set the mark.
   Press Down a few times to stretch the selection over several lines.  Press
   M-w to copy them, move to the end of the buffer, and C-y to paste the block.

>> Now the cutting version: mark a few lines with C-Space and Down, press C-w to
   cut them, then C-y to put them back.  C-w followed by C-y elsewhere is how
   you move a block of text from one place to another.


* LESSON 4.2: RUNNING ANY COMMAND BY NAME
-----------------------------------------
There are far more commands than there are keys.  To run one by name, press M-x,
type the command (a fuzzy list helps you find it), and press Return.  Everything
you have learned has a name reachable this way, plus much that has no key.

>> Press M-x and start typing apropos; choose it, press Return, then type a word
   like line to list commands whose names mention it.


* LESSON 4.3: GETTING HELP
--------------------------
Help starts with the C-h prefix:

   C-h k <key>   describe what a key does
   C-h b         list every key binding currently in effect
   C-h a         search command names for a word
   C-h t         start this tutorial again

>> Press C-h k and then C-n to confirm that C-n runs next-line.  Try C-h k on a
   few other keys you have learned.


* WHERE TO GO NEXT
------------------
You now know enough to edit comfortably.  From here, Macros has much more to
explore: LSP-powered code navigation, a magit-style git interface, project-wide
search, dired for files, org-mode, keyboard macros, themes, and a full Steel
(Scheme) extension language you can reshape the editor with.  Browse it all with
C-h a and M-x.

This is the end of the tutorial.  To close this buffer, type C-x k Return.  To
quit Macros, type C-x C-c.
")

;; help-with-tutorial (C-h t): open a fresh, editable copy of the tutorial and
;; put point at the top so the reader starts from the beginning. Rebuilding from
;; scratch each time gives a clean slate.
(define (help-with-tutorial)
  (kill-buffer-named "*TUTORIAL*")
  (append-to-buffer "*TUTORIAL*" *tutorial-text*)
  (goto-char 0)
  (message "Welcome to the Macros tutorial. Try the commands marked with >>."))

(define-key "fundamental" "ctrl-h t" "help-with-tutorial")
(register-command "help-with-tutorial")
