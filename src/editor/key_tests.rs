//! Keybinding tests through a real window: each key goes through gpui's own
//! `Window::dispatch_keystroke` — the action keymap (RET, DEL, arrows), the
//! editor's key-down layers, then the text-input handler — the path a typed
//! key takes and the one the MCP `press_keys` tool drives. Keys are written in
//! `press_keys` notation (see [`super::mcp::parse_keys`]).

use gpui::{Entity, TestAppContext, VisualTestContext};

use super::mcp::parse_keys;
use super::*;

#[derive(Clone, Copy)]
enum Style {
    Emacs,
    Evil,
    Helix,
}

/// A windowed editor with the bundled Scheme loaded, `style` keys on, and a
/// plain-text buffer holding `text`, point at its start.
fn setup<'a>(
    cx: &'a mut TestAppContext,
    style: Style,
    text: &str,
) -> (Entity<Editor>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(crate::theme::Theme::default());
        cx.set_global(crate::settings::Settings::default());
        cx.bind_keys(super::key_bindings());
    });
    let (editor, vcx) = cx.add_window_view(|_, cx| Editor::new(cx));
    editor.update(vcx, |e, cx| {
        for src in crate::runtime::startup_scripts() {
            e.run_script_blocking(src, cx);
        }
        match style {
            Style::Emacs => {}
            Style::Evil => e.run_script_blocking("(evil-mode)", cx),
            Style::Helix => e.run_script_blocking("(helix-mode)", cx),
        }
        e.mark_config_loaded(cx);
        let current = e.snapshot_active();
        e.inactive.push(current);
        e.install_active(make_document("test.txt".into(), None, text));
        if !matches!(style, Style::Emacs) {
            e.evil = EvilState::Normal;
        }
    });
    let focus = editor.read_with(vcx, |e, _| e.focus_handle.clone());
    vcx.update(|window, _| window.focus(&focus));
    settle(&editor, vcx);
    (editor, vcx)
}

/// Let dispatched Scheme commands run and their outcomes apply, and redraw
/// (painting installs the text-input handler typed characters go through).
fn settle(editor: &Entity<Editor>, vcx: &mut VisualTestContext) {
    for _ in 0..8 {
        vcx.run_until_parked();
        editor.read_with(vcx, |e, _| {
            let _ = e.script.eval("0");
        });
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
    }
}

/// Press `seq` (`press_keys` notation), settling after each key.
fn press(editor: &Entity<Editor>, vcx: &mut VisualTestContext, seq: &str) {
    for ks in parse_keys(seq).expect("valid keys") {
        vcx.update(|window, cx| {
            window.dispatch_keystroke(ks, cx);
        });
        settle(editor, vcx);
    }
}

fn text(editor: &Entity<Editor>, vcx: &mut VisualTestContext) -> String {
    editor.read_with(vcx, |e, _| e.active.buffer.full_text())
}

fn point(editor: &Entity<Editor>, vcx: &mut VisualTestContext) -> usize {
    editor.read_with(vcx, |e, _| e.point())
}

fn state(editor: &Entity<Editor>, vcx: &mut VisualTestContext) -> EvilState {
    editor.read_with(vcx, |e, _| e.evil)
}

// --- notation ----------------------------------------------------------------

#[test]
fn parses_emacs_and_gpui_notation() {
    let show = |seq: &str| -> Vec<String> {
        parse_keys(seq)
            .unwrap()
            .iter()
            .map(|k| {
                let m = &k.modifiers;
                format!(
                    "{}{}{}{}{}|{}",
                    if m.control { "ctrl-" } else { "" },
                    if m.alt { "alt-" } else { "" },
                    if m.platform { "cmd-" } else { "" },
                    if m.shift { "shift-" } else { "" },
                    k.key,
                    k.key_char.as_deref().unwrap_or("")
                )
            })
            .collect()
    };
    assert_eq!(
        show("C-x C-f M-x C-M-a s-v"),
        ["ctrl-x|", "ctrl-f|", "alt-x|", "ctrl-alt-a|", "cmd-v|"]
    );
    assert_eq!(
        show("RET SPC TAB ESC DEL <left> <f1> <return>"),
        [
            "enter|",
            "space|",
            "tab|",
            "escape|",
            "backspace|",
            "left|",
            "f1|",
            "enter|"
        ]
    );
    assert_eq!(
        show("ctrl-x alt-f cmd-v shift-tab enter"),
        ["ctrl-x|", "alt-f|", "cmd-v|", "shift-tab|", "enter|"]
    );
    assert_eq!(show("a A $ C--"), ["a|a", "shift-a|A", "$|$", "ctrl--|"]);
    assert_eq!(
        show("\"hi there\""),
        ["h|h", "i|i", "space| ", "t|t", "h|h", "e|e", "r|r", "e|e"]
    );
    assert_eq!(show(r#""a\"b\n""#), ["a|a", "\"|\"", "b|b", "enter|"]);
    assert!(parse_keys("C-x <nope>").is_err());
    assert!(parse_keys("frobnicate").is_err());
    assert!(parse_keys("\"open").is_err());
}

// --- the window and the MCP report ---------------------------------------------

#[gpui::test]
fn window_is_recorded_and_state_reports_keys(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Emacs, "hello\nworld\n");
    assert!(ed.read_with(vcx, |e, _| e.window_handle.is_some()));
    press(&ed, vcx, "C-n C-e C-x");
    let s = ed.read_with(vcx, |e, _| e.key_state());
    assert_eq!(s.state(), "emacs");
    assert_eq!(s.position(), (11, 1, 5));
    assert_eq!(s.current_line(), "world");
    assert_eq!(s.pending(), "ctrl-x");
    press(&ed, vcx, "C-g");
    let s = ed.read_with(vcx, |e, _| e.key_state());
    assert_eq!(s.pending(), "");
}

// --- Emacs -----------------------------------------------------------------------

#[gpui::test]
fn emacs_typing_and_os_keys(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Emacs, "");
    // Plain keys reach the buffer through the text-input handler; RET and DEL
    // through the window's action keymap.
    press(&ed, vcx, "\"hello world\" RET \"second\" DEL");
    assert_eq!(text(&ed, vcx), "hello world\nsecon");
    press(&ed, vcx, "<up> <home>");
    assert_eq!(point(&ed, vcx), 0);
}

#[gpui::test]
fn emacs_motion_chords(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Emacs, "one two three\nfour five\n");
    press(&ed, vcx, "C-e");
    assert_eq!(point(&ed, vcx), 13);
    press(&ed, vcx, "C-a M-f M-f");
    assert_eq!(point(&ed, vcx), 7);
    press(&ed, vcx, "M-b");
    assert_eq!(point(&ed, vcx), 4);
    press(&ed, vcx, "C-n C-a C-f C-f");
    assert_eq!(point(&ed, vcx), 16);
    press(&ed, vcx, "C-p");
    assert_eq!(point(&ed, vcx), 2);
}

#[gpui::test]
fn emacs_kill_yank_and_undo(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Emacs, "alpha beta\ngamma\n");
    press(&ed, vcx, "C-k");
    assert_eq!(text(&ed, vcx), "\ngamma\n");
    press(&ed, vcx, "C-n C-e C-y");
    assert_eq!(text(&ed, vcx), "\ngammaalpha beta\n");
    press(&ed, vcx, "C-/");
    assert_eq!(text(&ed, vcx), "\ngamma\n");
    press(&ed, vcx, "C-a M-d");
    assert_eq!(text(&ed, vcx), "\n\n");
}

#[gpui::test]
fn emacs_prefix_and_minibuffer(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Emacs, "x\n");
    // M-x opens a minibuffer; typed text goes into it, not the buffer.
    press(&ed, vcx, "M-x \"abc\"");
    let s = ed.read_with(vcx, |e, _| e.key_state());
    assert!(s.prompt().is_some(), "M-x opened no prompt");
    assert!(s.prompt().unwrap().ends_with("abc"));
    assert_eq!(text(&ed, vcx), "x\n");
    press(&ed, vcx, "C-g");
    assert!(ed.read_with(vcx, |e, _| e.key_state().prompt().is_none()));
    // A prefix waits for its second key, which then runs the binding.
    press(&ed, vcx, "C-SPC C-e C-x C-x");
    assert_eq!(point(&ed, vcx), 0);
}

// --- evil ------------------------------------------------------------------------

#[gpui::test]
fn evil_operators_and_insert(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Evil, "one two three\n");
    press(&ed, vcx, "d w");
    assert_eq!(text(&ed, vcx), "two three\n");
    // `c w` enters Insert; plain keys type through the input handler.
    press(&ed, vcx, "c w \"2\" ESC");
    assert_eq!(text(&ed, vcx), "2 three\n");
    assert!(state(&ed, vcx) == EvilState::Normal);
    press(&ed, vcx, "u");
    assert_eq!(text(&ed, vcx), "two three\n");
    press(&ed, vcx, "A \"!\" ESC");
    assert_eq!(text(&ed, vcx), "two three!\n");
}

#[gpui::test]
fn evil_lines_counts_and_dot(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Evil, "a\nb\nc\nd\n");
    press(&ed, vcx, "d d");
    assert_eq!(text(&ed, vcx), "b\nc\nd\n");
    press(&ed, vcx, ".");
    assert_eq!(text(&ed, vcx), "c\nd\n");
    press(&ed, vcx, "p");
    assert_eq!(text(&ed, vcx), "c\nb\nd\n");
    press(&ed, vcx, "g g 2 j");
    assert_eq!(point(&ed, vcx), 4);
    press(&ed, vcx, "o \"new\" ESC");
    assert_eq!(text(&ed, vcx), "c\nb\nd\nnew\n");
}

#[gpui::test]
fn evil_visual_and_insert_state_keys(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Evil, "hello world\n");
    press(&ed, vcx, "v e");
    assert!(state(&ed, vcx) == EvilState::Visual);
    press(&ed, vcx, "d");
    assert_eq!(text(&ed, vcx), " world\n");
    assert!(state(&ed, vcx) == EvilState::Normal);
    // In Insert, RET and DEL are the window's action keys.
    press(&ed, vcx, "i \"ab\" RET DEL DEL ESC");
    assert_eq!(text(&ed, vcx), "a world\n");
}

// --- Helix -----------------------------------------------------------------------

#[gpui::test]
fn helix_select_then_act(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Helix, "one two three\n");
    assert!(ed.read_with(vcx, |e, _| e.modal_helix));
    // `w` selects the word; `d` deletes the selection (noun → verb).
    press(&ed, vcx, "w d");
    assert_eq!(text(&ed, vcx), "two three\n");
    press(&ed, vcx, "u");
    assert_eq!(text(&ed, vcx), "one two three\n");
    press(&ed, vcx, "U");
    assert_eq!(text(&ed, vcx), "two three\n");
}

#[gpui::test]
fn helix_line_select_and_change(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Helix, "first\nsecond one\nthird\n");
    press(&ed, vcx, "x d");
    assert_eq!(text(&ed, vcx), "second one\nthird\n");
    // `w` selects "second " (word plus trailing space); `c` replaces it.
    press(&ed, vcx, "w c \"2nd \" ESC");
    assert_eq!(text(&ed, vcx), "2nd one\nthird\n");
    assert!(state(&ed, vcx) == EvilState::Normal);
}

/// Like Helix, `w` / `W` stop at a line ending, so `w c` on a line's last
/// word keeps the newline; the next `w` moves on to the next line's word.
#[gpui::test]
fn helix_word_at_line_end_keeps_newline(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Helix, "first\nthird\n");
    press(&ed, vcx, "w c \"1st\" ESC");
    assert_eq!(text(&ed, vcx), "1st\nthird\n");
    // From the end of a line, `w` skips the break and deletes just `third`.
    press(&ed, vcx, "w d");
    assert_eq!(text(&ed, vcx), "1st\n\n");
}

#[gpui::test]
fn helix_bigword_at_line_end_keeps_newline(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Helix, "a.b\nc\n");
    press(&ed, vcx, "W d");
    assert_eq!(text(&ed, vcx), "\nc\n");
}

#[gpui::test]
fn helix_insert_append_and_open(cx: &mut TestAppContext) {
    let (ed, vcx) = setup(cx, Style::Helix, "mid\n");
    press(&ed, vcx, "I \"<\" ESC A \">\" ESC");
    assert_eq!(text(&ed, vcx), "<mid>\n");
    press(&ed, vcx, "o \"below\" ESC");
    assert_eq!(text(&ed, vcx), "<mid>\nbelow\n");
    press(&ed, vcx, "% d");
    assert_eq!(text(&ed, vcx), "");
}
