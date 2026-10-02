//! End-to-end evil tests: a real `Editor` on gpui's headless test platform,
//! the bundled Scheme loaded, keys fed through the same layers as typing.
//! Each key waits for the Scheme engine to drain (a blocking eval queues
//! behind everything already dispatched) and for the outcomes to apply.

use gpui::{Entity, Keystroke, TestAppContext};

use super::*;

fn setup(cx: &mut TestAppContext, text: &str) -> Entity<Editor> {
    cx.update(|cx| {
        cx.set_global(crate::theme::Theme::default());
        cx.set_global(crate::settings::Settings::default());
    });
    let editor = cx.new(Editor::new);
    editor.update(cx, |e, cx| {
        for src in crate::runtime::startup_scripts() {
            e.run_script_blocking(src, cx);
        }
        e.run_script_blocking("(evil-mode)", cx);
        e.mark_config_loaded(cx);
        // A plain-text buffer with the test's contents.
        let current = e.snapshot_active();
        e.inactive.push(current);
        e.install_active(make_document("test.txt".into(), None, text));
        e.evil = EvilState::Normal;
    });
    settle(&editor, cx);
    editor
}

/// Let every dispatched Scheme command run and its commands apply (replays
/// chain several rounds).
fn settle(editor: &Entity<Editor>, cx: &mut TestAppContext) {
    for _ in 0..12 {
        cx.run_until_parked();
        editor.read_with(cx, |e, _| {
            let _ = e.script.eval("0");
        });
        cx.run_until_parked();
    }
}

fn keystroke(k: &str) -> Keystroke {
    if k.chars().count() == 1 {
        super::ex::token_keystroke(k).expect("printable key")
    } else {
        Keystroke::parse(k).expect("keystroke")
    }
}

/// Feed keys: space-separated tokens (`"d 2 w"`, `"ctrl-w l"`), each a
/// single char or a gpui keystroke name. In Insert state printable keys type.
fn keys(editor: &Entity<Editor>, cx: &mut TestAppContext, seq: &str) {
    for k in seq.split(' ').filter(|k| !k.is_empty()) {
        let k = if k == "SPC" { " " } else { k };
        let ks = keystroke(k);
        editor.update(cx, |e, cx| {
            // A pending one-key capture (`C-r x`) sees the key first, as it
            // does for a real keystroke; otherwise Insert-state text types.
            let capturing = e.vim.capture.is_some() || e.read_char_pending.is_some();
            if e.evil == EvilState::Insert
                && !capturing
                && !ks.modifiers.control
                && k.chars().count() == 1
            {
                e.type_text(None, k, cx);
            } else {
                e.dispatch_keystroke(&ks, cx);
            }
        });
        settle(editor, cx);
    }
}

fn text(editor: &Entity<Editor>, cx: &mut TestAppContext) -> String {
    editor.read_with(cx, |e, _| e.active.buffer.full_text())
}

fn point(editor: &Entity<Editor>, cx: &mut TestAppContext) -> usize {
    editor.read_with(cx, |e, _| e.point())
}

fn ex(editor: &Entity<Editor>, cx: &mut TestAppContext, line: &str) {
    editor.update(cx, |e, cx| e.run_ex_command(line, cx));
    settle(editor, cx);
}

#[gpui::test]
fn operators_counts_and_dot(cx: &mut TestAppContext) {
    let ed = setup(cx, "one two three four five six\n");
    keys(&ed, cx, "d w");
    assert_eq!(text(&ed, cx), "two three four five six\n");
    keys(&ed, cx, "2 d 2 w");
    assert_eq!(text(&ed, cx), "six\n");
    // dot repeats the last change, 3. with a new count.
    let ed = setup(cx, "abcdefgh\n");
    keys(&ed, cx, "x");
    keys(&ed, cx, ".");
    assert_eq!(text(&ed, cx), "cdefgh\n");
    keys(&ed, cx, "3 .");
    assert_eq!(text(&ed, cx), "fgh\n");
    // cw + typed text + Esc, then . on the next word; u undoes one change.
    let ed = setup(cx, "foo bar baz\n");
    keys(&ed, cx, "c w X Y escape");
    assert_eq!(text(&ed, cx), "XY bar baz\n");
    keys(&ed, cx, "w .");
    assert_eq!(text(&ed, cx), "XY XY baz\n");
    keys(&ed, cx, "u");
    assert_eq!(text(&ed, cx), "XY bar baz\n");
    keys(&ed, cx, "u");
    assert_eq!(text(&ed, cx), "foo bar baz\n");
    // dd / p, and dot of p.
    let ed = setup(cx, "a\nb\nc\n");
    keys(&ed, cx, "d d p");
    assert_eq!(text(&ed, cx), "b\na\nc\n");
    // Visual operator, gUiw, >j, dfx, dt with count.
    let ed = setup(cx, "hello world\n");
    keys(&ed, cx, "g shift-u i w");
    assert_eq!(text(&ed, cx), "HELLO world\n");
    let ed = setup(cx, "a.b.c.d\n");
    keys(&ed, cx, "d 2 f .");
    assert_eq!(text(&ed, cx), "c.d\n");
    // r is repeatable.
    let ed = setup(cx, "abcd\n");
    keys(&ed, cx, "r x l .");
    assert_eq!(text(&ed, cx), "xxcd\n");
    // A failed motion cancels: dh at column 0.
    let ed = setup(cx, "abc\n");
    keys(&ed, cx, "d h");
    assert_eq!(text(&ed, cx), "abc\n");
}

#[gpui::test]
fn registers_and_macros(cx: &mut TestAppContext) {
    let ed = setup(cx, "one\ntwo\n");
    keys(&ed, cx, "\" a y y j \" a p");
    assert_eq!(text(&ed, cx), "one\ntwo\none\n");
    // Black hole: the deleted line doesn't replace the unnamed register.
    let ed = setup(cx, "keep\ndrop\n");
    keys(&ed, cx, "y y j \" _ d d");
    assert_eq!(text(&ed, cx), "keep\n");
    keys(&ed, cx, "p");
    assert_eq!(text(&ed, cx), "keep\nkeep\n");
    // Macros in registers: qa A ; Esc j q, then @a and @@.
    let ed = setup(cx, "a\nb\nc\nd\n");
    keys(&ed, cx, "q a shift-a ; escape j q");
    assert_eq!(text(&ed, cx), "a;\nb\nc\nd\n");
    keys(&ed, cx, "@ a");
    assert_eq!(text(&ed, cx), "a;\nb;\nc\nd\n");
    keys(&ed, cx, "@ @");
    assert_eq!(text(&ed, cx), "a;\nb;\nc;\nd\n");
}

#[gpui::test]
fn marks_jumps_and_windows(cx: &mut TestAppContext) {
    let ed = setup(cx, "l1\nl2\nl3\nl4\n");
    keys(&ed, cx, "j m a shift-g");
    assert_eq!(point(&ed, cx), 9);
    keys(&ed, cx, "' a");
    assert_eq!(point(&ed, cx), 3);
    keys(&ed, cx, "' '");
    assert_eq!(point(&ed, cx), 9);
    // Marks follow edits: delete the first line, 'a still finds l2.
    keys(&ed, cx, "g g d d ' a");
    assert_eq!(point(&ed, cx), 0);
    // d'a — linewise to the mark.
    let ed = setup(cx, "l1\nl2\nl3\nl4\n");
    keys(&ed, cx, "j j m a g g d ' a");
    assert_eq!(text(&ed, cx), "l4\n");
    // The jump list: G then C-o comes back, C-i goes again.
    let ed = setup(cx, "l1\nl2\nl3\nl4\n");
    keys(&ed, cx, "shift-g");
    assert_eq!(point(&ed, cx), 9);
    keys(&ed, cx, "ctrl-o");
    assert_eq!(point(&ed, cx), 0);
    keys(&ed, cx, "ctrl-i");
    assert_eq!(point(&ed, cx), 9);
    // Windows: C-w v splits, C-w l / C-w h move between them.
    let first = ed.read_with(cx, |e, _| e.selected_pane);
    keys(&ed, cx, "ctrl-w v");
    let mut leaves = Vec::new();
    ed.read_with(cx, |e, _| layout_leaves(&e.layout, &mut leaves));
    assert_eq!(leaves.len(), 2);
    keys(&ed, cx, "ctrl-w l");
    let right = ed.read_with(cx, |e, _| e.selected_pane);
    assert_ne!(right, first);
    keys(&ed, cx, "ctrl-w h");
    assert_eq!(ed.read_with(cx, |e, _| e.selected_pane), first);
    keys(&ed, cx, "ctrl-w o");
    let mut leaves = Vec::new();
    ed.read_with(cx, |e, _| layout_leaves(&e.layout, &mut leaves));
    assert_eq!(leaves.len(), 1);
}

#[gpui::test]
fn ex_commands(cx: &mut TestAppContext) {
    let ed = setup(cx, "a x\nb x x\nc\nd x\n");
    ex(&ed, cx, "%s/x/y/g");
    assert_eq!(text(&ed, cx), "a y\nb y y\nc\nd y\n");
    ex(&ed, cx, "2,3d");
    assert_eq!(text(&ed, cx), "a y\nd y\n");
    ex(&ed, cx, "$m0");
    assert_eq!(text(&ed, cx), "d y\na y\n");
    ex(&ed, cx, "1t$");
    assert_eq!(text(&ed, cx), "d y\na y\nd y\n");
    ex(&ed, cx, "g/^d/d");
    assert_eq!(text(&ed, cx), "a y\n");
    let ed = setup(cx, "b\na\nc\n");
    ex(&ed, cx, "sort");
    assert_eq!(text(&ed, cx), "a\nb\nc\n");
    ex(&ed, cx, "%norm A!");
    assert_eq!(text(&ed, cx), "a!\nb!\nc!\n");
    ex(&ed, cx, "1,2j");
    assert_eq!(text(&ed, cx), "a! b!\nc!\n");
    ex(&ed, cx, "g/c/s/!/?/");
    assert_eq!(text(&ed, cx), "a! b!\nc?\n");
    // & repeats the last :s on the current line.
    let ed = setup(cx, "foo foo\nfoo\n");
    ex(&ed, cx, "s/foo/bar/");
    keys(&ed, cx, "j &");
    assert_eq!(text(&ed, cx), "bar foo\nbar\n");
    // :s///c — y, n, then q.
    let ed = setup(cx, "x x x\n");
    ex(&ed, cx, "s/x/y/gc");
    keys(&ed, cx, "y n y");
    assert_eq!(text(&ed, cx), "y x y\n");
}

/// Type into the open minibuffer and submit it (RET is a gpui action).
fn minibuffer(editor: &Entity<Editor>, cx: &mut TestAppContext, input: &str) {
    editor.update(cx, |e, cx| {
        e.type_text(None, input, cx);
        e.mb_submit(cx);
    });
    settle(editor, cx);
}

fn setup_file(cx: &mut TestAppContext, name: &str, text: &str) -> Entity<Editor> {
    let ed = setup(cx, "");
    ed.update(cx, |e, _cx| {
        let current = e.snapshot_active();
        e.inactive.push(current);
        let path = std::path::PathBuf::from(format!("/nonexistent-macros-test/{name}"));
        e.install_active(make_document(name.into(), Some(path), text));
        e.parse_full();
        e.evil = EvilState::Normal;
    });
    settle(&ed, cx);
    ed
}

#[gpui::test]
fn more_changes_and_repeats(cx: &mut TestAppContext) {
    // o / A / J / >> / ~ with dot.
    let ed = setup(cx, "a\nb\n");
    keys(&ed, cx, "o x escape j .");
    assert_eq!(text(&ed, cx), "a\nx\nb\nx\n");
    let ed = setup(cx, "a\nb\n");
    keys(&ed, cx, "shift-a ; escape j .");
    assert_eq!(text(&ed, cx), "a;\nb;\n");
    let ed = setup(cx, "a\nb\nc\nd\n");
    keys(&ed, cx, "shift-j j .");
    assert_eq!(text(&ed, cx), "a b\nc d\n");
    let ed = setup(cx, "a\nb\n");
    keys(&ed, cx, "> > j .");
    let t = text(&ed, cx);
    assert!(t.starts_with(' ') && t.contains("\n "), "{t:?}");
    let ed = setup(cx, "abc\n");
    keys(&ed, cx, "~ .");
    assert_eq!(text(&ed, cx), "ABc\n");
    // ci" + text, then . inside another string.
    let ed = setup(cx, "x \"aa\" \"bb\"\n");
    keys(&ed, cx, "f a c i \" Z escape f b .");
    assert_eq!(text(&ed, cx), "x \"Z\" \"Z\"\n");
    // cc keeps one (empty) line and inserts there.
    let ed = setup(cx, "one\ntwo\n");
    keys(&ed, cx, "c c N E W escape");
    assert_eq!(text(&ed, cx), "NEW\ntwo\n");
    // Visual delete, then . deletes the same-shaped selection again.
    let ed = setup(cx, "abcdef\n");
    keys(&ed, cx, "v l d");
    assert_eq!(text(&ed, cx), "cdef\n");
    keys(&ed, cx, ".");
    assert_eq!(text(&ed, cx), "ef\n");
    // C-a increments with a count; . repeats it.
    let ed = setup(cx, "n = 7;\n");
    keys(&ed, cx, "5 ctrl-a");
    assert_eq!(text(&ed, cx), "n = 12;\n");
    keys(&ed, cx, ".");
    assert_eq!(text(&ed, cx), "n = 17;\n");
    // gc{motion} comments via the language leader, gcc toggles back.
    let ed = setup_file(cx, "t.py", "x = 1\ny = 2\n");
    keys(&ed, cx, "g c j");
    assert_eq!(text(&ed, cx), "# x = 1\n# y = 2\n");
    keys(&ed, cx, "g c c");
    assert_eq!(text(&ed, cx), "x = 1\n# y = 2\n");
}

#[gpui::test]
fn registers_more(cx: &mut TestAppContext) {
    // "A appends; "0 survives a delete; C-r in Insert.
    let ed = setup(cx, "one\ntwo\nthree\n");
    keys(&ed, cx, "\" a y y j \" A y y");
    keys(&ed, cx, "shift-g \" a p");
    assert_eq!(text(&ed, cx), "one\ntwo\nthree\none\ntwo\n");
    let ed = setup(cx, "keep\ngone\n");
    keys(&ed, cx, "y w j d d \" 0 shift-p");
    assert_eq!(text(&ed, cx), "keepkeep\n");
    let ed = setup(cx, "word\n");
    keys(&ed, cx, "\" b y i w shift-a SPC ctrl-r b escape");
    assert_eq!(text(&ed, cx), "word word\n");
    // 3@a.
    let ed = setup(cx, "1\n2\n3\n4\n5\n");
    keys(&ed, cx, "q a shift-a . escape j q 3 @ a");
    assert_eq!(text(&ed, cx), "1.\n2.\n3.\n4.\n5\n");
}

#[gpui::test]
fn search_jumps_visual_ex_and_tree_sitter(cx: &mut TestAppContext) {
    // / search then C-o back.
    let ed = setup(cx, "aa\nbb\ncc\n");
    keys(&ed, cx, "/");
    minibuffer(&ed, cx, "cc");
    assert_eq!(point(&ed, cx), 6);
    keys(&ed, cx, "ctrl-o");
    assert_eq!(point(&ed, cx), 0);
    // d/pat — the search motion of an operator.
    let ed = setup(cx, "one two three\n");
    keys(&ed, cx, "d /");
    minibuffer(&ed, cx, "three");
    assert_eq!(text(&ed, cx), "three\n");
    // : from Visual Line runs on the selection.
    let ed = setup(cx, "x\nx\nx\n");
    keys(&ed, cx, "j shift-v j :");
    minibuffer(&ed, cx, "s/x/y/");
    assert_eq!(text(&ed, cx), "x\ny\ny\n");
    // :g/…/normal.
    let ed = setup(cx, "a\nb\na\n");
    ex(&ed, cx, "g/a/normal Az");
    assert_eq!(text(&ed, cx), "az\nb\naz\n");
    // Tree-sitter: ]m motion, daf on a Rust function, cia.
    let src = "fn a(x: i32, y: i32) {\n    x;\n}\n\nfn b() {}\n";
    let ed = setup_file(cx, "t.rs", src);
    keys(&ed, cx, "] m");
    assert_eq!(point(&ed, cx), src.find("fn b").unwrap());
    let ed = setup_file(cx, "t.rs", src);
    keys(&ed, cx, "j d a f");
    assert_eq!(text(&ed, cx), "\nfn b() {}\n");
    let ed = setup_file(cx, "t.rs", src);
    keys(&ed, cx, "f y c i a z escape");
    assert!(
        text(&ed, cx).starts_with("fn a(x: i32, z) {"),
        "{:?}",
        text(&ed, cx)
    );
    // Windows: C-w s then C-w j / k / p.
    let ed = setup(cx, "x\n");
    let top = ed.read_with(cx, |e, _| e.selected_pane);
    keys(&ed, cx, "ctrl-w s ctrl-w j");
    let bottom = ed.read_with(cx, |e, _| e.selected_pane);
    assert_ne!(top, bottom);
    keys(&ed, cx, "ctrl-w k");
    assert_eq!(ed.read_with(cx, |e, _| e.selected_pane), top);
    keys(&ed, cx, "ctrl-w p");
    assert_eq!(ed.read_with(cx, |e, _| e.selected_pane), bottom);
}
