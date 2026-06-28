;; licenses.scm — `M-x licenses` opens a read-only *licenses* buffer with
;; Macros' license and third-party acknowledgments (mirrors the repo NOTICE).
;; The full text of every bundled Rust crate license lives in
;; THIRD-PARTY-LICENSES.md, which is far too large to embed here.

(define *licenses-text*
  "Macros — License & Acknowledgments
====================================

Macros is free software, licensed under the MIT License. Copyright (c) 2026
Kyle Gwinnup. See the LICENSE file distributed with Macros for the full text.


Magit, Org mode, and Helm
-------------------------
Macros includes features named after, and modeled on, the GNU Emacs packages
Magit, Org mode, and Helm. These are original reimplementations
written in Scheme on Macros' own primitives — no source code from those
packages is copied or included, and Macros is not affiliated with or endorsed
by them. They are named only to credit the work that inspired them, and are
covered by Macros' own MIT License.


Lilex font
----------
Macros bundles the Lilex typeface (Copyright 2019 The Lilex Project Authors),
licensed under the SIL Open Font License, Version 1.1.


Third-party Rust crates
-----------------------
Macros links many Rust crates — the tree-sitter grammars, the GPUI renderer,
the Steel interpreter, the Alacritty terminal core, and their dependencies.
Each crate's full license text is reproduced in THIRD-PARTY-LICENSES.md,
distributed with the Macros source.


Press q to close this buffer.")

;; M-x licenses — show the notice in a read-only buffer (help-mode gives q / n / p).
(define (licenses)
  (render-buffer "*licenses*" "help-mode" *licenses-text* '()))
(register-command 'licenses)
