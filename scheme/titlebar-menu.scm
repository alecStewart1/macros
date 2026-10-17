;; requires: core

;; Title-bar menus — labelled quick actions in dropdowns beside the projects
;; switcher. Each menu is named by its title (the trigger's label) and holds
;; rows of (label . command); clicking a row runs its command. Menus appear
;; left to right in the order they were first defined, rows in insertion order.
;;
;;   (titlebar-menu-add "Dashboards" "Postgres"   'psql-dashboard)
;;   (titlebar-menu-add "Dashboards" "Kubernetes" 'k8s-dashboard)
;;   (titlebar-menu-add "Tools" "Scratch" (lambda () (switch-to-buffer "*scratch*")))
;;
;; A command is a function name (symbol or string) or a no-argument procedure.
;; Prefer a name for anything a package defines: it's looked up when the row is
;; clicked, so it works even before the package has loaded. A lambda's body is
;; resolved when the lambda is written, so it may only call functions that
;; already exist.
;;
;; All the logic lives here — Rust only draws the dropdowns (workspace.rs) and
;; calls the clicked row's command by name (`set-titlebar-menu`).

(define *titlebar-menus* '())  ;; alist: title -> list of (label . command-name), in order

(define (titlebar-menu-items title)
  (or (alist-ref *titlebar-menus* title) '()))

;; Publish `title`'s rows to the title bar (an empty menu is removed).
(define (titlebar-menu-sync title)
  (let ((rows (titlebar-menu-items title)))
    (set-titlebar-menu title (map car rows) (map cdr rows))))

;; Store `rows` under `title`, keeping the menu's position when it exists.
(define (titlebar-menu-store! title rows)
  (set! *titlebar-menus*
        (if (alist-ref *titlebar-menus* title)
            (map (lambda (kv) (if (equal? (car kv) title) (cons title rows) kv))
                 *titlebar-menus*)
            (append *titlebar-menus* (list (cons title rows)))))
  (titlebar-menu-sync title))

;; Add the row `label` running `command` to the menu `title` (created on first
;; use). Re-adding a label replaces its command in place.
(define (titlebar-menu-add title label command)
  (let* ((name (hook->name command))
         (rows (titlebar-menu-items title)))
    (titlebar-menu-store!
     title
     (if (alist-ref rows label)
         (map (lambda (kv) (if (equal? (car kv) label) (cons label name) kv)) rows)
         (append rows (list (cons label name)))))))

;; Drop the row `label` from the menu `title`; the menu goes when it empties.
(define (titlebar-menu-remove title label)
  (titlebar-menu-store! title (alist-remove (titlebar-menu-items title) label)))

;; Remove the menu `title` and all its rows.
(define (titlebar-menu-clear title)
  (set! *titlebar-menus* (alist-remove *titlebar-menus* title))
  (set-titlebar-menu title '() '()))
