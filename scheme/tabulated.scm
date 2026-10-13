;; tabulated-list — a columnar, sortable list UI, the Macros port of Emacs
;; `tabulated-list-mode` (the engine behind `list-packages`, `proced`, the buffer
;; menu, …). It renders rows of aligned columns into a read-only buffer with a
;; header line, lets you sort by a column, and dispatches a per-row action on RET.
;; Built purely on the special-buffer primitive (`render-buffer`) plus the core
;; list helpers — no new Rust.
;;
;; Usage:
;;   (tabulated-render
;;     "*packages*" "package-menu-mode"
;;     ;; columns: (title width) — width is a char count; #f = flexible (last col)
;;     (list (list "Name" 20) (list "Version" 10) (list "Status" #f))
;;     ;; rows: (id (cell …)) — id is any value, passed to the action on RET
;;     (list (list 'magit  (list "magit"  "3.3.0" "installed"))
;;           (list 'consult (list "consult" "1.0"  "available")))
;;     ;; action: a procedure called with the row id when RET is pressed
;;     (lambda (id) (message (string-append "chose " (symbol->string id)))))
;;   (tabulated-setup-keys "package-menu-mode")   ; RET / S / n / p / q
;;
;; A re-render with the same buffer name replaces it (the refresh pattern).

(set-face "tabulated-header" "#66d9ef")

;; --- per-buffer state -------------------------------------------------------
;; buffer-name -> (mode columns orig-rows action sort-idx sort-desc display-rows)
(define *tabulated-tables* '())

(define (tabulated-set! buffer state)
  (set! *tabulated-tables*
        (alist-set *tabulated-tables* buffer state)))

;; alist-ref returns the stored state (the cdr) or #f.
(define (tabulated-get buffer) (alist-ref *tabulated-tables* buffer))

(define (tab-mode s)         (nth s 0))
(define (tab-columns s)      (nth s 1))
(define (tab-rows s)         (nth s 2))
(define (tab-action s)       (nth s 3))
(define (tab-sort-idx s)     (nth s 4))
(define (tab-sort-desc s)    (nth s 5))
(define (tab-display-rows s) (nth s 6))

(define (row-id r)    (nth r 0))
(define (row-cells r) (nth r 1))

;; --- column formatting ------------------------------------------------------
;; Pad/truncate `s` to `width` display columns; #f width leaves it untouched
;; (the last, flexible column).
(define (tabulated-pad s width)
  (cond ((not width) s)
        ((> (string-length s) width) (substring s 0 width))
        (else (pad-right s width))))

;; Join one row's cells, each padded to its column width, with a single-space
;; gutter between columns.
(define (tabulated-format cells widths)
  (let loop ((cells cells) (widths widths) (acc '()))
    (cond
      ((null? cells) (string-join (reverse acc) " "))
      (else (loop (cdr cells)
                  (if (null? widths) '() (cdr widths))
                  (cons (tabulated-pad (car cells)
                                       (if (null? widths) #f (car widths)))
                        acc))))))

;; --- sorting ----------------------------------------------------------------
;; Rows sorted by the `idx`th cell (string compare); reversed when `desc`.
;; (Keyed sorting itself is core's `sort-by`.)
(define (tabulated-sort-rows rows idx desc)
  (let ((sorted (sort-by (lambda (r) (nth (row-cells r) idx)) string<? rows)))
    (if desc (reverse sorted) sorted)))

;; --- rendering --------------------------------------------------------------
;; Draw the table currently stored for `buffer` (header line + sorted rows),
;; caching the display order so RET can map a line back to its row id.
(define (tabulated-draw buffer)
  (let* ((st (tabulated-get buffer))
         (mode (tab-mode st))
         (columns (tab-columns st))
         (widths (map (lambda (c) (nth c 1)) columns))
         (titles (map (lambda (c) (nth c 0)) columns))
         (idx (tab-sort-idx st))
         (display (if (>= idx 0)
                      (tabulated-sort-rows (tab-rows st) idx (tab-sort-desc st))
                      (tab-rows st)))
         (header (tabulated-format titles widths))
         (body (map (lambda (r) (tabulated-format (row-cells r) widths)) display)))
    (tabulated-set! buffer
                    (list mode columns (tab-rows st) (tab-action st)
                          idx (tab-sort-desc st) display))
    (render-buffer buffer mode
                   (join-lines (cons header body))
                   (list (list 0 "tabulated-header")))))

;; Public entry: store a fresh table and render it.
(define (tabulated-render buffer mode columns rows action)
  (tabulated-set! buffer (list mode columns rows action -1 #f '()))
  (tabulated-draw buffer))

;; --- navigation / interaction -----------------------------------------------
;; The display row under point (line 0 is the header), or #f.
(define (tabulated-row-at-point)
  (let* ((buffer (current-buffer))
         (st (tabulated-get buffer))
         (line (buffer-line)))
    (if (and st (>= line 1) (<= line (length (tab-display-rows st))))
        (nth (tab-display-rows st) (- line 1))
        #f)))

;; RET: run the table's action with the id of the row at point.
(define (tabulated-visit)
  (let ((row (tabulated-row-at-point))
        (st (tabulated-get (current-buffer))))
    (when (and row st (tab-action st))
      ((tab-action st) (row-id row)))))

;; S: cycle the sort column (ascending). Re-sorting the same column toggles to
;; descending, like clicking a tabulated-list header twice.
(define (tabulated-sort)
  (let* ((buffer (current-buffer))
         (st (tabulated-get buffer)))
    (when st
      (let* ((columns (tab-columns st))
             (n (length columns))
             (idx (tab-sort-idx st))
             (desc (tab-sort-desc st))
             (same? (>= idx 0))
             (next-idx (if (and same? (not desc)) idx (modulo (+ idx 1) n)))
             (next-desc (if (and same? (not desc)) #t #f)))
        (tabulated-set! buffer
                        (list (tab-mode st) columns (tab-rows st) (tab-action st)
                              next-idx next-desc (tab-display-rows st)))
        (tabulated-draw buffer)
        (message (string-append "Sorted by " (nth (nth columns next-idx) 0)
                                (if next-desc " (desc)" " (asc)")))))))

;; Sort programmatically by a specific column index.
(define (tabulated-sort-by-column idx desc)
  (let* ((buffer (current-buffer))
         (st (tabulated-get buffer)))
    (when st
      (tabulated-set! buffer
                      (list (tab-mode st) (tab-columns st) (tab-rows st) (tab-action st)
                            idx desc (tab-display-rows st)))
      (tabulated-draw buffer))))

;; Bind the conventional tabulated-list keys into `mode`'s keymap and turn off
;; line numbers (core's define-list-mode-keys), plus S for sorting.
(define (tabulated-setup-keys mode)
  (define-list-mode-keys mode 'tabulated-visit)
  (define-key mode "s" 'tabulated-sort))
