//! Grid editing behavior end to end, filling the gaps the other grid suites
//! leave: motion and selection at the data's edges under Emacs, evil and
//! helix keys, inserting and deleting rows and columns, workbook sheet
//! edge cases, and multiple cursors. A real `Editor` on gpui's headless test
//! platform with the bundled Scheme, so `scheme/grid.scm`'s keymaps are the
//! ones exercised.

use std::path::PathBuf;

use gpui::{Entity, Keystroke, TestAppContext};

use super::*;

/// A grid on a fresh temp file under `mode`: "emacs", "evil" or "helix".
fn setup(cx: &mut TestAppContext, csv: &str, mode: &str) -> (Entity<Editor>, PathBuf) {
    test_context(cx);
    cx.update(|cx| {
        cx.set_global(crate::theme::Theme::default());
        cx.set_global(crate::settings::Settings::default());
    });
    // A counter as well as the clock, so parallel tests never share a file.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "macros-grid-editor-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("data.csv");
    std::fs::write(&path, csv).unwrap();
    let editor = cx.new(Editor::new);
    let p = path.clone();
    let modal = mode != "emacs";
    editor.update(cx, |e, cx| {
        for src in crate::runtime::startup_scripts() {
            e.run_script_blocking(src, cx);
        }
        if modal {
            e.run_script_blocking("(evil-mode)", cx);
        }
        e.mark_config_loaded(cx);
        if modal {
            e.evil = EvilState::Normal;
        }
        e.open_grid("data.csv".into(), p.clone(), std::fs::read(&p).unwrap(), cx);
        if mode == "helix" {
            e.run_script_blocking("(helix-mode)", cx);
        }
    });
    settle(&editor, cx);
    if mode == "helix" {
        assert!(editor.read_with(cx, |e, _| e.modal_helix));
    }
    (editor, path)
}

fn settle(editor: &Entity<Editor>, cx: &mut TestAppContext) {
    for _ in 0..6 {
        cx.run_until_parked();
        editor.read_with(cx, |e, _| {
            let _ = e.script.eval("0");
        });
        cx.run_until_parked();
    }
}

/// Feed keys: space-separated tokens. Action keys go through the
/// gpui-action path the way the platform routes them; everything else
/// through `dispatch_keystroke`.
fn keys(editor: &Entity<Editor>, cx: &mut TestAppContext, seq: &str) {
    for k in seq.split(' ').filter(|k| !k.is_empty()) {
        let action = matches!(
            k,
            "enter"
                | "tab"
                | "up"
                | "down"
                | "left"
                | "right"
                | "home"
                | "end"
                | "backspace"
                | "delete"
        );
        editor.update(cx, |e, cx| {
            if action {
                if !e.grid_action_key(k, cx) {
                    panic!("grid did not take action key {k}");
                }
            } else {
                let ks: Keystroke = if k.chars().count() == 1 {
                    super::ex::token_keystroke(k).expect("printable key")
                } else {
                    Keystroke::parse(k).expect("keystroke")
                };
                e.dispatch_keystroke(&ks, cx);
            }
        });
        settle(editor, cx);
    }
}

/// Run Scheme as a dispatched command batch, the path key bindings take.
fn scheme(editor: &Entity<Editor>, cx: &mut TestAppContext, src: &str) {
    editor.update(cx, |e, _| {
        e.publish_point();
        e.script.dispatch(src, false);
    });
    settle(editor, cx);
}

fn eval(editor: &Entity<Editor>, cx: &mut TestAppContext, src: &str) -> String {
    editor.update(cx, |e, _| e.publish_point());
    editor.read_with(cx, |e, _| {
        e.script.eval(src).expect("eval").value.unwrap_or_default()
    })
}

/// Type `input` into the open minibuffer and press RET.
fn submit(editor: &Entity<Editor>, cx: &mut TestAppContext, input: &str) {
    editor.update(cx, |e, cx| {
        let mb = e.minibuffer.as_mut().expect("a minibuffer read");
        mb.input = input.to_string();
        mb.cursor = input.len();
        e.mb_submit(cx);
    });
    settle(editor, cx);
}

fn cell(editor: &Entity<Editor>, cx: &mut TestAppContext, r: usize, c: usize) -> String {
    editor.read_with(cx, |e, _| {
        e.active.grid.as_ref().unwrap().cell(r, c).to_string()
    })
}

fn row(editor: &Entity<Editor>, cx: &mut TestAppContext, r: usize) -> Vec<String> {
    let cols = used(editor, cx).1;
    (0..cols).map(|c| cell(editor, cx, r, c)).collect()
}

fn cursor(editor: &Entity<Editor>, cx: &mut TestAppContext) -> (usize, usize) {
    editor.read_with(cx, |e, _| e.active.grid.as_ref().unwrap().view.cursor)
}

fn used(editor: &Entity<Editor>, cx: &mut TestAppContext) -> (usize, usize) {
    editor.read_with(cx, |e, _| e.active.grid.as_ref().unwrap().used())
}

/// The selection's bounds, or `None` with no anchor.
fn sel(editor: &Entity<Editor>, cx: &mut TestAppContext) -> Option<(usize, usize, usize, usize)> {
    editor.read_with(cx, |e, _| {
        let g = e.active.grid.as_ref().unwrap();
        g.view.anchor.map(|_| {
            let r = g.view.selection(g.used());
            (r.r0, r.c0, r.r1, r.c1)
        })
    })
}

/// The extra (non-primary) cursors, sorted.
fn extra_cursors(editor: &Entity<Editor>, cx: &mut TestAppContext) -> Vec<(usize, usize)> {
    editor.read_with(cx, |e, _| {
        let mut c = e.active.grid.as_ref().unwrap().view.cursors.clone();
        c.sort_unstable();
        c
    })
}

fn status(editor: &Entity<Editor>, cx: &mut TestAppContext) -> String {
    editor.read_with(cx, |e, _| e.status.to_string())
}

fn modified(editor: &Entity<Editor>, cx: &mut TestAppContext) -> bool {
    editor.read_with(cx, |e, _| e.active.modified)
}

fn sheets(editor: &Entity<Editor>, cx: &mut TestAppContext) -> String {
    eval(editor, cx, "(grid-sheets)")
}

fn active_name(editor: &Entity<Editor>, cx: &mut TestAppContext) -> String {
    editor.read_with(cx, |e, _| e.active.name.clone())
}

const CSV: &str = "name,qty,note\napple,3,\"red, crisp\"\npear,007,\nfig,12,ok\n";

// --- Motion at the edges ------------------------------------------------------

#[gpui::test]
fn emacs_jumps_to_row_ends_and_the_data_corners(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    keys(&ed, cx, "ctrl-n ctrl-e");
    assert_eq!(cursor(&ed, cx), (1, 2), "C-e: the row's last filled cell");
    keys(&ed, cx, "ctrl-a");
    assert_eq!(cursor(&ed, cx), (1, 0));
    keys(&ed, cx, "alt->");
    assert_eq!(cursor(&ed, cx), (3, 2), "M->: the data's last cell");
    keys(&ed, cx, "alt-<");
    assert_eq!(cursor(&ed, cx), (0, 0));
    keys(&ed, cx, "ctrl-end");
    assert_eq!(cursor(&ed, cx), (3, 2));
    keys(&ed, cx, "ctrl-home");
    assert_eq!(cursor(&ed, cx), (0, 0));
    keys(&ed, cx, "ctrl-n ctrl-n end");
    assert_eq!(cursor(&ed, cx), (2, 1), "End skips the empty note");
    keys(&ed, cx, "home");
    assert_eq!(cursor(&ed, cx), (2, 0));
}

#[gpui::test]
fn emacs_steps_run_on_past_the_data(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    keys(&ed, cx, "ctrl-f ctrl-f ctrl-f ctrl-f");
    assert_eq!(cursor(&ed, cx), (0, 4), "no wall at the last column");
    keys(&ed, cx, "ctrl-n ctrl-n ctrl-n ctrl-n ctrl-n");
    assert_eq!(cursor(&ed, cx), (5, 4), "nor at the last row");
    assert_eq!(used(&ed, cx), (4, 3), "visiting empty space adds nothing");
    assert!(!modified(&ed, cx));
    // M-> comes back to the data's corner from out there.
    keys(&ed, cx, "alt->");
    assert_eq!(cursor(&ed, cx), (3, 2));
}

#[gpui::test]
fn emacs_up_and_left_at_the_origin_select_the_line(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    keys(&ed, cx, "ctrl-p");
    assert_eq!(cursor(&ed, cx), (0, 0));
    assert_eq!(sel(&ed, cx), Some((0, 0, 3, 0)), "C-p on row 1: column A");
    keys(&ed, cx, "ctrl-n");
    assert_eq!((cursor(&ed, cx), sel(&ed, cx)), ((0, 0), None));
    keys(&ed, cx, "ctrl-b");
    assert_eq!(sel(&ed, cx), Some((0, 0, 0, 2)), "C-b on column A: row 1");
    keys(&ed, cx, "ctrl-f");
    assert_eq!((cursor(&ed, cx), sel(&ed, cx)), ((0, 0), None));
}

#[gpui::test]
fn edge_jumps_cross_gaps_and_stop_at_the_data(cx: &mut TestAppContext) {
    // Column A: 1 2 _ 4 5 — runs separated by a gap.
    let (ed, _) = setup(cx, "1,x\n2,\n,\n4,\n5,y\n", "emacs");
    keys(&ed, cx, "alt-n");
    assert_eq!(cursor(&ed, cx), (1, 0), "to the end of the filled run");
    keys(&ed, cx, "alt-n");
    assert_eq!(
        cursor(&ed, cx),
        (3, 0),
        "over the gap to the next filled cell"
    );
    keys(&ed, cx, "alt-n");
    assert_eq!(cursor(&ed, cx), (4, 0));
    keys(&ed, cx, "alt-n");
    assert_eq!(cursor(&ed, cx), (4, 0), "nothing further: the data's edge");
    keys(&ed, cx, "alt-p");
    assert_eq!(cursor(&ed, cx), (3, 0));
    keys(&ed, cx, "alt-p alt-p");
    assert_eq!(cursor(&ed, cx), (0, 0));
    keys(&ed, cx, "alt-p");
    assert_eq!(cursor(&ed, cx), (0, 0), "row 1 is the top");
    // Across a row: B1 is filled, so M-f runs to it and then stops.
    keys(&ed, cx, "alt-f");
    assert_eq!(cursor(&ed, cx), (0, 1));
    keys(&ed, cx, "alt-f");
    assert_eq!(cursor(&ed, cx), (0, 1), "the last column of the data");
    keys(&ed, cx, "alt-b alt-b");
    assert_eq!(cursor(&ed, cx), (0, 0));
}

#[gpui::test]
fn row_end_and_first_nonempty_follow_ragged_rows(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, "a,b,c\n1,,\n,,z\n,y,\n", "evil");
    keys(&ed, cx, "j $");
    assert_eq!(cursor(&ed, cx), (1, 0), "$ on a row with only A filled");
    keys(&ed, cx, "j ^");
    assert_eq!(cursor(&ed, cx), (2, 2), "^ skips the empty leading cells");
    keys(&ed, cx, "j ^");
    assert_eq!(cursor(&ed, cx), (3, 1));
    keys(&ed, cx, "$");
    assert_eq!(cursor(&ed, cx), (3, 1));
    keys(&ed, cx, "0");
    assert_eq!(cursor(&ed, cx), (3, 0));
    // An entirely empty row past the data: both land on column A.
    keys(&ed, cx, "j l l ^");
    assert_eq!(cursor(&ed, cx), (4, 0));
    keys(&ed, cx, "l l $");
    assert_eq!(cursor(&ed, cx), (4, 0));
}

#[gpui::test]
fn next_and_previous_cell_wrap_between_rows(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    keys(&ed, cx, "ctrl-e tab");
    assert_eq!(cursor(&ed, cx), (1, 0), "Tab off the last column wraps");
    keys(&ed, cx, "shift-tab");
    assert_eq!(cursor(&ed, cx), (0, 2), "S-Tab off column A wraps back");
    keys(&ed, cx, "ctrl-a shift-tab");
    assert_eq!(cursor(&ed, cx), (0, 0), "nowhere before the first cell");
    // Under evil, w / b do the same.
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "j b");
    assert_eq!(cursor(&ed, cx), (0, 2));
    keys(&ed, cx, "w");
    assert_eq!(cursor(&ed, cx), (1, 0));
}

#[gpui::test]
fn paging_moves_by_a_screenful_and_stops_at_the_top(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    let page = ed.read_with(cx, |e, _| e.active.grid.as_ref().unwrap().view.page_rows());
    assert!(page > 1, "a test viewport holds several rows");
    keys(&ed, cx, "ctrl-f ctrl-v");
    assert_eq!(cursor(&ed, cx), (page, 1), "C-v pages past the data");
    keys(&ed, cx, "pagedown");
    assert_eq!(cursor(&ed, cx), (2 * page, 1));
    ed.read_with(cx, |e, _| {
        assert!(e.active.grid.as_ref().unwrap().view.scroll_y > 0.0)
    });
    keys(&ed, cx, "alt-v pageup");
    assert_eq!(cursor(&ed, cx), (0, 1));
    keys(&ed, cx, "alt-v");
    assert_eq!(cursor(&ed, cx), (0, 1), "paging up at the top stays put");
    ed.read_with(cx, |e, _| {
        assert_eq!(e.active.grid.as_ref().unwrap().view.scroll_y, 0.0)
    });
}

#[gpui::test]
fn evil_half_pages_column_jumps_and_first_row(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    let page = ed.read_with(cx, |e, _| e.active.grid.as_ref().unwrap().view.page_rows());
    keys(&ed, cx, "ctrl-d");
    assert_eq!(cursor(&ed, cx), ((page / 2).max(1), 0));
    keys(&ed, cx, "ctrl-u ctrl-u");
    assert_eq!(cursor(&ed, cx), (0, 0), "C-u clamps at the top");
    keys(&ed, cx, "3 |");
    assert_eq!(cursor(&ed, cx), (0, 2), "3| = column C");
    keys(&ed, cx, "|");
    assert_eq!(cursor(&ed, cx), (0, 0), "| alone = column A");
    keys(&ed, cx, "l j j g g");
    assert_eq!(cursor(&ed, cx), (0, 1), "gg keeps the column");
    keys(&ed, cx, "ctrl-f");
    assert_eq!(cursor(&ed, cx), (page, 1));
    keys(&ed, cx, "ctrl-b");
    assert_eq!(cursor(&ed, cx), (0, 1));
}

// --- Selection extension --------------------------------------------------------

#[gpui::test]
fn emacs_shift_arrows_extend_and_a_plain_motion_collapses(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    keys(&ed, cx, "ctrl-n shift-down shift-right");
    assert_eq!(cursor(&ed, cx), (2, 1));
    assert_eq!(sel(&ed, cx), Some((1, 0, 2, 1)));
    keys(&ed, cx, "shift-up shift-up shift-up");
    assert_eq!(
        sel(&ed, cx),
        Some((0, 0, 1, 1)),
        "extending flips past the anchor and stops at row 1"
    );
    keys(&ed, cx, "shift-left shift-left");
    assert_eq!(sel(&ed, cx), Some((0, 0, 1, 0)), "and at column A");
    keys(&ed, cx, "ctrl-n");
    assert_eq!((cursor(&ed, cx), sel(&ed, cx)), ((1, 0), None));
}

#[gpui::test]
fn extending_past_the_data_and_cancelling(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    keys(&ed, cx, "alt-> shift-right shift-right shift-down");
    assert_eq!(
        sel(&ed, cx),
        Some((3, 2, 4, 4)),
        "selections reach past the data"
    );
    keys(&ed, cx, "ctrl-g");
    assert_eq!((cursor(&ed, cx), sel(&ed, cx)), ((4, 4), None));
    keys(&ed, cx, "shift-left escape");
    assert_eq!(sel(&ed, cx), None, "Escape cancels too");
}

#[gpui::test]
fn emacs_mark_makes_plain_motion_extend(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    keys(&ed, cx, "ctrl-n ctrl-space ctrl-n ctrl-f");
    assert_eq!(sel(&ed, cx), Some((1, 0, 2, 1)), "the mark sticks");
    keys(&ed, cx, "alt->");
    assert_eq!(
        sel(&ed, cx),
        Some((1, 0, 3, 2)),
        "jumps extend from the mark"
    );
    // C-x C-x twice is back where it started.
    keys(&ed, cx, "ctrl-x ctrl-x ctrl-x ctrl-x");
    assert_eq!(cursor(&ed, cx), (3, 2));
    assert_eq!(sel(&ed, cx), Some((1, 0, 3, 2)));
    // C-SPC again ends it.
    keys(&ed, cx, "ctrl-space");
    assert_eq!(sel(&ed, cx), None);
    keys(&ed, cx, "ctrl-p");
    assert_eq!(sel(&ed, cx), None);
    // With no selection, C-x C-x only says so.
    keys(&ed, cx, "ctrl-x ctrl-x");
    assert_eq!(cursor(&ed, cx), (2, 2));
    assert!(
        status(&ed, cx).contains("No selection"),
        "{}",
        status(&ed, cx)
    );
}

#[gpui::test]
fn evil_visual_kinds_switch_and_span_the_data(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "j l shift-v j");
    assert_eq!(sel(&ed, cx), Some((1, 0, 2, 2)), "V spans every column");
    // v from row-visual keeps the anchor and narrows to the rectangle.
    keys(&ed, cx, "v");
    assert_eq!(sel(&ed, cx), Some((1, 1, 2, 1)));
    keys(&ed, cx, "ctrl-v");
    assert_eq!(sel(&ed, cx), Some((0, 1, 3, 1)), "C-v spans every row");
    // The same kind again ends it.
    keys(&ed, cx, "ctrl-v");
    assert_eq!(sel(&ed, cx), None);
    // Visual sticks through jumps: v then G reaches the last row.
    keys(&ed, cx, "g g v shift-g $");
    assert_eq!(sel(&ed, cx), Some((0, 1, 3, 2)));
    keys(&ed, cx, "escape");
    assert_eq!(sel(&ed, cx), None);
}

#[gpui::test]
fn visual_clear_then_undo_restores_the_block(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "j v j l x");
    assert_eq!(row(&ed, cx, 1), ["", "", "red, crisp"]);
    assert_eq!(row(&ed, cx, 2), ["", "", ""]);
    keys(&ed, cx, "u");
    assert_eq!(row(&ed, cx, 1), ["apple", "3", "red, crisp"]);
    assert_eq!(row(&ed, cx, 2), ["pear", "007", ""]);
    assert!(!modified(&ed, cx));
}

// --- Rows and columns -------------------------------------------------------------

#[gpui::test]
fn inserting_rows_at_the_top_and_bottom(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "l shift-o");
    assert_eq!(cursor(&ed, cx), (0, 1), "the cursor sits in the new row");
    assert_eq!(row(&ed, cx, 0), ["", "", ""]);
    assert_eq!(cell(&ed, cx, 1, 0), "name");
    keys(&ed, cx, "u");
    assert_eq!(cell(&ed, cx, 0, 0), "name");
    assert!(!modified(&ed, cx));
    // Below the last row: a fresh row past the data, the cursor in it.
    keys(&ed, cx, "shift-g l o");
    assert_eq!(cursor(&ed, cx), (4, 1));
    assert_eq!(cell(&ed, cx, 3, 0), "fig");
    keys(&ed, cx, "c 9 escape");
    assert_eq!(row(&ed, cx, 4), ["", "9", ""]);
    assert_eq!(used(&ed, cx), (5, 3));
}

#[gpui::test]
fn inserting_or_deleting_past_the_data_changes_nothing(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "2 0 shift-g o");
    assert_eq!(cursor(&ed, cx), (20, 0), "the cursor still moves down");
    keys(&ed, cx, "d d");
    keys(&ed, cx, "2 0 | g o d c");
    assert_eq!(used(&ed, cx), (4, 3));
    assert!(!modified(&ed, cx), "nothing was edited");
    keys(&ed, cx, "u");
    assert!(
        status(&ed, cx).contains("No further undo"),
        "no undo steps were recorded: {}",
        status(&ed, cx)
    );
}

#[gpui::test]
fn inserting_columns_left_and_right_undo_and_redo(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "j g shift-o");
    assert_eq!(cursor(&ed, cx), (1, 0));
    assert_eq!(row(&ed, cx, 0), ["", "name", "qty", "note"]);
    keys(&ed, cx, "u");
    assert_eq!(row(&ed, cx, 0), ["name", "qty", "note"]);
    keys(&ed, cx, "ctrl-r");
    assert_eq!(row(&ed, cx, 0), ["", "name", "qty", "note"]);
    keys(&ed, cx, "u");
    // Right of the last column: a column past the data.
    keys(&ed, cx, "g g j $ g o");
    assert_eq!(cursor(&ed, cx), (1, 3));
    assert_eq!(row(&ed, cx, 1), ["apple", "3", "red, crisp"]);
    keys(&ed, cx, "c x escape");
    assert_eq!(row(&ed, cx, 1), ["apple", "3", "red, crisp", "x"]);
}

#[gpui::test]
fn emacs_structure_keys_insert_and_delete(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    keys(&ed, cx, "ctrl-n ctrl-c ctrl-o");
    assert_eq!(cursor(&ed, cx), (2, 0));
    assert_eq!(row(&ed, cx, 2), ["", "", ""]);
    assert_eq!(cell(&ed, cx, 3, 0), "pear");
    keys(&ed, cx, "ctrl-c alt-o");
    assert_eq!(cursor(&ed, cx), (2, 0), "above keeps the row index");
    assert_eq!(used(&ed, cx), (6, 3));
    keys(&ed, cx, "ctrl-c ctrl-k ctrl-c ctrl-k");
    assert_eq!(cell(&ed, cx, 2, 0), "pear");
    assert_eq!(used(&ed, cx), (4, 3));
    keys(&ed, cx, "ctrl-f ctrl-c ctrl-c");
    assert_eq!(cursor(&ed, cx), (2, 2));
    assert_eq!(row(&ed, cx, 0), ["name", "qty", "", "note"]);
    keys(&ed, cx, "ctrl-b ctrl-c alt-k");
    assert_eq!(row(&ed, cx, 0), ["name", "", "note"]);
    assert_eq!(cursor(&ed, cx), (2, 1));
    // Every step undoes on its own, back to the file as loaded.
    for _ in 0..6 {
        keys(&ed, cx, "ctrl-/");
    }
    assert_eq!(row(&ed, cx, 0), ["name", "qty", "note"]);
    assert_eq!(used(&ed, cx), (4, 3));
    assert!(!modified(&ed, cx));
    keys(&ed, cx, "ctrl-shift-/");
    assert_eq!(used(&ed, cx), (5, 3), "redo replays the first insert");
}

#[gpui::test]
fn deleting_a_selection_of_rows_or_columns_at_once(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "j l shift-v j d d");
    assert_eq!(used(&ed, cx).0, 2);
    assert_eq!(cell(&ed, cx, 1, 0), "fig");
    assert_eq!((cursor(&ed, cx), sel(&ed, cx)), ((1, 1), None));
    keys(&ed, cx, "h ctrl-v l d c");
    assert_eq!(row(&ed, cx, 0), ["note"]);
    assert_eq!(cursor(&ed, cx), (1, 0));
    // Each delete is one step, rows and columns alike.
    keys(&ed, cx, "u");
    assert_eq!(row(&ed, cx, 0), ["name", "qty", "note"]);
    assert_eq!(used(&ed, cx).0, 2);
    keys(&ed, cx, "u");
    assert_eq!(cell(&ed, cx, 2, 0), "pear");
    assert!(!modified(&ed, cx));
}

#[gpui::test]
fn deleting_the_last_row_and_column(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "shift-g d d");
    assert_eq!(used(&ed, cx), (3, 3));
    assert_eq!(
        cursor(&ed, cx),
        (3, 0),
        "the cursor stays where the row was"
    );
    // `$` on the empty row past the data is column A; from row 2 it's C.
    keys(&ed, cx, "k k $ d c");
    assert_eq!(used(&ed, cx), (3, 2));
    assert_eq!(
        cursor(&ed, cx),
        (1, 2),
        "the cursor stays where the column was"
    );
    keys(&ed, cx, "u u");
    assert_eq!(row(&ed, cx, 3), ["fig", "12", "ok"]);
}

#[gpui::test]
fn counted_row_inserts_and_deletes_are_one_step_each(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "evil");
    keys(&ed, cx, "j 3 o");
    assert_eq!(used(&ed, cx).0, 7);
    assert_eq!(cell(&ed, cx, 5, 0), "pear");
    assert_eq!(cursor(&ed, cx), (4, 0), "each o stepped down one");
    keys(&ed, cx, "u");
    assert_eq!(used(&ed, cx).0, 6, "a counted command undoes per repeat");
    keys(&ed, cx, "u u");
    assert_eq!(used(&ed, cx).0, 4);
    keys(&ed, cx, "g g j 2 d d");
    assert_eq!(cell(&ed, cx, 1, 0), "fig");
    keys(&ed, cx, "5 d d");
    assert_eq!(used(&ed, cx).0, 1, "a count past the data stops there");
    assert_eq!(cell(&ed, cx, 0, 0), "name");
}

#[gpui::test]
fn scheme_inserts_and_deletes_rows_and_columns(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    scheme(&ed, cx, "(grid-insert-rows 1 2)");
    assert_eq!(used(&ed, cx).0, 6);
    assert_eq!(cell(&ed, cx, 3, 0), "apple");
    scheme(&ed, cx, "(grid-delete-columns \"B\" 2)");
    assert_eq!(row(&ed, cx, 0), ["name"]);
    scheme(&ed, cx, "(grid-insert-columns 0 1)");
    assert_eq!(row(&ed, cx, 0), ["", "name"]);
    scheme(&ed, cx, "(grid-delete-rows 0 3)");
    assert_eq!(row(&ed, cx, 0), ["", "apple"]);
    // Bad indices are reported, and change nothing.
    scheme(&ed, cx, "(grid-delete-rows -1 1)");
    assert!(status(&ed, cx).contains("invalid"), "{}", status(&ed, cx));
    assert_eq!(used(&ed, cx), (3, 2));
    for _ in 0..4 {
        keys(&ed, cx, "ctrl-/");
    }
    assert_eq!(row(&ed, cx, 0), ["name", "qty", "note"]);
    assert_eq!(used(&ed, cx), (4, 3));
    assert!(!modified(&ed, cx));
}

// --- Workbook sheets ------------------------------------------------------------

/// `book.xlsx` beside `csv` with three sheets: First (A1 =Second!A1),
/// Second (A1 5), Third (A1 7).
fn open_book(cx: &mut TestAppContext) -> Entity<Editor> {
    use crate::xlsx::{WriteCell, XValue};
    let (ed, csv) = setup(cx, CSV, "emacs");
    let path = csv.with_file_name("book.xlsx");
    let (five, seven) = (XValue::Number(5.0), XValue::Number(7.0));
    let cell = |value, formula| WriteCell {
        row: 0,
        col: 0,
        value,
        formula,
        array: None,
        style: None,
    };
    let bytes = crate::xlsx::write_book(
        vec![
            ("First", vec![cell(&five, Some("Second!A1"))].into_iter()),
            ("Second", vec![cell(&five, None)].into_iter()),
            ("Third", vec![cell(&seven, None)].into_iter()),
        ],
        false,
    );
    std::fs::write(&path, bytes).unwrap();
    ed.update(cx, |e, cx| {
        e.open_grid(
            "book.xlsx".into(),
            path.clone(),
            std::fs::read(&path).unwrap(),
            cx,
        );
    });
    settle(&ed, cx);
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Second\" \"Third\")");
    ed
}

#[gpui::test]
fn sheet_names_are_checked_on_add_and_rename(cx: &mut TestAppContext) {
    let ed = open_book(cx);
    for bad in ["second", "a/b", "'quoted", &"x".repeat(32)] {
        scheme(&ed, cx, &format!("(grid-add-sheet-impl {bad:?})"));
        assert!(
            status(&ed, cx).starts_with("Not added"),
            "{bad}: {}",
            status(&ed, cx)
        );
        scheme(&ed, cx, &format!("(grid-rename-sheet {bad:?})"));
        assert!(
            status(&ed, cx).starts_with("Not renamed"),
            "{bad}: {}",
            status(&ed, cx)
        );
    }
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Second\" \"Third\")");
    assert!(!modified(&ed, cx));
    // Surrounding blanks are trimmed; a case-only change of its own name is
    // a rename.
    scheme(&ed, cx, "(grid-add-sheet-impl \"  Extra  \")");
    assert_eq!(active_name(&ed, cx), "book.xlsx<Extra>");
    scheme(&ed, cx, "(grid-rename-sheet \"EXTRA\")");
    assert_eq!(
        sheets(&ed, cx),
        "(\"First\" \"Second\" \"Third\" \"EXTRA\")"
    );
    scheme(&ed, cx, "(grid-rename-sheet \"EXTRA\")");
    assert!(
        status(&ed, cx).contains("already has that name"),
        "{}",
        status(&ed, cx)
    );
    scheme(&ed, cx, "(grid-rename-sheet \"Nope\" \"Other\")");
    assert!(
        status(&ed, cx).contains("No sheet named"),
        "{}",
        status(&ed, cx)
    );
}

#[gpui::test]
fn added_sheets_take_the_next_free_name_and_redo(cx: &mut TestAppContext) {
    let ed = open_book(cx);
    scheme(&ed, cx, "(grid-add-sheet-impl \"Sheet4\")");
    scheme(&ed, cx, "(grid-open-sheet \"First\")");
    scheme(&ed, cx, "(grid-add-sheet-impl \"\")");
    assert_eq!(active_name(&ed, cx), "book.xlsx<Sheet5>", "Sheet4 is taken");
    // Undo in the buffer it ran in, then redo there.
    scheme(&ed, cx, "(grid-open-sheet \"First\")");
    keys(&ed, cx, "ctrl-/");
    assert_eq!(
        sheets(&ed, cx),
        "(\"First\" \"Second\" \"Third\" \"Sheet4\")"
    );
    keys(&ed, cx, "ctrl-shift-/");
    assert_eq!(
        sheets(&ed, cx),
        "(\"First\" \"Second\" \"Third\" \"Sheet4\" \"Sheet5\")"
    );
}

#[gpui::test]
fn rename_undo_and_redo_follow_references(cx: &mut TestAppContext) {
    let ed = open_book(cx);
    scheme(&ed, cx, "(grid-rename-sheet \"Second\" \"Costs\")");
    let formula = |ed: &Entity<Editor>, cx: &mut TestAppContext| {
        ed.read_with(cx, |e, _| {
            let g = e.active.grid.as_ref().unwrap();
            crate::formula::engine::display_text(g.wb.cell(super::grid::SHEET, 0, 0)).to_string()
        })
    };
    assert_eq!(formula(&ed, cx), "=Costs!A1");
    keys(&ed, cx, "ctrl-/");
    assert_eq!(formula(&ed, cx), "=Second!A1");
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Second\" \"Third\")");
    keys(&ed, cx, "ctrl-shift-/");
    assert_eq!(formula(&ed, cx), "=Costs!A1");
    assert_eq!(cell(&ed, cx, 0, 0), "5", "the reference still resolves");
}

#[gpui::test]
fn moving_sheets_clamps_at_either_end(cx: &mut TestAppContext) {
    let ed = open_book(cx);
    keys(&ed, cx, "ctrl-c shift-t b");
    assert!(
        status(&ed, cx).contains("already at that end"),
        "{}",
        status(&ed, cx)
    );
    assert!(!modified(&ed, cx));
    scheme(&ed, cx, "(grid-move-sheet 10)");
    assert_eq!(sheets(&ed, cx), "(\"Second\" \"Third\" \"First\")");
    keys(&ed, cx, "ctrl-c shift-t f");
    assert!(status(&ed, cx).contains("already at that end"));
    scheme(&ed, cx, "(grid-move-sheet -10)");
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Second\" \"Third\")");
    keys(&ed, cx, "ctrl-/");
    assert_eq!(sheets(&ed, cx), "(\"Second\" \"Third\" \"First\")");
    keys(&ed, cx, "ctrl-/");
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Second\" \"Third\")");
    // The last sheet moves left from its own buffer.
    scheme(&ed, cx, "(grid-open-sheet \"Third\")");
    keys(&ed, cx, "ctrl-c shift-t b");
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Third\" \"Second\")");
}

#[gpui::test]
fn deleting_the_last_sheet_shown_moves_to_the_one_before(cx: &mut TestAppContext) {
    let ed = open_book(cx);
    scheme(&ed, cx, "(grid-open-sheet \"Third\")");
    scheme(&ed, cx, "(grid-delete-sheet-impl \"\")");
    assert_eq!(active_name(&ed, cx), "book.xlsx<Second>");
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Second\")");
    scheme(&ed, cx, "(grid-delete-sheet \"Nope\")");
    assert!(
        status(&ed, cx).contains("No sheet named"),
        "{}",
        status(&ed, cx)
    );
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Second\")");
    keys(&ed, cx, "ctrl-/");
    assert_eq!(sheets(&ed, cx), "(\"First\" \"Second\" \"Third\")");
}

#[gpui::test]
fn a_csv_has_no_sheets_to_rename_delete_or_move(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "emacs");
    scheme(&ed, cx, "(grid-rename-sheet \"Other\")");
    assert!(
        status(&ed, cx).contains("no sheet names"),
        "{}",
        status(&ed, cx)
    );
    scheme(&ed, cx, "(grid-delete-sheet \"Sheet1\")");
    assert!(
        status(&ed, cx).contains("only one sheet"),
        "{}",
        status(&ed, cx)
    );
    scheme(&ed, cx, "(grid-move-sheet 1)");
    assert!(
        status(&ed, cx).contains("only one sheet"),
        "{}",
        status(&ed, cx)
    );
    assert!(!modified(&ed, cx));
    assert_eq!(sheets(&ed, cx), "()");
}

// --- Multiple cursors ---------------------------------------------------------------

#[gpui::test]
fn adding_a_cursor_above_the_top_row_is_refused(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "helix");
    keys(&ed, cx, "alt-shift-c");
    assert!(
        status(&ed, cx).contains("No row there"),
        "{}",
        status(&ed, cx)
    );
    assert!(extra_cursors(&ed, cx).is_empty());
    // Removing or rotating with one cursor is refused too.
    keys(&ed, cx, "alt-,");
    assert!(
        status(&ed, cx).contains("Only one cursor"),
        "{}",
        status(&ed, cx)
    );
    keys(&ed, cx, ")");
    assert!(status(&ed, cx).contains("Only one cursor"));
    assert_eq!(cursor(&ed, cx), (0, 0));
}

#[gpui::test]
fn added_cursors_merge_instead_of_doubling(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "helix");
    keys(&ed, cx, "j shift-c shift-c");
    assert_eq!(cursor(&ed, cx), (3, 0));
    assert_eq!(extra_cursors(&ed, cx), [(1, 0), (2, 0)]);
    // Copying all three up lands two copies on existing cursors.
    keys(&ed, cx, "alt-shift-c");
    assert_eq!(cursor(&ed, cx), (2, 0), "the primary's copy is the primary");
    assert_eq!(extra_cursors(&ed, cx), [(0, 0), (1, 0), (3, 0)]);
    // Motion against the top edge merges cursors that meet.
    keys(&ed, cx, "k k k");
    assert_eq!(cursor(&ed, cx), (0, 0));
    assert!(
        extra_cursors(&ed, cx).is_empty(),
        "{:?}",
        extra_cursors(&ed, cx)
    );
    assert_eq!(sel(&ed, cx), None, "no column selected by a multi-cursor k");
}

#[gpui::test]
fn removing_and_rotating_the_primary_cursor(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "helix");
    keys(&ed, cx, "l shift-c shift-c");
    assert_eq!(cursor(&ed, cx), (2, 1));
    keys(&ed, cx, ")");
    assert_eq!(
        cursor(&ed, cx),
        (0, 1),
        "rotating forward wraps to the first"
    );
    keys(&ed, cx, "(");
    assert_eq!(cursor(&ed, cx), (2, 1), "and backward wraps to the last");
    keys(&ed, cx, "alt-,");
    assert_eq!(
        cursor(&ed, cx),
        (1, 1),
        "the last gone: the one before leads"
    );
    assert_eq!(extra_cursors(&ed, cx), [(0, 1)]);
    keys(&ed, cx, "( alt-,");
    assert_eq!(cursor(&ed, cx), (1, 1), "the first gone: the next leads");
    assert!(extra_cursors(&ed, cx).is_empty());
}

#[gpui::test]
fn enter_and_tab_commits_move_every_cursor(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "helix");
    keys(&ed, cx, "j l shift-c c x enter");
    assert_eq!(
        (cell(&ed, cx, 1, 1), cell(&ed, cx, 2, 1)),
        ("x".into(), "x".into())
    );
    assert_eq!(cursor(&ed, cx), (3, 1));
    assert_eq!(extra_cursors(&ed, cx), [(2, 1)]);
    keys(&ed, cx, "c y tab");
    assert_eq!(
        (cell(&ed, cx, 2, 1), cell(&ed, cx, 3, 1)),
        ("y".into(), "y".into())
    );
    assert_eq!(cursor(&ed, cx), (3, 2));
    assert_eq!(extra_cursors(&ed, cx), [(2, 2)]);
}

#[gpui::test]
fn structural_commands_keep_only_the_primary_cursor(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "helix");
    keys(&ed, cx, "j shift-c g d");
    assert!(extra_cursors(&ed, cx).is_empty());
    assert_eq!(used(&ed, cx).0, 3, "only the primary's row went");
    assert_eq!(cell(&ed, cx, 1, 0), "apple");
    assert_eq!(cell(&ed, cx, 2, 0), "fig");
    keys(&ed, cx, "u");
    assert_eq!(cell(&ed, cx, 2, 0), "pear");
    // A jump drops them as well.
    keys(&ed, cx, "g g shift-c g e");
    assert!(extra_cursors(&ed, cx).is_empty());
    assert_eq!(cursor(&ed, cx), (3, 0));
}

#[gpui::test]
fn select_matching_reports_misses_and_bad_patterns(cx: &mut TestAppContext) {
    let (ed, _) = setup(cx, CSV, "helix");
    keys(&ed, cx, "s");
    submit(&ed, cx, "zzz");
    assert!(
        status(&ed, cx).contains("No cell matches"),
        "{}",
        status(&ed, cx)
    );
    assert!(extra_cursors(&ed, cx).is_empty());
    keys(&ed, cx, "s");
    submit(&ed, cx, "(");
    assert!(status(&ed, cx).contains("Bad regex"), "{}", status(&ed, cx));
    // Within a selected row only.
    keys(&ed, cx, "j x s");
    submit(&ed, cx, "^[0-9a-z]");
    assert_eq!(cursor(&ed, cx), (1, 0));
    assert_eq!(extra_cursors(&ed, cx), [(1, 1), (1, 2)]);
    assert!(status(&ed, cx).contains("3 cursors"), "{}", status(&ed, cx));
}

#[gpui::test]
fn emacs_and_evil_drive_multi_cursors_through_commands(cx: &mut TestAppContext) {
    // No keys bind them outside helix, but the commands work everywhere.
    let (ed, _) = setup(cx, "k,v\n1,x\n2,y\n3,z\n", "emacs");
    keys(&ed, cx, "ctrl-n ctrl-f");
    scheme(&ed, cx, "(grid-add-cursor-below)");
    scheme(&ed, cx, "(grid-add-cursor-below)");
    assert_eq!(cursor(&ed, cx), (3, 1));
    assert_eq!(extra_cursors(&ed, cx), [(1, 1), (2, 1)]);
    // Typing over the cell replaces every cursor's cell, as one step.
    keys(&ed, cx, "n enter");
    assert_eq!(
        (1..4).map(|r| cell(&ed, cx, r, 1)).collect::<Vec<_>>(),
        ["n", "n", "n"]
    );
    keys(&ed, cx, "ctrl-/");
    assert_eq!(
        (1..4).map(|r| cell(&ed, cx, r, 1)).collect::<Vec<_>>(),
        ["x", "y", "z"]
    );
    // Under evil, x clears every cursor's cell.
    let (ed, _) = setup(cx, "k,v\n1,x\n2,y\n3,z\n", "evil");
    keys(&ed, cx, "j l");
    scheme(&ed, cx, "(grid-add-cursor-below)");
    keys(&ed, cx, "x");
    assert_eq!(
        (1..4).map(|r| cell(&ed, cx, r, 1)).collect::<Vec<_>>(),
        ["", "", "z"]
    );
    keys(&ed, cx, "j");
    assert_eq!(cursor(&ed, cx), (3, 1));
    assert_eq!(extra_cursors(&ed, cx), [(2, 1)], "motions move them all");
    scheme(&ed, cx, "(grid-keep-primary-cursor)");
    assert!(extra_cursors(&ed, cx).is_empty());
}
