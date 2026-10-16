;; requires: core org plot

;; plotting-org.scm — Org table → text chart (org-plot, drawn with plot.scm).
;;
;; `org-plot/text` (C-c " g) charts the table at point and writes the chart as
;; a #+RESULTS: block under it, colored like a scheme babel chart (see
;; org--write-results in org.scm). Re-running replaces the block. Settings come
;; from a `#+PLOT:` line above the table, in Emacs org-plot's syntax:
;;
;;   #+PLOT: title:"Requests" ind:1 deps:(2 3) type:lines width:60 height:12
;;
;;   ind     the x column (1-based); default 1. Numbers in it are x values;
;;           anything else labels the bars (lines then use the row number).
;;   deps    the columns to plot; default every other column with a number.
;;   type    lines (default; Emacs's 2d), bars, or scatter. Emacs's
;;           with:points means scatter and with:boxes / with:histograms bars.
;;   title, width, height   passed to the chart.
;;
;; Requires org.scm and plot.scm, since Steel resolves a name when the code
;; using it is defined.

;; --- #+PLOT: options --------------------------------------------------------

;; Every key:value of a #+PLOT: line as ((key value) …). A value is "quoted",
;; a (parenthesised list), or a bare word.
(define (org-plot--parse-options line)
  (let ((pat "([A-Za-z-]+):(\"[^\"]*\"|\\([^)]*\\)|[^ \t]+)"))
    (let loop ((s line) (acc '()))
      (let ((m (string-match pat s)))
        (if (< m 0)
            (reverse acc)
            (let* ((c (regexp-captures pat s))
                   (val (nth c 2))
                   (val (if (and (> (string-length val) 1)
                                 (member (substring val 0 1) (list "\"" "(")))
                            (substring val 1 (- (string-length val) 1))
                            val)))
              (loop (byte-drop s (+ m (utf8-length (car c))))
                    (cons (list (string-downcase (nth c 1)) val) acc))))))))

(define (org-plot--opt opts key default)
  (let ((e (assoc key opts))) (if e (cadr e) default)))

;; The #+PLOT: line among the keyword lines directly above table line `start`,
;; or "".
(define (org-plot--plot-line lines start)
  (let up ((i (- start 1)))
    (cond ((< i 0) "")
          ((>= (string-match "^[ \t]*#\\+[pP][lL][oO][tT]:" (nth lines i)) 0) (nth lines i))
          ((org--keyword-line? (nth lines i)) (up (- i 1)))
          (else ""))))

;; --- table → data -----------------------------------------------------------

;; A cell's number, or #f.
(define (org-plot--num cell)
  (let ((t (string-trim cell)))
    (if (equal? t "") #f (let ((n (string->number t))) (if (number? n) n #f)))))

(define (org-plot--col cells c) (org--cell cells c))

;; ((header-cells or #f) data-rows) for a table slice; a header is the rows
;; above the first hline (the last of them names the columns).
(define (org-plot--table slice)
  (let* ((grid (map cdr (org--table-grid slice)))
         (first (org--table-first-data-row slice)))
    (list (if (> first 1) (nth grid (- first 2)) #f)
          (org--slice grid (- first 1) (- (length grid) 1)))))

;; Integers 0..n-1.
(define (org-plot--range n)
  (let loop ((i (- n 1)) (acc '())) (if (< i 0) acc (loop (- i 1) (cons i acc)))))

;; Default deps: every column but `ind` that has a number in it.
(define (org-plot--default-deps rows ncols ind)
  (filter (lambda (c)
            (and (not (= c ind))
                 (not (all? (lambda (r) (not (org-plot--num (org-plot--col r c)))) rows))))
          (org-plot--range ncols)))

;; Column numbers from a deps value like "2 3" (1-based) → 0-based.
(define (org-plot--parse-deps s)
  (filter (lambda (n) (>= n 0))
          (map (lambda (w) (let ((n (string->number w))) (if (integer? n) (- n 1) -1)))
               (filter (lambda (w) (not (equal? w ""))) (split-many (string-trim s) " ")))))

;; --- chart ------------------------------------------------------------------

;; A one-row plot holding `text` in the title face.
(define (org-plot--title-row text)
  (list (plot-row text (list (list 0 (utf8-length text) "plot-title")))))

;; The chart for the table slice and #+PLOT: options, or a string saying why
;; there is none.
(define (org-plot--chart slice opts)
  (let* ((t (org-plot--table slice))
         (header (car t)) (rows (cadr t))
         (ncols (org--table-ncols slice))
         (ind (let ((n (string->number (org-plot--opt opts "ind" "1"))))
                (if (integer? n) (- n 1) 0)))
         (deps (let ((d (org-plot--opt opts "deps" #f)))
                 (if d (org-plot--parse-deps d) (org-plot--default-deps rows ncols ind))))
         (name (lambda (c) (let ((h (if header (org-plot--col header c) "")))
                             (if (equal? h "") (string-append "$" (number->string (+ c 1))) h))))
         (type (string-downcase (org-plot--opt opts "type" "lines")))
         (with (string-downcase (org-plot--opt opts "with" "")))
         (kind (cond ((or (member type (list "bars" "bar")) (member with (list "boxes" "histograms"))) 'bars)
                     ((or (equal? type "scatter") (equal? with "points")) 'scatter)
                     (else 'lines)))
         (size (lambda (key)
                 (let ((n (string->number (org-plot--opt opts key ""))))
                   (if (integer? n) (list (string->symbol key) n) '()))))
         (title (org-plot--opt opts "title" #f))
         (numeric-x (and (not (null? rows))
                         (all? (lambda (r) (org-plot--num (org-plot--col r ind))) rows))))
    (cond
      ((null? rows) "No data rows to plot")
      ((null? deps) "No numeric columns to plot")
      ((eq? kind 'bars)
       ;; One bar chart per dep, each under its column name in its own color.
       (let loop ((ds deps) (i 0) (acc (if title (org-plot--title-row title) '())))
         (if (null? ds)
             acc
             (let* ((c (car ds))
                    (items (filter (lambda (it) (cadr it))
                                   (map (lambda (r) (list (org-plot--col r ind)
                                                          (org-plot--num (org-plot--col r c))))
                                        rows)))
                    (chart (apply plot-bars items 'face (plot--series-face i) (size "width"))))
               (loop (cdr ds) (+ i 1)
                     (append acc
                             (if (null? acc) '() (list (plot-row "" '())))
                             (if (> (length deps) 1) (org-plot--title-row (name c)) '())
                             chart))))))
      (else
       (let ((series
              (map (lambda (c)
                     (list (name c)
                           (let loop ((rs rows) (i 0) (acc '()))
                             (if (null? rs)
                                 (reverse acc)
                                 (let* ((r (car rs)) (y (org-plot--num (org-plot--col r c))))
                                   (loop (cdr rs) (+ i 1)
                                         (if y
                                             (cons (list (if numeric-x (org-plot--num (org-plot--col r ind)) i) y) acc)
                                             acc)))))))
                   deps)))
         (apply (if (eq? kind 'scatter) plot-scatter plot-lines)
                series
                (append (size "width") (size "height")
                        (if title (list 'title title) '()))))))))

;; C-c " g — chart the table at point into a #+RESULTS: block below it.
(define-command (org-plot/text)
  (let* ((L0 (buffer-line)) (b (org--table-bounds L0)))
    (if (not b)
        (message "Not in a table")
        (let* ((lines (org--lines))
               (start (nth b 0)) (end (nth b 1))
               (opts (org-plot--parse-options (org-plot--plot-line lines start)))
               (chart (org-plot--chart (org--slice lines start end) opts)))
          (if (string? chart)
              (message chart)
              (begin
                (org--write-results (or (org--tblfm-after end) end) chart)
                (goto-line L0)
                (org--fontify-schedule)
                (message "Table plotted")))))))

(define-key "org-mode" "ctrl-c \" g" 'org-plot/text)
