;; requires: core

;; tutorial.scm — interactive, hands-on tutorials (help-with-tutorial, C-h t).
;;
;; Two tutorials, one per editing style: an Emacs-keys tutorial and a Vim/evil
;; tutorial. Each opens a normal *editable* buffer: you read the text and practice
;; the commands right here, on this copy. Lines that begin with ">>" tell you a
;; command to try at that spot. Loaded after help.scm so the C-h prefix exists.
;;
;; C-h t opens the one matching your *current* keybindings (vim under evil-mode,
;; emacs otherwise) without changing the mode. The welcome screen's [emacs] and
;; [evil/vim] labels call help-with-emacs-tutorial / help-with-vim-tutorial, which
;; switch the editor into that mode first (evil-disable / evil-mode) so the keys
;; you practice are the ones actually in effect.
;;
;; The text is original to Macros. Every key each one teaches is a binding that
;; actually exists (see fundamental.scm / undo-tree.scm / evil.scm). The lessons
;; follow a vimtutor-style progression: move, then how to leave/enter insert, then
;; delete, undo, paste, navigate and search, files, and selecting text. The vim
;; tutorial also covers the Emacs chords that survive into evil mode (C-x, M-x …).
;;
;; Conventions used in the text: C-<chr> = hold Control + <chr>; M-<chr> = hold
;; Meta (Alt, or Option on a Mac) + <chr>, or press Esc, release, then <chr>.
(define *emacs-tutorial-text* "\
MACROS TUTORIAL (EMACS KEYS)

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

;; The vim/evil tutorial. Teaches modal editing (the keys defined in evil.scm)
;; plus the handful of Emacs chords that survive *into* evil mode: evil only
;; intercepts plain keys in Normal/Visual, so C-x, C-c, M-x, C-h and C-g still
;; reach their Emacs commands and you need them for files, buffers and quitting.
(define *vim-tutorial-text* "\
MACROS TUTORIAL (VIM / EVIL KEYS)

Welcome.  Evil mode is now ON, so Macros edits the Vim way: a modal editor.  You
are reading this inside an ordinary, editable buffer, so practice every command
right here, on this text.  Nothing is permanent: this copy is rebuilt fresh each
time you open it, so experiment freely.

Two keys are written in shorthand:

 C-x   hold Control and press x.
 M-x   hold Meta and press x.  Meta is Alt on most keyboards, or Option on a
       Mac.  No Meta key?  Press Esc, let go, then the next key.

Lines that begin with >> are things to try.  Do what they say before reading on.


* LESSON 0: MODES (THE BIG IDEA)
--------------------------------
Vim has separate modes.  In NORMAL mode the letter keys are commands (they move
and edit); in INSERT mode they type text.  You start in Normal mode.

   i        enter Insert mode (type text), then Esc to return to Normal
   Esc      leave Insert mode; also cancels a half-typed command

>> Press i, type a few words, then press Esc.  You are back in Normal mode, where
   h j k l move instead of typing.  When in doubt, press Esc.

The mode banner at the bottom shows '-- Insert --' while inserting; Normal mode
shows nothing (the resting state).


* LESSON 1: MOVING AROUND
-------------------------
In Normal mode the home-row keys move point (the cursor):

   k   up            j   down
   h   left          l   right

>> With Esc to be sure you are in Normal mode, steer around this paragraph with
   h j k l.  (The arrow keys work too, but h j k l keep your hands home.)

Jump by words and along the line:

   w   forward to next word     b   back a word     e   end of word
   W   forward a WORD           B   back a WORD     E   end of WORD
   0   start of line            ^   first non-blank text     $   end of line
   gg  top of the buffer        G   bottom of the buffer

A WORD (the shifted keys) is whitespace-delimited, so foo.bar counts as one;
the lowercase ones stop at punctuation too.

>> Try w and b across a sentence, then 0 and $ to snap to the ends of a line.
   Finish with gg and G to leap to the top and bottom (then come back here).

COUNTS.  Type a number before a command and it repeats that many times:

   3j   down three lines        2w   forward two words
   5x   delete five characters  3dd  delete three lines

>> Press 3j to drop three lines, then 3k to come back.


* LESSON 2: ENTERING INSERT MODE
--------------------------------
Several keys enter Insert mode, each at a different spot:

   i   insert before the cursor      a   append after the cursor
   I   insert at first non-blank     A   append at end of line
   o   open a new line below         O   open a new line above

>> Put point in the middle of this line and press A; the cursor jumps to the end,
   ready to type.  Press Esc.  Now press o to open a fresh line below and type on
   it, then Esc.


* LESSON 3: DELETING AND CHANGING
---------------------------------
Operators act in Normal mode.  The common ones:

   x    delete the character under the cursor
   dd   delete (cut) the whole line          dw   delete to next word
   d$ / D   delete to end of line
   cc   change the whole line (delete + Insert)   cw   change a word
   r    replace one character (r then the new char)
   ~    toggle the case of the character        J    join the next line up

>> Put point on the next line and press dd to delete it:
This is a throwaway practice line.  Delete it with dd and watch the text close up.

>> Put point on a word and press cw, type a replacement, and Esc.

What you delete is not lost — it is saved, ready to put back (next lesson).


Shift lines left and right with the indent operators:

   >>  indent the current line       <<  outdent it
   (in Visual mode, > and < shift every selected line)

>> Press >> on this line to indent it, then << to put it back.


* LESSON 4: YANK, PUT, AND UNDO
-------------------------------
'Yank' means copy; 'put' means paste.

   yy   yank (copy) the current line     yw   yank a word
   p    put after the cursor / below      P    put before / above
   u    undo the last change              C-?  redo (re-apply an undo)

>> Yank this line with yy, then press p to drop a copy on the line below.
>> Make a mess with dd or x, then press u a few times to undo it.

(Undo is u, as in Vim.  Redo is C-? — a Control chord, which works in every mode;
more on those Emacs chords in Lesson 7.)


* LESSON 5: VISUAL MODE (SELECTING)
-----------------------------------
Visual mode selects a range, then an operator acts on it.

   v    start a character-wise selection   V    select whole lines
   (move with h j k l, w, $, …)            the span highlights as you go
   d    delete the selection   y  yank it   c  change it

>> Press v, stretch the selection with l or w, then press d to cut it (u to undo).
>> Press V to select this whole line, press j to add the next, then y to copy.


* LESSON 6: SEARCH AND EX COMMANDS
----------------------------------
   /text   search forward for 'text' (Return to jump)    ?text  search backward
   n       next match            N       previous match
   :       start an Ex command, e.g.  :%s/old/new/g  to replace across the buffer

>> Press / then type the word region and Return; press n to find the next one.


* LESSON 7: THE EMACS KEYS YOU STILL NEED
-----------------------------------------
Evil only takes over the plain letter keys.  Control- and Meta-chords still run
their Emacs commands in every mode, and you genuinely need a few for files,
buffers, help, and quitting.  These are not optional extras — there is no Vim
':w' wired up; saving is C-x C-s.

   C-g        cancel a half-typed command (Vim's Esc cancels modes; C-g cancels
              a pending Emacs chord) — your get-out-of-trouble key
   C-x C-s    save the current buffer to its file        (Vim :w)
   C-x C-f    open (visit) a file by name                (Vim :e)
   C-x b      switch to another buffer       C-x C-b   list all buffers
   C-x k      kill (close) a buffer          C-x C-c   quit Macros   (Vim :q)
   M-x        run any command by name (a fuzzy list helps you find it)
   C-h k <key>   describe what a key does    C-h t   reopen this tutorial

>> Press C-x C-f to begin opening a file, then C-g to cancel.  Press M-x, type
   apropos, Return, then a word like line to list matching commands.

(Prefer Vim-style file commands?  Evil's Ex line has them too: try :w, :e, :q
after pressing : — but the Emacs chords above always work.)


* WHERE TO GO NEXT
------------------
You now know enough to edit the Vim way.  From here, Macros has much more: LSP
code navigation, a magit-style git interface, project-wide search, dired, org-
mode, keyboard macros, themes, and a full Steel (Scheme) extension language.
Evil keymaps are all Scheme too — rebind anything in ~/.config/macros/init.scm.
Add a leader key (Doom/Spacemacs style) with evil-set-leader and evil-leader-set.

This is the end of the tutorial.  To close this buffer, type C-x k Return.  To
quit Macros, type C-x C-c.
")

(define *helix-tutorial-text* "\
MACROS TUTORIAL (HELIX KEYS)

Welcome.  This is a hands-on tour of Macros' Helix-style editing.  You are
reading it inside an ordinary, editable buffer, so you can practice every
command right here, on this text.  Nothing you do is permanent: this copy is
rebuilt fresh each time you open it, so experiment freely.

Helix has two main modes:

 NORMAL   the resting mode; letters run commands.  This is where you start.
 INSERT   what you get after i, a, c, o; letters type themselves.  Esc returns.

The big idea — SELECTION FIRST.  In Helix you SELECT, then ACT.  A motion does
not just move the cursor, it selects: w selects the next word, x selects the
line.  Then a verb acts on what is selected: d deletes the selection, c changes
it, y copies it.  Noun first, then verb.  You always see what you are about to
act on, highlighted, before you act.

Two keys are written in shorthand below:

 C-x   hold Control and press x.
 M-x   hold Meta and press x.  Meta is Alt on most keyboards, or Option on a
       Mac.  No Meta key?  Press Esc, let go, then the next key.

Lines that begin with >> are things to try.  Do what they say before reading on.


* LESSON 0: THE EMACS KEYS YOU NEED
-----------------------------------
Helix mode handles editing, but files, buffers, and quitting still go through
Macros' Emacs chords — and they work in EVERY mode, so learn these first.  You
genuinely need them.

   C-g        cancel a half-typed command — your get-out-of-trouble key
   C-x C-s    save the current buffer to its file
   C-x C-f    open (visit) a file by name
   C-x b      switch to another buffer       C-x C-b   list all buffers
   C-x k      kill (close) a buffer          C-x C-c   quit Macros
   M-x        run any command by name (a fuzzy list helps you find it)

>> Press C-x C-f to begin opening a file, then C-g to cancel.  Press M-x, type
   part of a command name to see the list, then C-g to back out.

(Prefer Vim-style file commands?  Helix's Ex line has them too: press : then try
:w, :e file, :q — but the Emacs chords above always work.)


* LESSON 1: MOVING (AND SELECTING AS YOU GO)
--------------------------------------------
The home-row keys move point:

   h  left      l  right      j  down      k  up

>> Steer around this paragraph with h j k l until it feels natural.

Word motions SELECT the word they cross — watch the highlight:

   w  select forward to the next word     b  select back a word
   e  select to the end of the word

>> Put point on this line and press w a few times.  Each press highlights the
   next word.  Now press b to walk the selection back.

   0  go to the start of the line     $  go to the end of the line
   g g  go to the top of the file     g e  go to the very bottom

>> Jump to the top with g g, then back down here with j, or g e for the end.


* LESSON 2: SELECTING LINES, AND THE PRIMARY VERBS
--------------------------------------------------
   x   select the whole current line (press again to add the next line)

>> Put point on this line and press x.  The whole line lights up.  Press x once
   more to extend the selection down to the following line.

Now the verbs.  Each acts on the current selection:

   d   delete the selection (and copy it, so p can put it back)
   c   change: delete the selection and drop you into INSERT to retype it
   y   yank (copy) the selection         p   paste after

>> Type a junk word here: FIXME.  Put point on it, press w to select it, then d
   to delete it.  Press u to undo if you like.

>> Select this whole line with x, press y to copy it, then p to paste a copy
   below.  Delete the extra line with x then d.


* LESSON 3: INSERTING TEXT
--------------------------
From Normal mode, enter Insert with:

   i   insert before the selection      a   append after it
   o   open a new line below            O   open a new line above

Type your text, then press Esc to return to Normal.

>> Press o to open a line below this one, type a sentence, and press Esc.
>> Press u to undo it if you want the tutorial tidy again.


* LESSON 4: MULTIPLE SELECTIONS (THE HELIX SUPERPOWER)
------------------------------------------------------
This is what makes Helix Helix.  You can have MANY selections at once, and every
motion and verb acts on ALL of them together.

   C   add a cursor on the line below (repeat to add more)
   ,   keep only the primary selection (drop the extras)
   ;   collapse every selection back to a single cursor
   v   toggle SELECT mode: motions now EXTEND the selection instead of replacing

>> Put point on the first line below, then press C twice.  Each press drops a
   cursor one line further down, so you now have three cursors:

     change me
     change me
     change me

>> With the three cursors active, press w to select 'change' on every line at
   once, then press c, type 'fix', and press Esc.  All three change together.
   Press , to drop back to one selection when you are done.

>> Now try v: press v (SELECT mode on), then l l l to grow one selection a
   character at a time, then d to delete it.  Press v again to turn it off.


* LESSON 5: SEARCH AND UNDO
---------------------------
   /text Return   search for text        n  next match     N  previous match
   u  undo        U  redo

(For saving, opening files, and quitting, use the Emacs chords from Lesson 0 —
C-x C-s, C-x C-f, C-x C-c — they work here too.)

>> Search for the word 'Helix' with /Helix Return, then press n to jump to the
   next one.


* WHERE TO GO NEXT
------------------
You now know enough to edit the Helix way: select, then act, on one cursor or
many.  From here Macros has much more: LSP code navigation, a magit-style git
interface, project-wide search, dired, org-mode, themes, and a full Steel
(Scheme) extension language.  The Helix keymaps are all Scheme too — rebind
anything in ~/.config/macros/init.scm with (define-key \"helix-normal\" ...).

This is the end of the tutorial.  To close this buffer, type C-x k Return.  To
quit Macros, type C-x C-c.
")

;; Open a fresh, editable copy of `text` in buffer `name`, point at the top, with
;; an echo-area hint. Rebuilding from scratch each time gives a clean slate.
(define (tutorial-open name text hint)
  (kill-buffer-named name)
  (append-to-buffer name text)
  (goto-char 0)
  (message hint))

;; The Emacs tutorial. Entering it switches OFF evil-mode so the keys it teaches
;; (C-n, C-k, C-Space …) are the ones actually in effect while you practice.
(define (help-with-emacs-tutorial)
  (evil-disable)
  (tutorial-open "*TUTORIAL*" *emacs-tutorial-text*
                 "Welcome to the Macros tutorial. Try the commands marked with >>."))

;; The vim/evil tutorial. Entering it switches ON evil-mode so the modal keys it
;; teaches are live while you practice.
(define (help-with-vim-tutorial)
  (evil-mode)
  (tutorial-open "*VIM-TUTORIAL*" *vim-tutorial-text*
                 "Welcome to the Vim tutorial. Evil mode is on. Try the >> commands."))

;; The Helix tutorial. Entering it switches ON Helix mode so the selection-first
;; keys it teaches are live while you practice.
(define (help-with-helix-tutorial)
  (helix-mode)
  (tutorial-open "*HELIX-TUTORIAL*" *helix-tutorial-text*
                 "Welcome to the Helix tutorial. Helix mode is on. Try the >> commands."))

;; help-with-tutorial (C-h t): open the tutorial that matches your *current*
;; keybindings — Helix under helix-mode, vim under evil-mode, emacs otherwise. It
;; does NOT change the mode (the welcome-screen labels do that). Helix is checked
;; first because evil-enabled? is also true under Helix (shared state machine).
(define (help-with-tutorial)
  (cond ((helix-enabled?)
         (tutorial-open "*HELIX-TUTORIAL*" *helix-tutorial-text*
                        "Welcome to the Helix tutorial. Try the >> commands."))
        ((evil-enabled?)
         (tutorial-open "*VIM-TUTORIAL*" *vim-tutorial-text*
                        "Welcome to the Vim tutorial. Try the >> commands."))
        (else
         (tutorial-open "*TUTORIAL*" *emacs-tutorial-text*
                        "Welcome to the Macros tutorial. Try the commands marked with >>."))))

(define-key "fundamental" "ctrl-h t" 'help-with-tutorial)
(register-command 'help-with-tutorial)
(register-command 'help-with-emacs-tutorial)
(register-command 'help-with-vim-tutorial)
(register-command 'help-with-helix-tutorial)
