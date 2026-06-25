;; org-roam.scm — a Zettelkasten over your .org files, in the org-roam style,
;; built entirely in Scheme on the editor primitives. No SQLite: the node index
;; is scanned from disk (ripgrep-fast `walk-files`) and cached in memory; a
;; `(sql-query …)` bridge could replace `*org-roam-nodes*` later without touching
;; the commands.
;;
;; A "node" is an .org file under the roam directory with a top `:ID:` property
;; and a `#+title:`. Links between nodes use `[[id:UUID][desc]]`. Commands:
;;   C-c n f  find/visit a node (creates it if the title is new)
;;   C-c n i  insert an [[id:…]] link to a node (creates if new)
;;   C-c n c  capture a new node
;;   C-c n l  show backlinks to the node at point
;;   C-c n s  re-scan the roam directory
;;   C-c C-o  open the link at point (id: / file: / http)
;;
;; This file is self-contained (core.scm + primitives + the `org-id-new` /
;; `current-time` primitives), so it loads independently of org.scm.

;; ===========================================================================
;; State (defined first so the config setters below can touch them)
;; ===========================================================================
;; The node index: each entry is (id title file (tag …)).
(define *org-roam-nodes* '())
;; Whether the index has been scanned this session.
(define *org-roam-synced* #f)

;; ===========================================================================
;; Configuration + small helpers
;; ===========================================================================
;; The directory scanned for nodes. Empty = ~/org-roam. Set from init.scm with
;; (org-roam-set-directory "~/notes").
(define *org-roam-directory* "")
(define (org-roam-set-directory d)
  (set! *org-roam-directory* d)
  (set! *org-roam-synced* #f))

;; Command used to open external (http / file) links. macOS "open"; set to
;; "xdg-open" on Linux from init.scm via (set! *org-open-command* "xdg-open").
(define *org-open-command* "open")

;; Inline display width (px) of the graph image, and the graphviz render DPI.
;; Raise the width to see a large graph bigger (it's capped to its natural size,
;; so the DPI controls how much detail there is to show). Set from init.scm:
;;   (set! *org-roam-graph-width* 1600)
;;   (set! *org-roam-graph-dpi* 200)
(define *org-roam-graph-width* 1200)
(define *org-roam-graph-dpi* 150)

(define (org-roam--trim s)
  (replace-regexp-in-string "^[ \t]+" "" (replace-regexp-in-string "[ \t]+$" "" s)))

(define (org-roam--home-expand d)
  (if (>= (string-match "^~/" d) 0)
      (path-join (getenv "HOME") (substring d 2 (string-length d)))
      d))

(define (org-roam-directory)
  (org-roam--home-expand
   (if (equal? *org-roam-directory* "")
       (path-join (getenv "HOME") "org-roam")
       *org-roam-directory*)))

;; Resolve a (possibly relative) link target against the active buffer's dir.
(define (org-roam--resolve p)
  (if (>= (string-match "^[/~]" p) 0) p (path-join (buffer-directory) p)))

;; A filesystem-safe slug from a title: lowercased, non-alphanumerics collapsed
;; to single dashes, trimmed.
(define (org-roam--slug title)
  (let* ((s (string-downcase title))
         (s (replace-regexp-in-string "[^a-z0-9]+" "-" s))
         (s (replace-regexp-in-string "^-+" "" s))
         (s (replace-regexp-in-string "-+$" "" s)))
    (if (equal? s "") "untitled" s)))

;; ":a:b:" filetags string → ("a" "b").
(define (org-roam--split-tags s)
  (filter (lambda (x) (not (equal? x ""))) (split-many (org-roam--trim s) ":")))

;; ===========================================================================
;; The node index, scanned and cached
;; ===========================================================================
;; Each node is a list: (id title file (tag …) line). `line` is the 0-based line
;; to jump to — 0 for a file-level node, the headline's line for a subtree node.
(define (org-roam--node-line n) (if (> (length n) 4) (nth n 4) 0))

;; Lines before the first headline (the file's "front matter").
(define (org-roam--front-matter lines)
  (let loop ((ls lines) (acc '()))
    (cond ((null? ls) (reverse acc))
          ((>= (string-match "^\\*+[ \t]" (car ls)) 0) (reverse acc))
          (else (loop (cdr ls) (cons (car ls) acc))))))

;; A file-level node (line 0) if the front matter has an :ID: and there's a
;; #+title:, else #f.
(define (org-roam--file-level-node text lines path)
  (let ((idc (regexp-captures "(?im)^[ \t]*:ID:[ \t]+(\\S+)" (join-lines (org-roam--front-matter lines))))
        (ttc (regexp-captures "(?im)^#\\+title:[ \t]*(.*)$" text)))
    (if (or (null? idc) (null? ttc))
        #f
        (list (nth idc 1) (org-roam--trim (nth ttc 1)) path
              (let ((tc (regexp-captures "(?im)^#\\+filetags:[ \t]*(.*)$" text)))
                (if (null? tc) '() (org-roam--split-tags (nth tc 1))))
              0))))

;; The cleaned title of a headline (stars + trailing tags removed), or #f.
(define (org-roam--heading-text line)
  (let ((c (regexp-captures "^\\*+[ \t]+(.*)$" line)))
    (if (null? c)
        #f
        (org-roam--trim
         (replace-regexp-in-string "[ \t]*:[A-Za-z0-9_@#%:]+:[ \t]*$" "" (nth c 1))))))

;; The :ID: of a :PROPERTIES: drawer in the lines following a headline (scanning
;; up to the drawer's :END: or the next headline), or #f.
(define (org-roam--drawer-id lines-after)
  (let loop ((ls lines-after) (k 0) (in-drawer #f))
    (cond ((or (null? ls) (> k 12)) #f)
          ((>= (string-match "^[ \t]*:PROPERTIES:[ \t]*$" (car ls)) 0) (loop (cdr ls) (+ k 1) #t))
          ((>= (string-match "^[ \t]*:END:[ \t]*$" (car ls)) 0) #f)
          ((>= (string-match "^\\*+[ \t]" (car ls)) 0) #f) ; next headline
          (in-drawer
           (let ((c (regexp-captures "^[ \t]*:ID:[ \t]+(\\S+)" (car ls))))
             (if (null? c) (loop (cdr ls) (+ k 1) #t) (nth c 1))))
          ((equal? (org-roam--trim (car ls)) "") (loop (cdr ls) (+ k 1) in-drawer))
          (else #f)))) ; non-blank, non-drawer content before a drawer → none

;; All nodes in one file: the file-level node (if any) plus every headline that
;; carries an :ID: property (subtree nodes).
(define (org-roam--scan-file path)
  (let* ((text (read-file path))
         (lines (split-many text "\n"))
         (fl (org-roam--file-level-node text lines path)))
    (let loop ((ls lines) (i 0) (acc (if fl (list fl) '())))
      (if (null? ls)
          (reverse acc)
          (let ((ht (org-roam--heading-text (car ls))))
            (if ht
                (let ((id (org-roam--drawer-id (cdr ls))))
                  (loop (cdr ls) (+ i 1)
                        (if id (cons (list id ht path '() i) acc) acc)))
                (loop (cdr ls) (+ i 1) acc)))))))

;; C-c n s — (re)scan the roam directory into the index.
(define (org-roam-db-sync)
  (set! *org-roam-nodes* '())
  (let ((dir (org-roam-directory)))
    (when (file-exists? dir)
      (for-each (lambda (f)
                  (when (ends-with? f ".org")
                    (for-each (lambda (n) (set! *org-roam-nodes* (cons n *org-roam-nodes*)))
                              (org-roam--scan-file f))))
                (walk-files dir))))
  (set! *org-roam-synced* #t)
  (message (string-append "org-roam: indexed " (int->string (length *org-roam-nodes*)) " node(s)")))

(define (org-roam--ensure-index)
  (when (not *org-roam-synced*) (org-roam-db-sync)))

(define (org-roam--titles) (map (lambda (n) (nth n 1)) *org-roam-nodes*))
(define (org-roam--node-by-id id)
  (let loop ((ns *org-roam-nodes*))
    (cond ((null? ns) #f) ((equal? (car (car ns)) id) (car ns)) (else (loop (cdr ns))))))
(define (org-roam--node-by-title t)
  (let loop ((ns *org-roam-nodes*))
    (cond ((null? ns) #f) ((equal? (nth (car ns) 1) t) (car ns)) (else (loop (cdr ns))))))
(define (org-roam--node-by-file f)
  (let loop ((ns *org-roam-nodes*))
    (cond ((null? ns) #f) ((equal? (nth (car ns) 2) f) (car ns)) (else (loop (cdr ns))))))

;; ===========================================================================
;; Creating nodes
;; ===========================================================================
;; Write a new node file (`<timestamp>-<slug>.org`) with an :ID: and #+title:,
;; add it to the index, and return the node record.
(define (org-roam--create-node title)
  (org-roam--ensure-index)
  (let* ((dir (org-roam-directory))
         (id (org-id-new))
         (file (path-join dir (string-append (current-time "%Y%m%d%H%M%S")
                                             "-" (org-roam--slug title) ".org")))
         (node (list id title file '() 0)))
    (call-process "mkdir" (list "-p" dir))
    (write-file file
      (string-append ":PROPERTIES:\n:ID:       " id "\n:END:\n#+title: " title "\n\n"))
    (set! *org-roam-nodes* (cons node *org-roam-nodes*))
    node))

;; ===========================================================================
;; Find / insert / capture
;; ===========================================================================
;; C-c n f — pick a node by title (fuzzy) and visit it; a new title creates one.
(define (org-roam-node-find)
  (org-roam--ensure-index)
  (completing-read "Node: " (org-roam--titles) "org-roam--find-finish"))
(define (org-roam--find-finish title)
  (when (not (equal? (org-roam--trim title) ""))
    (let ((n (or (org-roam--node-by-title title)
                 (org-roam--create-node (org-roam--trim title)))))
      (open-file (nth n 2)))))

;; C-c n i — insert an [[id:…][title]] link to a node (creating it if new).
(define (org-roam-node-insert)
  (org-roam--ensure-index)
  (completing-read "Insert node: " (org-roam--titles) "org-roam--insert-finish"))
(define (org-roam--insert-finish title)
  (when (not (equal? (org-roam--trim title) ""))
    (let ((n (or (org-roam--node-by-title title)
                 (org-roam--create-node (org-roam--trim title)))))
      (insert (string-append "[[id:" (car n) "][" (nth n 1) "]]")))))

;; C-c n c — capture a brand-new node and open it.
(define (org-roam-capture)
  (read-string "Title: " "org-roam--capture-finish"))
(define (org-roam--capture-finish title)
  (when (not (equal? (org-roam--trim title) ""))
    (open-file (nth (org-roam--create-node (org-roam--trim title)) 2))))

;; ===========================================================================
;; Following links (id: / file: / http)
;; ===========================================================================
(define (org-roam--current-line)
  (let ((ls (split-many (buffer-string) "\n")) (i (buffer-line)))
    (if (< i (length ls)) (nth ls i) "")))

;; (prefix . target) for the link on the current line, or #f. `prefix` is one of
;; "id:", "file:", "http://", "https://", or "".
(define (org-roam--link-at-point)
  (let ((c (regexp-captures "\\[\\[(id:|file:|https?://)?([^]]+?)\\](\\[[^]]*\\])?\\]"
                            (org-roam--current-line))))
    (if (null? c) #f (cons (nth c 1) (nth c 2)))))

(define (org-roam--visit-id id)
  (org-roam--ensure-index)
  (let ((n (org-roam--node-by-id id)))
    (if (not n)
        (message (string-append "org-roam: no node " id))
        (begin (open-file (nth n 2))
               (when (> (org-roam--node-line n) 0) (goto-line (org-roam--node-line n)))))))

;; C-c C-o — open the link at point.
(define (org-open-at-point)
  (let ((lk (org-roam--link-at-point)))
    (if (not lk)
        (message "No link at point")
        (let ((prefix (car lk)) (target (cdr lk)))
          (cond
            ((equal? prefix "id:") (org-roam--visit-id target))
            ((and (not (equal? prefix "")) (>= (string-match "^https?://" prefix) 0))
             (call-process *org-open-command* (list (string-append prefix target))))
            ((>= (string-match "^https?://" target) 0)
             (call-process *org-open-command* (list target)))
            ((equal? prefix "file:") (open-file (org-roam--resolve target)))
            (else (open-file (org-roam--resolve target))))))))

;; ===========================================================================
;; Backlinks ("what links here")
;; ===========================================================================
;; The :ID: of the node in the active buffer, or #f.
(define (org-roam--current-id)
  (let ((c (regexp-captures "(?im)^[ \t]*:ID:[ \t]+(\\S+)" (buffer-string))))
    (if (null? c) #f (nth c 1))))

;; "path:line:text" → (path line-number text), or #f. Path is non-greedy up to
;; the first ":<digits>:" so it tolerates colons elsewhere.
(define (org-roam--parse-hit h)
  (let ((c (regexp-captures "^(.+?):([0-9]+):(.*)$" h)))
    (if (null? c) #f (list (nth c 1) (string->number (nth c 2)) (nth c 3)))))

;; agenda-buffer line -> (source-file . 0-based-line), for RET.
(define *org-roam-backlink-targets* '())

(define (org-roam--render-backlinks id title)
  (set! *org-roam-backlink-targets* '())
  (let ((hits (grep-files (org-roam-directory) (string-append "\\[\\[id:" id))))
    (let loop ((hs hits) (i 2) (out '()))
      (if (null? hs)
          (render-buffer "*org-roam-backlinks*" "org-roam-backlinks-mode"
            (join-lines
             (append (list (string-append "Backlinks to: " title
                                          "  (" (int->string (length hits)) ")")
                           "")
                     (reverse out)))
            '())
          (let ((p (org-roam--parse-hit (car hs))))
            (if (not p)
                (loop (cdr hs) i out)
                (let* ((path (nth p 0)) (lnum (nth p 1)) (txt (nth p 2))
                       (src (org-roam--node-by-file path))
                       (label (if src (nth src 1) (basename path))))
                  (set! *org-roam-backlink-targets*
                        (cons (cons i (cons path (- lnum 1))) *org-roam-backlink-targets*))
                  (loop (cdr hs) (+ i 1)
                        (cons (string-append label "  :  " (org-roam--trim txt)) out)))))))))

;; C-c n l — show backlinks to the node at point in a side buffer.
(define (org-roam-buffer-toggle)
  (org-roam--ensure-index)
  (let ((id (org-roam--current-id)))
    (if (not id)
        (message "Not in an org-roam node (no :ID:)")
        (let ((n (org-roam--node-by-id id)))
          (org-roam--render-backlinks id (if n (nth n 1) (basename (current-buffer))))))))

(define (org-roam-backlinks-visit)
  (let ((t (alist-ref *org-roam-backlink-targets* (buffer-line))))
    (when t (open-file (car t)) (goto-line (cdr t)))))

;; ===========================================================================
;; Graph view (graphviz → inline image)
;; ===========================================================================
;; Emit a DOT graph of the node network and render it with `dot -Tpng`, shown
;; inline via the Org image pipeline. Needs graphviz (`dot`) on PATH.
(define (org-roam--concat lst)
  (if (null? lst) "" (string-append (car lst) (org-roam--concat (cdr lst)))))

(define (org-roam--char-bytes ch)
  (let ((cp (char->integer ch)))
    (cond ((< cp 128) 1) ((< cp 2048) 2) ((< cp 65536) 3) (else 4))))
(define (org-roam--byte-drop s n)
  (if (<= n 0)
      s
      (let loop ((chars (string->list s)) (acc 0))
        (cond ((null? chars) "")
              ((>= acc n) (list->string chars))
              (else (loop (cdr chars) (+ acc (org-roam--char-bytes (car chars)))))))))

;; Every node ID referenced by `[[id:…]]` links in `text`, in order.
(define (org-roam--link-ids text)
  (let loop ((s text) (acc '()))
    (let ((m (string-match "\\[\\[id:([^]\n]+)" s)))
      (if (< m 0)
          (reverse acc)
          (let* ((c (regexp-captures "\\[\\[id:([^]\n]+)" s))
                 (id (org-roam--trim (nth c 1)))
                 (consumed (+ m (utf8-length (car c)))))
            (loop (org-roam--byte-drop s consumed) (cons id acc)))))))

;; (source-id . target-id) edges between known nodes (self-loops dropped).
(define (org-roam--edges)
  (let loop ((ns *org-roam-nodes*) (acc '()))
    (if (null? ns)
        acc
        (let* ((n (car ns)) (sid (car n)))
          (loop (cdr ns)
                (append acc
                  (filter (lambda (e)
                            (and (not (equal? (car e) (cdr e))) (org-roam--node-by-id (cdr e))))
                          (map (lambda (t) (cons sid t))
                               (org-roam--link-ids (read-file (nth n 2)))))))))))

;; Make a title safe inside a DOT "label": quotes → apostrophes, backslashes and
;; newlines → spaces. (Sanitising avoids DOT's escaping rules entirely.)
(define (org-roam--dot-escape s)
  (replace-regexp-in-string "\"" "'"
    (replace-regexp-in-string "\\\\" " "
      (replace-regexp-in-string "\n" " " s))))

(define (org-roam--dot)
  (string-append
   "digraph roam {\n  rankdir=LR;\n  bgcolor=\"transparent\";\n"
   "  node [shape=box, style=\"rounded,filled\", fillcolor=\"#3c3836\", fontcolor=\"#ebdbb2\", color=\"#83a598\", fontname=\"sans\"];\n"
   "  edge [color=\"#928374\"];\n"
   (org-roam--concat
    (map (lambda (n)
           (string-append "  \"" (car n) "\" [label=\"" (org-roam--dot-escape (nth n 1)) "\"];\n"))
         *org-roam-nodes*))
   (org-roam--concat
    (map (lambda (e) (string-append "  \"" (car e) "\" -> \"" (cdr e) "\";\n"))
         (org-roam--edges)))
   "}\n"))

(define (org-roam--graph-tmpdir)
  (let ((t (getenv "TMPDIR"))) (path-join (if (equal? t "") "/tmp" t) "macros-org-roam")))

;; C-c n g — render the node graph and show it inline.
(define (org-roam-graph)
  (org-roam--ensure-index)
  (if (null? *org-roam-nodes*)
      (message "org-roam: no nodes to graph (try C-c n s)")
      (let* ((dir (org-roam--graph-tmpdir))
             (dotf (path-join dir "graph.dot"))
             (png (path-join dir "graph.png")))
        (call-process "mkdir" (list "-p" dir))
        (write-file dotf (org-roam--dot))
        (call-process-full "dot"
          (list "-Tpng" (string-append "-Gdpi=" (int->string *org-roam-graph-dpi*))
                "-o" png dotf) "")
        (if (not (file-exists? png))
            (message "org-roam: graphviz `dot` failed or not installed")
            (begin
              (render-buffer "*org-roam-graph*" "org-roam-graph-mode"
                (string-append "org-roam graph — " (int->string (length *org-roam-nodes*))
                               " nodes\n" png "\n")
                '())
              (clear-line-images)
              (set-line-image 1 png *org-roam-graph-width*)
              (message "org-roam graph rendered"))))))

;; ===========================================================================
;; Keymap + M-x
;; ===========================================================================
;; The C-c n prefix, available globally and in org buffers.
(define-key "fundamental" "ctrl-c n f" "org-roam-node-find")
(define-key "fundamental" "ctrl-c n c" "org-roam-capture")
(define-key "fundamental" "ctrl-c n s" "org-roam-db-sync")
(define-key "fundamental" "ctrl-c n g" "org-roam-graph")
(define-key "org-mode" "ctrl-c n f" "org-roam-node-find")
(define-key "org-mode" "ctrl-c n i" "org-roam-node-insert")
(define-key "org-mode" "ctrl-c n c" "org-roam-capture")
(define-key "org-mode" "ctrl-c n l" "org-roam-buffer-toggle")
(define-key "org-mode" "ctrl-c n s" "org-roam-db-sync")
(define-key "org-mode" "ctrl-c n g" "org-roam-graph")
(define-key "org-mode" "ctrl-c ctrl-o" "org-open-at-point")

;; *org-roam-backlinks* buffer.
(define-key "org-roam-backlinks-mode" "enter" "org-roam-backlinks-visit")
(define-key "org-roam-backlinks-mode" "g" "org-roam-buffer-toggle")
(define-key "org-roam-backlinks-mode" "n" "next-line")
(define-key "org-roam-backlinks-mode" "p" "previous-line")
(define-key "org-roam-backlinks-mode" "q" "kill-buffer")
(set-option "line-numbers:org-roam-backlinks-mode" "false")

;; *org-roam-graph* buffer.
(define-key "org-roam-graph-mode" "g" "org-roam-graph")
(define-key "org-roam-graph-mode" "q" "kill-buffer")
(set-option "line-numbers:org-roam-graph-mode" "false")

(register-command "org-roam-node-find")
(register-command "org-roam-node-insert")
(register-command "org-roam-capture")
(register-command "org-roam-buffer-toggle")
(register-command "org-roam-db-sync")
(register-command "org-roam-graph")
(register-command "org-roam-set-directory")
(register-command "org-open-at-point")
