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

#[gpui::test]
fn classic_commands_still_work(cx: &mut TestAppContext) {
    let cases: &[(&str, usize, &str, &str)] = &[
        // (text, point, keys, expected)
        ("foo bar baz\n", 5, "c i w X escape w .", "foo X X\n"),
        ("a b c\n", 0, "d a w .", "c\n"),
        ("p1\np1\n\np2\n", 0, "d a p", "p2\n"),
        ("x\ny\n", 0, "y i p shift-g p", "x\ny\nx\ny\n"),
        ("abc\ndef\n", 1, "d v j", "aef\n"),
        ("Hello\n", 0, "g shift-u shift-u", "HELLO\n"),
        ("Hello\n", 0, "g u u", "hello\n"),
        ("Hello\n", 0, "g ~ ~", "hELLO\n"),
        ("a\nb\nc\nd\n", 0, "2 c c Z escape", "Z\nc\nd\n"),
        ("abc def\n", 4, "shift-d", "abc \n"),
        ("abc def\n", 4, "shift-c Z escape", "abc Z\n"),
        ("abc\n", 0, "s Z escape", "Zbc\n"),
        ("abc\n", 2, "x x", "a\n"),
        ("one two\n", 0, "y w shift-p", "one one two\n"),
        ("a\nb\nc\nd\n", 0, "3 shift-j", "a b c\nd\n"),
        ("x1\nx2\nx3\n", 0, "d 2 j", ""),
        ("x1\nx2\nx3\n", 3, "d k", "x3\n"),
        ("  a\nb\n", 3, "= j", "  a\n  b\n"),
        ("a b\n", 0, "> > .", "        a b\n"),
        ("one two three\n", 0, "d t h", "hree\n"),
        ("one two three\n", 0, "d e .", " three\n"),
        ("one two three\n", 8, "d b", "one three\n"),
        ("one two three\n", 12, "d 0", "e\n"),
        ("    abc\n", 6, "d ^", "    c\n"),
        ("(a (b) c)\n", 4, "d i (", "(a () c)\n"),
        ("(a (b) c)\n", 4, "c a ( Z escape", "(a Z c)\n"),
        ("f(x, y)\n", 0, "d %", "\n"),
    ];
    for (text_in, pt, seq, want) in cases {
        let ed = setup(cx, text_in);
        ed.update(cx, |e, _| e.collapse_to(*pt));
        keys(&ed, cx, seq);
        assert_eq!(&text(&ed, cx), want, "{seq:?} on {text_in:?}");
    }
}

#[gpui::test]
fn marks_search_and_ex_extras(cx: &mut TestAppContext) {
    // Exact mark jump and a marked range in ex.
    let ed = setup(cx, "l1\nl2 xy\nl3\nl4\nl5\n");
    keys(&ed, cx, "j l l l m a j j m b g g ` a");
    assert_eq!(point(&ed, cx), 6);
    ex(&ed, cx, "'a,'bd");
    assert_eq!(text(&ed, cx), "l1\nl5\n");
    // dn after a search; @: repeats the last ex command.
    let ed = setup(cx, "a x b x c x\n");
    keys(&ed, cx, "/");
    minibuffer(&ed, cx, "x");
    keys(&ed, cx, "0 d n");
    assert_eq!(text(&ed, cx), "x b x c x\n");
    let ed = setup(cx, "a\na\na\n");
    ex(&ed, cx, "s/a/b/");
    keys(&ed, cx, "j @ :");
    assert_eq!(text(&ed, cx), "b\nb\na\n");
    // :s with groups and &.
    let ed = setup(cx, "key=val\n");
    ex(&ed, cx, r"s/(\w+)=(\w+)/\2=\1 [&]/");
    assert_eq!(text(&ed, cx), "val=key [key=val]\n");
    // :g!/pat/d and :v.
    let ed = setup(cx, "keep 1\ndrop\nkeep 2\n");
    ex(&ed, cx, "v/keep/d");
    assert_eq!(text(&ed, cx), "keep 1\nkeep 2\n");
    // Undo of a :g is one step.
    keys(&ed, cx, "u");
    assert_eq!(text(&ed, cx), "keep 1\ndrop\nkeep 2\n");
    // :{n} goes to a line; :$ the last; jump list records it.
    ex(&ed, cx, "2");
    assert_eq!(point(&ed, cx), 7);
    // Visual block delete still works.
    let ed = setup(cx, "abc\ndef\n");
    keys(&ed, cx, "ctrl-v j l d");
    assert_eq!(text(&ed, cx), "c\nf\n");
}

/// The MCP server, end to end: options start it, and over real HTTP an
/// `eval` edits the buffer and reports its value and messages, which
/// `buffer_text` then sees. Turning the option off stops it.
#[gpui::test]
fn mcp_server_eval_and_buffer_text(cx: &mut TestAppContext) {
    use std::io::{Read as _, Write as _};

    let editor = setup(cx, "");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    editor.update(cx, |e, cx| {
        e.run_script_blocking(
            &format!(
                r#"(set-option "mcp-server-token" "test-token")
                   (set-option "mcp-server-port" {port})
                   (set-option "mcp-server" #t)"#
            ),
            cx,
        )
    });
    settle(&editor, cx);
    assert!(
        editor.read_with(cx, |e, _| e.mcp.is_some()),
        "server should be running"
    );

    // POST one tools/call from a worker thread, pumping the editor's executor
    // until it answers (the server blocks on foreground replies).
    let call = |cx: &mut TestAppContext,
                tool: &str,
                args: serde_json::Value|
     -> serde_json::Value {
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                      "params": {"name": tool, "arguments": args}})
        .to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                s,
                "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer test-token\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            tx.send(out).unwrap();
        });
        for _ in 0..1000 {
            cx.run_until_parked();
            if let Ok(out) = rx.try_recv() {
                let json = out.split("\r\n\r\n").nth(1).unwrap();
                return serde_json::from_str(json).unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("no MCP response");
    };
    let reply = |r: &serde_json::Value| {
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let r = call(
        cx,
        "eval",
        serde_json::json!({"code": r#"(insert "hi from mcp") (message "inserted") (+ 40 2)"#}),
    );
    let out = reply(&r);
    assert!(out.starts_with("=> 42"), "{out}");
    assert!(out.contains("inserted"), "{out}");
    assert_eq!(text(&editor, cx), "hi from mcp");

    let r = call(cx, "buffer_text", serde_json::json!({}));
    let out = reply(&r);
    assert!(
        out.contains("buffer: test.txt") && out.ends_with("---\nhi from mcp"),
        "{out}"
    );

    let r = call(cx, "eval", serde_json::json!({"code": "(car '())"}));
    assert_eq!(r["result"]["isError"], true, "{r}");

    let r = call(
        cx,
        "describe",
        serde_json::json!({"name": "plot-sparkline"}),
    );
    assert!(reply(&r).starts_with("plot-sparkline"), "{r}");

    editor.update(cx, |e, cx| {
        e.run_script_blocking(r#"(set-option "mcp-server" #f)"#, cx)
    });
    settle(&editor, cx);
    assert!(
        editor.read_with(cx, |e, _| e.mcp.is_none()),
        "server should stop"
    );
}

/// Refreshing a special buffer in place keeps the view where the user left
/// it, even when they wheel-scrolled away from point (dashboards that
/// auto-refresh used to snap back to the cursor on every redraw).
#[gpui::test]
fn render_buffer_refresh_keeps_scroll_position(cx: &mut TestAppContext) {
    let ed = setup(cx, "");
    let body: String = (0..300).map(|i| format!("row {i}\\n")).collect();
    let render = format!("(render-buffer \"*rows*\" \"rows-mode\" \"{body}\" '())");
    ed.update(cx, |e, cx| e.run_script_blocking(&render, cx));
    settle(&ed, cx);
    let wheel = ed.update(cx, |e, _| {
        // The headless platform never lays out, so give the view real geometry.
        e.viewport_height = px(400.);
        e.line_height = px(20.);
        // Wheel-scroll far down without moving point (it stays on line 0).
        let to = px(20. * 120.);
        e.active.scroll_offset = to;
        e.cursor_follow_pending = false;
        to
    });
    ed.update(cx, |e, cx| e.run_script_blocking(&render, cx));
    settle(&ed, cx);
    ed.read_with(cx, |e, _| {
        assert_eq!(e.active.name, "*rows*");
        assert_eq!(e.active.scroll_offset, wheel, "refresh scrolled the view");
        assert_eq!(
            e.active.buffer.line_index_at_byte(e.point()),
            0,
            "point kept its line"
        );
    });
}
