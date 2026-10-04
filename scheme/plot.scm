;; plot.scm — text charts (sparklines, bars, line/scatter plots, histograms,
;; heatmaps) built on the textplot-* primitives in src/textplot.rs.
;;
;; A PLOT is a list of rows; a row is (text spans), each span a
;; (start end face) BYTE range within that row's text. Rust rasterizes and
;; computes the spans, so nothing here does byte arithmetic on the multibyte
;; braille/block glyphs — composition only shifts spans by `utf8-length` of
;; whatever is prepended.
;;
;; Output: `plot-insert` (at point), `plot-section-emit` (into a lib-section
;; build, for magit-style dashboards), `plot-show` (its own buffer), or
;; `plot-text` + `plot-spans` to place it yourself.
;;
;; Options are a flat plist after the data: (plot-lines s 'width 60 'height 10).
;; Colors are faces defined in scheme/themes/*.scm: plot-series-0…5,
;; plot-axis, plot-label, plot-title, plot-heat-0…7 (+ their -bg backgrounds).
;;
;; Loads after core.scm and lib-section.scm (alphabetical), so it can use
;; utf8-length / pad-right / string-repeat / section-line.

;; --- plot values -------------------------------------------------------------

(define (plot-row text spans) (list text spans))
(define (plot-row-text row) (car row))
(define (plot-row-spans row) (cadr row))

;; Look `key` up in a flat (key value …) plist, else `default`.
(define (plot--opt opts key default)
  (cond ((or (null? opts) (null? (cdr opts))) default)
        ((eq? (car opts) key) (cadr opts))
        (else (plot--opt (cdr (cdr opts)) key default))))

(define (plot--shift spans n)
  (map (lambda (s) (list (+ (car s) n) (+ (cadr s) n) (nth s 2))) spans))

;; Build a row from (text face) segments; a #f face leaves that text uncolored.
(define (plot--segments segs)
  (let loop ((segs segs) (texts '()) (spans '()) (at 0))
    (if (null? segs)
        (plot-row (string-concat (reverse texts)) (reverse spans))
        (let* ((text (car (car segs)))
               (face (cadr (car segs)))
               (n (utf8-length text)))
          (loop (cdr segs)
                (cons text texts)
                (if (and face (> n 0)) (cons (list at (+ at n) face) spans) spans)
                (+ at n))))))

;; Row `a` followed by row `b` on the same line.
(define (plot--concat a b)
  (let ((n (utf8-length (plot-row-text a))))
    (plot-row (string-append (plot-row-text a) (plot-row-text b))
              (append (plot-row-spans a) (plot--shift (plot-row-spans b) n)))))

;; Prepend uncolored-or-`face` text to a row.
(define (plot--prepend prefix face row)
  (plot--concat (plot--segments (list (list prefix face))) row))

;; --- output ------------------------------------------------------------------

;; The plot's text, rows joined by newlines (no trailing newline).
(define (plot-text p) (join-lines (map plot-row-text p)))

;; Absolute (start end face) triples for the plot placed at byte `base` — the
;; shape `overlay-set-faces` takes.
(define (plot-spans p base)
  (let loop ((rows p) (at base) (acc '()))
    (if (null? rows)
        (reverse acc)
        (let ((row (car rows)))
          (loop (cdr rows)
                (+ at (utf8-length (plot-row-text row)) 1)
                (append (reverse (plot--shift (plot-row-spans row) at)) acc))))))

;; Insert the plot at point and color it.
(define (plot-insert p)
  (let ((base (point)))
    (insert (plot-text p))
    (overlay-set-faces (plot-spans p base))))

;; Emit the plot's rows into the current lib-section build (between
;; section-begin and section-render), its colors as section accents.
(define (plot-section-emit p)
  (for-each
    (lambda (row)
      (let ((b (section-point-byte)))
        (section-line (plot-row-text row))
        (for-each (lambda (s) (section-accent (+ b (car s)) (+ b (cadr s)) (nth s 2)))
                  (plot-row-spans row))))
    p))

;; Show the plot alone in buffer `name`.
(define (plot-show p name)
  (render-buffer name "plot-mode" (plot-text p) '())
  (overlay-set-faces (plot-spans p 0)))

;; --- numbers -----------------------------------------------------------------

(define (plot--num v) (exact->inexact v))
(define (plot--int v) (inexact->exact (round v)))
(define (plot--finite? v) (and (number? v) (not (nan? (exact->inexact v)))))
(define (plot--numbers vals) (map plot--num (filter plot--finite? vals)))
(define (plot--min nums) (if (null? nums) 0.0 (apply min nums)))
(define (plot--max nums) (if (null? nums) 0.0 (apply max nums)))

;; Widen a degenerate (lo = hi) range so scaling never divides by zero.
(define (plot--range lo hi)
  (if (> hi lo) (list lo hi) (list (- lo 1.0) (+ hi 1.0))))

(define (plot--label x)
  (cond ((string? x) x)
        ((symbol? x) (symbol->string x))
        ((number? x) (textplot-format x))
        (else "")))

(define (plot--pad-left s width)
  (let ((len (string-length s)))
    (if (>= len width) s (string-append (string-repeat " " (- width len)) s))))

(define (plot--series-face i)
  (string-append "plot-series-" (number->string (modulo i 6))))

;; --- sparkline / bars ----------------------------------------------------------

;; One ▁…█ glyph per value. Options: 'min 'max (default: the data's range).
(define (plot-sparkline-string values . opts)
  (let ((nums (plot--numbers values)))
    (textplot-sparkline values
                        (plot--opt opts 'min (plot--min nums))
                        (plot--opt opts 'max (plot--max nums)))))

;; A one-row plot. Options: 'min 'max 'face (default plot-series-0).
(define (plot-sparkline values . opts)
  (list (plot--segments
          (list (list (apply plot-sparkline-string values opts)
                      (plot--opt opts 'face "plot-series-0"))))))

;; Horizontal bar chart from (label value) items. Options: 'width (bar cells
;; at full scale, 40), 'max (default: largest value), 'face (plot-series-0),
;; 'colors #t (cycle series faces per bar), 'values #t (print each value).
(define (plot-bars items . opts)
  (let* ((width (plot--opt opts 'width 40))
         (vals (map (lambda (it) (plot--num (cadr it))) items))
         (mx (plot--num (plot--opt opts 'max (plot--max vals))))
         (labels (map (lambda (it) (plot--label (car it))) items))
         (lw (apply max (cons 0 (map string-length labels))))
         (face (plot--opt opts 'face "plot-series-0"))
         (cycle (plot--opt opts 'colors #f))
         (show (plot--opt opts 'values #t)))
    (let loop ((labels labels) (vals vals) (i 0) (rows '()))
      (if (null? labels)
          (reverse rows)
          (let* ((v (car vals))
                 (bar (textplot-hbar v mx width))
                 (gap (string-repeat " " (- (+ width 1) (string-length bar)))))
            (loop (cdr labels) (cdr vals) (+ i 1)
                  (cons (plot--segments
                          (list (list (pad-right (car labels) lw) "plot-label")
                                (list " " #f)
                                (list bar (if cycle (plot--series-face i) face))
                                (list (if show (string-append gap (textplot-format v)) "")
                                      "plot-label")))
                        rows)))))))

;; --- axes ----------------------------------------------------------------------

;; Widen lo…hi outward to whole tick steps, so the axis ends land on labeled
;; ticks (data spanning -0.9997…1.0 gets an axis of -1…1, not one labeled
;; only at 0). Used when the caller didn't pin the range.
(define (plot--nice-range lo hi nticks) (textplot-nice-range lo hi nticks))

;; Dots per cell for a canvas mode, (columns rows).
(define (plot--mode-dots mode)
  (cond ((eq? mode 'block) (list 1 8))
        ((eq? mode 'quadrant) (list 2 2))
        ((eq? mode 'half) (list 1 2))
        (else (list 2 4))))

;; Data → dot coordinates over a dot extent of `n` (0 … n-1).
(define (plot--scale v lo hi n)
  (* (/ (- v lo) (- hi lo)) (- n 1)))
(define (plot--y->dot v lo hi dh) (- (- dh 1) (plot--scale v lo hi dh)))

(define (plot--row-label tick-rows r)
  (cond ((null? tick-rows) #f)
        ((= (car (car tick-rows)) r) (cadr (car tick-rows)))
        (else (plot--row-label (cdr tick-rows) r))))

;; Frame canvas `rows` with a y axis (ticks over ylo…yhi), an x axis, x range
;; labels, and an optional title / legend row (a row, or #f).
(define (plot--frame rows ylo yhi dy nticks xlo-label xhi-label title legend)
  (if (null? rows)
      '()
      (let* ((height (length rows))
             (width (string-length (plot-row-text (car rows))))
             (dh (* height dy))
             (tick-rows
               (map (lambda (t)
                      (list (quotient (plot--int (plot--y->dot (car t) ylo yhi dh)) dy)
                            (cadr t)))
                    (textplot-ticks ylo yhi nticks)))
             (lw (apply max (cons 1 (map (lambda (t) (string-length (cadr t))) tick-rows))))
             (indent (string-repeat " " (+ lw 2)))
             (body
               (let loop ((rows rows) (r 0) (acc '()))
                 (if (null? rows)
                     (reverse acc)
                     (let ((label (plot--row-label tick-rows r)))
                       (loop (cdr rows) (+ r 1)
                             (cons (plot--concat
                                     (plot--segments
                                       (list (list (plot--pad-left (or label "") lw) "plot-label")
                                             (list (if label " ┤" " │") "plot-axis")))
                                     (car rows))
                                   acc))))))
             (axis (plot--segments
                     (list (list (string-repeat " " lw) #f)
                           (list (string-append " └" (string-repeat "─" width)) "plot-axis"))))
             (room (- width (string-length xlo-label) (string-length xhi-label)))
             (xlabels (plot--segments
                        (list (list indent #f)
                              (list (string-append xlo-label
                                                   (string-repeat " " (max 1 room))
                                                   xhi-label)
                                    "plot-label")))))
        (append (if title (list (plot--segments (list (list indent #f) (list title "plot-title"))))
                    '())
                body
                (list axis xlabels)
                (if legend (list (plot--prepend indent #f legend)) '())))))

;; ■ name ■ name … in series colors.
(define (plot--legend names)
  (let loop ((names names) (i 0) (segs '()))
    (if (null? names)
        (plot--segments (reverse segs))
        (loop (cdr names) (+ i 1)
              (append (list (list "  " #f)
                            (list (plot--label (car names)) "plot-label")
                            (list "■ " (plot--series-face i)))
                      segs)))))

;; --- line / scatter --------------------------------------------------------------

;; A series' data as ((x y) …): a list of y numbers (x = index) or of (x y) lists.
(define (plot--points data)
  (cond ((null? data) '())
        ((number? (car data))
         (let loop ((d data) (i 0) (acc '()))
           (if (null? d)
               (reverse acc)
               (loop (cdr d) (+ i 1)
                     (if (plot--finite? (car d))
                         (cons (list (plot--num i) (plot--num (car d))) acc)
                         acc)))))
        (else
         (map (lambda (p) (list (plot--num (car p)) (plot--num (cadr p))))
              (filter (lambda (p) (and (list? p) (= (length p) 2)
                                       (plot--finite? (car p)) (plot--finite? (cadr p))))
                      data)))))

;; Series as ((name points) …). Bare data (a list of numbers) is one unnamed series.
(define (plot--series series)
  (if (and (not (null? series)) (number? (car series)))
      (list (list "" (plot--points series)))
      (map (lambda (s) (list (car s) (plot--points (cadr s)))) series)))

(define (plot--xy series opts scatter?)
  (let* ((series (plot--series series))
         (width (plot--opt opts 'width 60))
         (height (plot--opt opts 'height 12))
         (mode (plot--opt opts 'mode 'braille))
         (dots (plot--mode-dots mode))
         (dw (* width (car dots)))
         (dh (* height (cadr dots)))
         (all (apply append (map cadr series)))
         (xr (plot--range (plot--num (plot--opt opts 'x-min (plot--min (map car all))))
                          (plot--num (plot--opt opts 'x-max (plot--max (map car all))))))
         (nticks (max 2 (quotient height 3)))
         (yr (let ((r (plot--range (plot--num (plot--opt opts 'y-min (plot--min (map cadr all))))
                                   (plot--num (plot--opt opts 'y-max (plot--max (map cadr all)))))))
               (if (or (plot--opt opts 'y-min #f) (plot--opt opts 'y-max #f))
                   r
                   (plot--nice-range (car r) (cadr r) nticks))))
         (xlo (car xr)) (xhi (cadr xr)) (ylo (car yr)) (yhi (cadr yr))
         (dot (lambda (p) (list (plot--scale (car p) xlo xhi dw)
                                (plot--y->dot (cadr p) ylo yhi dh))))
         (ops
           (let loop ((s series) (i 0) (acc '()))
             (if (null? s)
                 (reverse acc)
                 (let ((face (plot--series-face i))
                       (pts (map dot (cadr (car s)))))
                   (loop (cdr s) (+ i 1)
                         (if scatter?
                             (append (reverse (map (lambda (p) (list "point" (car p) (cadr p) face)) pts))
                                     acc)
                             (cons (list "polyline" face (apply append pts)) acc)))))))
         (names (map car series))
         (legend? (plot--opt opts 'legend (> (length series) 1))))
    (plot--frame (textplot-render width height mode ops)
                 ylo yhi (cadr dots) nticks
                 (textplot-format xlo) (textplot-format xhi)
                 (plot--opt opts 'title #f)
                 (if legend? (plot--legend names) #f))))

;; Line chart. `series`: a list of y numbers, or ((name data) …) where data is
;; y numbers (x = index) or (x y) lists. Options: 'width 60 'height 12 (plot
;; area, in cells), 'mode ('braille | 'quadrant | 'block), 'x-min 'x-max
;; 'y-min 'y-max, 'title, 'legend (default: when >1 series). Where series
;; cross, the later one's color wins the cell.
(define (plot-lines series . opts) (plot--xy series opts #f))

;; Scatter plot: same arguments as plot-lines, points unconnected.
(define (plot-scatter series . opts) (plot--xy series opts #t))

;; --- histogram ---------------------------------------------------------------------

(define (plot--bin-index v lo span bins)
  (max 0 (min (- bins 1) (inexact->exact (floor (* (/ (- v lo) span) bins))))))

;; Vertical histogram of `values`. Options: 'bins 10, 'height 8, 'bin-width 3
;; (cells per bin; bins >1 cell wide get a gap), 'min 'max, 'face, 'title.
(define (plot-histogram values . opts)
  (let* ((nums (plot--numbers values))
         (bins (max 1 (plot--opt opts 'bins 10)))
         (height (plot--opt opts 'height 8))
         (bw (max 1 (plot--opt opts 'bin-width 3)))
         (face (plot--opt opts 'face "plot-series-0"))
         (lo (plot--num (plot--opt opts 'min (plot--min nums))))
         (hi (plot--num (plot--opt opts 'max (plot--max nums))))
         (span (if (> hi lo) (- hi lo) 1.0))
         (idx (map (lambda (v) (plot--bin-index v lo span bins))
                   (filter (lambda (v) (and (>= v lo) (<= v hi))) nums)))
         (counts (let loop ((k (- bins 1)) (acc '()))
                   (if (< k 0)
                       acc
                       (loop (- k 1) (cons (length (filter (lambda (i) (= i k)) idx)) acc)))))
         (nticks (max 2 (min (max 1 (apply max counts)) (quotient height 2))))
         (cmax (cadr (plot--nice-range 0.0 (plot--num (max 1 (apply max counts))) nticks)))
         (dh (* height 8))
         (gap (if (> bw 1) 1 0))
         (ops (let loop ((cs counts) (k 0) (acc '()))
                (if (null? cs)
                    acc
                    (let ((level (plot--int (* dh (/ (car cs) cmax)))))
                      (loop (cdr cs) (+ k 1)
                            (if (> level 0)
                                (cons (list "rect" (* k bw) (- dh level)
                                            (- (+ (* k bw) bw) 1 gap) (- dh 1) face)
                                      acc)
                                acc)))))))
    (plot--frame (textplot-render (* bins bw) height 'block ops)
                 0.0 cmax 8 nticks
                 (textplot-format lo) (textplot-format hi)
                 (plot--opt opts 'title #f) #f)))

;; --- heatmap -----------------------------------------------------------------------

(define (plot--heat-level v lo hi)
  (if (> hi lo)
      (plot--int (* 7 (max 0.0 (min 1.0 (/ (- v lo) (- hi lo))))))
      0))

(define (plot--heat-face level) (string-append "plot-heat-" (number->string level)))

;; Heatmap of a matrix (a list of rows of numbers). Options:
;;   'mode 'cell (default: each value is 'cell-width spaces on a background
;;          color — robust with any font and line height) or 'half (two data
;;          rows per text row via ▀ foreground + background; denser, but the
;;          two halves look uneven when the line height exceeds the glyph)
;;   'cell-width 2, 'min 'max, 'row-labels, 'col-labels ('cell mode), 'title,
;;   'scale #t (a min…max color-scale row).
;; Colors: plot-heat-0…7 (foreground) / plot-heat-0…7-bg (background).
(define (plot-heatmap matrix . opts)
  (let* ((mode (plot--opt opts 'mode 'cell))
         (cw (max 1 (plot--opt opts 'cell-width 2)))
         (nums (plot--numbers (apply append matrix)))
         (lo (plot--num (plot--opt opts 'min (plot--min nums))))
         (hi (plot--num (plot--opt opts 'max (plot--max nums))))
         (row-labels (map plot--label (plot--opt opts 'row-labels '())))
         (lw (if (null? row-labels) 0 (+ 1 (apply max (map string-length row-labels)))))
         (indent (string-repeat " " lw))
         (cell (string-repeat " " cw))
         (title (plot--opt opts 'title #f))
         (body
           (if (eq? mode 'half)
               (let* ((cols (apply max (cons 0 (map length matrix))))
                      (ops (let rloop ((rows matrix) (r 0) (acc '()))
                             (if (null? rows)
                                 acc
                                 (rloop (cdr rows) (+ r 1)
                                        (let cloop ((vs (car rows)) (c 0) (acc acc))
                                          (if (null? vs)
                                              acc
                                              (cloop (cdr vs) (+ c 1)
                                                     (if (plot--finite? (car vs))
                                                         (cons (list "point" c r
                                                                     (plot--heat-face
                                                                       (plot--heat-level (plot--num (car vs)) lo hi)))
                                                               acc)
                                                         acc))))))))
                      (ops (reverse ops)))
                 (map (lambda (row) (plot--prepend indent #f row))
                      (textplot-render cols (quotient (+ (length matrix) 1) 2) 'half ops)))
               (let loop ((rows matrix) (labels row-labels) (acc '()))
                 (if (null? rows)
                     (reverse acc)
                     (loop (cdr rows)
                           (if (null? labels) '() (cdr labels))
                           (cons (plot--segments
                                   (cons (list (pad-right (if (null? labels) "" (car labels)) lw)
                                               "plot-label")
                                         (map (lambda (v)
                                                (list cell
                                                      (if (plot--finite? v)
                                                          (string-append
                                                            (plot--heat-face (plot--heat-level (plot--num v) lo hi))
                                                            "-bg")
                                                          #f)))
                                              (car rows))))
                                 acc))))))
         (header
           (let ((cols (map plot--label (plot--opt opts 'col-labels '()))))
             (if (or (null? cols) (eq? mode 'half))
                 '()
                 (list (plot--segments
                         (list (list indent #f)
                               (list (string-concat
                                       (map (lambda (c)
                                              (pad-right (if (> (string-length c) cw) (substring c 0 cw) c) cw))
                                            cols))
                                     "plot-label")))))))
         (scale
           (if (plot--opt opts 'scale #t)
               (list (plot--segments
                       (append (list (list indent #f)
                                     (list (string-append (textplot-format lo) " ") "plot-label"))
                               (map (lambda (k) (list cell (string-append (plot--heat-face k) "-bg")))
                                    (list 0 1 2 3 4 5 6 7))
                               (list (list (string-append " " (textplot-format hi)) "plot-label")))))
               '())))
    (append (if title (list (plot--segments (list (list indent #f) (list title "plot-title")))) '())
            header body scale)))

;; --- demo --------------------------------------------------------------------------

(define (plot--demo-wave n f)
  (let loop ((i 0) (acc '()))
    (if (= i n) (reverse acc) (loop (+ i 1) (cons (f (plot--num i)) acc)))))

;; M-x plot-demo — every chart type in one buffer (g redraws).
(define-command (plot-demo)
  (section-begin)
  (let ((heading (lambda (s) (section-line "") (section-line s "plot-title")))
        (sine (plot--demo-wave 60 (lambda (x) (sin (/ x 6.0)))))
        (cosine (plot--demo-wave 60 (lambda (x) (* 0.6 (cos (/ x 4.0)))))))
    (section-line "Text plotting demo — textplot-* primitives + scheme/plot.scm" "plot-title")
    (heading "plot-sparkline")
    (plot-section-emit
      (list (plot--concat (plot--segments (list (list "cpu  " "plot-label")))
                          (car (plot-sparkline (list 1 3 2 5 8 6 4 7 9 5 3 2 4 6 8))))))
    (heading "plot-bars")
    (plot-section-emit
      (plot-bars (list (list "rust" 48210) (list "scheme" 15302)
                       (list "c" 7120) (list "toml" 640))
                 'width 30 'colors #t))
    (heading "plot-lines")
    (plot-section-emit
      (plot-lines (list (list "sin" sine) (list "0.6 cos" cosine))
                  'width 60 'height 10 'title "two series, braille"))
    (heading "plot-scatter")
    (plot-section-emit
      (plot-scatter (list (list "a" (plot--demo-wave 40 (lambda (x) (list x (* x (sin x))))))
                          (list "b" (plot--demo-wave 40 (lambda (x) (list x (* 0.5 x x 0.1))))))
                    'width 50 'height 8))
    (heading "plot-histogram")
    (plot-section-emit
      (plot-histogram (plot--demo-wave 400 (lambda (x) (+ (sin x) (sin (* 1.7 x)) (cos (* 0.3 x)))))
                      'bins 12 'height 6))
    (heading "plot-heatmap ('cell)")
    (plot-section-emit
      (plot-heatmap (plot--demo-wave 6 (lambda (r) (plot--demo-wave 12 (lambda (c) (* (sin (/ r 2.0)) (cos (/ c 3.0)))))))
                    'row-labels (list "mon" "tue" "wed" "thu" "fri" "sat")))
    (heading "plot-heatmap ('half)")
    (plot-section-emit
      (plot-heatmap (plot--demo-wave 8 (lambda (r) (plot--demo-wave 24 (lambda (c) (+ r c)))))
                    'mode 'half)))
  (section-render "*plot-demo*" "plot-mode")
  (define-key "plot-mode" "g" 'plot-demo))
