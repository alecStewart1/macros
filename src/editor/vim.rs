//! Vim-grammar infrastructure for evil mode: operator-pending state, one-key
//! argument capture (`%c` bindings), and the routing that turns `2d3w` into a
//! single Scheme call. The motions/operators themselves live in
//! `scheme/evil.scm`; Rust only owns the parts that must be synchronous with
//! keystrokes (which keymap the next key resolves against, what counts were
//! typed), so the composed command is self-contained — a macro or `.` replays
//! it without any of this state.

use gpui::{Context, Keystroke, SharedString};

use super::input::Handled;
use super::*;

/// The keymap motions and text objects resolve against while an operator is
/// pending (`d`, `c`, `y`, `>`, `gU`, …). Like the other evil keymaps it may be
/// shadowed per major mode (`evil-operator:<major>`).
pub(super) const OPERATOR_KEYMAP: &str = "evil-operator";

/// The placeholder a binding uses for a one-keystroke argument: `m` is bound to
/// `evil-set-mark %c`, so `ma` runs `(evil-set-mark "a")`.
pub(super) const CHAR_ARG: &str = "%c";

/// An operator waiting for its motion (`d` typed, `w` not yet).
#[derive(Clone, Debug)]
pub(super) struct OpPending {
    /// Operator name handed to Scheme's `evil-operate` ("delete", "change", …).
    pub op: String,
    /// The key sequence that started it — typing it again (`dd`, `gUgU`), or
    /// its last key (`gUU`), means "the current line(s)".
    pub keys: String,
    /// The count typed before the operator (`2` in `2d3w`).
    pub count: Option<usize>,
    /// `v` / `V` typed after the operator: force charwise / linewise.
    pub force: Option<&'static str>,
}

/// A binding waiting for its one-key argument (`%c`).
#[derive(Clone, Debug)]
pub(super) struct Capture {
    pub expr: String,
    pub count: Option<usize>,
    /// Set when the capture is a motion of a pending operator (`dfx`, `d'a`).
    pub op: Option<OpPending>,
}

/// All Vim-grammar state hung off the editor.
#[derive(Default)]
pub(super) struct VimState {
    pub op: Option<OpPending>,
    pub capture: Option<Capture>,
    /// The motion count typed before an operator's `/` / `?` search motion,
    /// held while the minibuffer reads the pattern.
    pub search_count: Option<usize>,
}

/// `(evil-operator "name")` → `name`.
pub(super) fn parse_operator_expr(expr: &str) -> Option<String> {
    let (name, args) = parse_string_call(expr)?;
    (name == "evil-operator" && args.len() == 1).then(|| args[0].clone())
}

/// Parse `(name "a" "b" …)` — a call whose arguments are all string literals.
/// `None` for anything else (nested forms, numbers, symbols).
pub(super) fn parse_string_call(expr: &str) -> Option<(String, Vec<String>)> {
    let inner = expr.trim().strip_prefix('(')?.strip_suffix(')')?;
    let inner = inner.trim_start();
    let name_end = inner
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner.len());
    let name = &inner[..name_end];
    if name.is_empty() || name.contains(['(', ')', '"']) {
        return None;
    }
    let mut rest = inner[name_end..].trim_start();
    let mut args = Vec::new();
    while !rest.is_empty() {
        let body = rest.strip_prefix('"')?;
        let mut out = String::new();
        let mut chars = body.char_indices();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    let (_, n) = chars.next()?;
                    out.push(n);
                }
                '"' => {
                    end = Some(i);
                    break;
                }
                _ => out.push(c),
            }
        }
        let end = end?;
        args.push(out);
        rest = body[end + 1..].trim_start();
    }
    Some((name.to_string(), args))
}

/// A Steel string literal for `s` (quotes and backslashes escaped).
pub(super) fn steel_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The operator count times the motion count (`2d3w` = 6), or 0 when neither
/// was typed (so a motion can tell `G` from `1G`). Capped like the plain count.
pub(super) fn combined_count(op: Option<usize>, motion: Option<usize>) -> usize {
    match (op, motion) {
        (None, None) => 0,
        (a, b) => a
            .unwrap_or(1)
            .saturating_mul(b.unwrap_or(1))
            .clamp(1, 10_000),
    }
}

/// The self-contained Scheme call an operator + motion resolves to. A macro or
/// `.` replays exactly this string — no pending state is needed to rerun it.
pub(super) fn compose_operate(op: &OpPending, motion_count: Option<usize>, motion: &str) -> String {
    let motion = motion.trim();
    let motion = if motion.starts_with('(') {
        motion.to_string()
    } else {
        format!("({motion})")
    };
    format!(
        "(evil-operate {} {} {} (lambda () {motion}))",
        steel_quote(&op.op),
        combined_count(op.count, motion_count),
        steel_quote(op.force.unwrap_or("")),
    )
}

/// Substitute the captured key for the `%c` placeholder.
pub(super) fn fill_char_arg(expr: &str, ch: &str) -> String {
    expr.replacen(CHAR_ARG, &steel_quote(ch), 1)
}

impl Editor {
    /// Route a key-resolved evil binding that needs more input before it can
    /// run: a `%c` argument capture, or an operator (`evil-operator`) that now
    /// waits for its motion. Returns true when it took the binding (nothing
    /// should be dispatched yet).
    pub(super) fn evil_route_binding(
        &mut self,
        expr: &str,
        keys: &str,
        count: Option<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        if expr.contains(CHAR_ARG) {
            self.vim.capture = Some(Capture {
                expr: expr.to_string(),
                count,
                op: None,
            });
            let n = count.map(|n| n.to_string()).unwrap_or_default();
            self.status = format!("{n}{}-", pretty_sequence(keys)).into();
            cx.notify();
            return true;
        }
        let Some(op) = parse_operator_expr(expr) else {
            return false;
        };
        if self.evil == EvilState::Normal {
            self.vim.op = Some(OpPending {
                op,
                keys: keys.to_string(),
                count,
                force: None,
            });
            self.echo_operator_pending(cx);
        } else {
            // Visual states: the operator applies to the selection right away.
            let expr = format!("(evil-visual-operator {})", steel_quote(&op));
            self.dispatch_counted(&expr, count, cx);
        }
        true
    }

    fn echo_operator_pending(&mut self, cx: &mut Context<Self>) {
        if let Some(op) = &self.vim.op {
            let n = op.count.map(|n| n.to_string()).unwrap_or_default();
            let m = self.modal_count.map(|n| n.to_string()).unwrap_or_default();
            let force = op.force.unwrap_or("");
            let pend = if self.pending_keys.is_empty() {
                String::new()
            } else {
                format!("{} ", pretty_sequence(&self.pending_keys.join(" ")))
            };
            self.status = format!("{n}{}{force}{m}{pend}-", pretty_sequence(&op.keys)).into();
        }
        cx.notify();
    }

    /// Abandon a pending operator / capture (Escape, an unbound key).
    pub(super) fn evil_cancel_pending(&mut self, cx: &mut Context<Self>) {
        self.vim.op = None;
        self.vim.capture = None;
        self.pending_keys.clear();
        self.modal_count = None;
        self.status = SharedString::default();
        cx.notify();
    }

    /// Run an operator over a resolved motion expression.
    pub(super) fn evil_run_operator(
        &mut self,
        op: OpPending,
        motion_count: Option<usize>,
        motion: &str,
        cx: &mut Context<Self>,
    ) {
        let expr = compose_operate(&op, motion_count, motion);
        self.status = SharedString::default();
        self.dispatch_recorded(&expr, cx);
    }

    /// Operator-pending keys: counts, `v`/`V` forcing, the doubled operator
    /// (`dd`), and the motion / text-object keymap. Anything unbound cancels
    /// the operator, as in Vim.
    pub(super) fn key_operator_pending(
        &mut self,
        ks: &Keystroke,
        cx: &mut Context<Self>,
    ) -> Handled {
        let chord = chord_string(ks);
        cx.stop_propagation();
        if chord == "escape" || chord == "ctrl-g" || chord == "ctrl-c" {
            self.evil_cancel_pending(cx);
            return Handled::Yes;
        }
        let plain = !ks.modifiers.control && !ks.modifiers.alt;
        if self.pending_keys.is_empty()
            && plain
            && let Some(d) = Self::ascii_digit(&chord)
            && (d != 0 || self.modal_count.is_some())
        {
            let n = self.modal_count.unwrap_or(0).saturating_mul(10) + d as usize;
            self.modal_count = Some(n.min(10_000));
            self.echo_operator_pending(cx);
            return Handled::Yes;
        }
        let Some(mut op) = self.vim.op.take() else {
            return Handled::No;
        };
        if self.pending_keys.is_empty() && (chord == "v" || chord == "shift-v") {
            op.force = Some(if chord == "v" { "v" } else { "V" });
            self.vim.op = Some(op);
            self.echo_operator_pending(cx);
            return Handled::Yes;
        }
        let candidate = if self.pending_keys.is_empty() {
            chord.clone()
        } else {
            format!("{} {}", self.pending_keys.join(" "), chord)
        };
        // The doubled operator: `dd`, `>>`, `gUgU`, and `gUU` (its last key).
        let last_key = op.keys.rsplit(' ').next().unwrap_or("");
        if candidate == op.keys || (self.pending_keys.is_empty() && chord == last_key) {
            self.pending_keys.clear();
            let count = self.modal_count.take();
            self.evil_run_operator(op, count, "(evil-motion \"line\")", cx);
            return Handled::Yes;
        }
        let major: Option<String> = self.active.modes.names().last().map(|s| s.to_string());
        if let Some(expr) = self
            .keymap
            .resolve_evil(OPERATOR_KEYMAP, major.as_deref(), &candidate)
            .map(str::to_string)
        {
            self.pending_keys.clear();
            let count = self.modal_count.take();
            if expr.contains(CHAR_ARG) {
                self.vim.capture = Some(Capture {
                    expr,
                    count,
                    op: Some(op),
                });
                self.status = format!("{}-", pretty_sequence(&candidate)).into();
                cx.notify();
            } else if let Some(forward) = search_prompt_expr(&expr) {
                // `d/foo<RET>`: read the pattern, then run the operator.
                self.vim.op = Some(op);
                self.vim.search_count = count;
                let prompt = if forward { "/" } else { "?" };
                self.open_minibuffer(prompt, MinibufferPurpose::OperatorSearch(forward), "", cx);
            } else {
                self.evil_run_operator(op, count, &expr, cx);
            }
            return Handled::Yes;
        }
        if self
            .keymap
            .is_prefix_evil(OPERATOR_KEYMAP, major.as_deref(), &candidate)
        {
            self.pending_keys.push(chord);
            self.vim.op = Some(op);
            self.echo_operator_pending(cx);
            return Handled::Yes;
        }
        // Not a motion: the whole command is aborted.
        self.evil_cancel_pending(cx);
        Handled::Yes
    }

    /// The one-keystroke argument of a `%c` binding (`ma`, `"a`, `dfx`). A
    /// printable key fills it in; anything else cancels.
    pub(super) fn key_evil_capture(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        let Some(capture) = self.vim.capture.take() else {
            return Handled::No;
        };
        cx.stop_propagation();
        self.status = SharedString::default();
        let is_text = ks.key_char.is_some()
            && !ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform;
        let ch = match ks.key_char.clone() {
            Some(ch) if is_text => ch,
            _ => {
                self.evil_cancel_pending(cx);
                return Handled::Yes;
            }
        };
        let expr = fill_char_arg(&capture.expr, &ch);
        match capture.op {
            Some(op) => self.evil_run_operator(op, capture.count, &expr, cx),
            None => {
                if !self.evil_route_binding(&expr, "", capture.count, cx) {
                    self.dispatch_counted(&expr, capture.count, cx);
                }
            }
        }
        cx.notify();
        Handled::Yes
    }

    /// `d/pat<RET>` / `d?pat<RET>`: the minibuffer submitted the pattern.
    pub(super) fn evil_operator_search_submit(
        &mut self,
        forward: bool,
        input: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(op) = self.vim.op.take() else {
            return;
        };
        let count = self.vim.search_count.take();
        let pattern = if input.is_empty() {
            self.search_query.clone().unwrap_or_default()
        } else {
            input.to_string()
        };
        if pattern.is_empty() {
            return;
        }
        let motion = format!(
            "(evil-motion-search {} {})",
            steel_quote(&pattern),
            if forward { "#t" } else { "#f" }
        );
        self.evil_run_operator(op, count, &motion, cx);
    }
}

/// `(evil-op-search "/")` / `(evil-op-search "?")` — the search motions, which
/// need the minibuffer before the operator can run.
fn search_prompt_expr(expr: &str) -> Option<bool> {
    let (name, args) = parse_string_call(expr)?;
    if name != "evil-op-search" || args.len() != 1 {
        return None;
    }
    Some(args[0] != "?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_calls() {
        assert_eq!(
            parse_string_call(r#"(evil-operator "delete")"#),
            Some(("evil-operator".into(), vec!["delete".into()]))
        );
        assert_eq!(
            parse_string_call(r#"(f "a\"b" "c")"#),
            Some(("f".into(), vec!["a\"b".into(), "c".into()]))
        );
        assert_eq!(parse_string_call("(f 1)"), None);
        assert_eq!(parse_string_call("(f (g))"), None);
        assert_eq!(
            parse_string_call("(evil-repeat)"),
            Some(("evil-repeat".into(), vec![]))
        );
        assert_eq!(
            parse_operator_expr(r#"(evil-operator "upcase")"#).as_deref(),
            Some("upcase")
        );
        assert_eq!(parse_operator_expr("(evil-delete-line)"), None);
    }

    #[test]
    fn composes_operator_calls() {
        let op = OpPending {
            op: "delete".into(),
            keys: "d".into(),
            count: Some(2),
            force: None,
        };
        assert_eq!(
            compose_operate(&op, Some(3), r#"(evil-motion "w")"#),
            r#"(evil-operate "delete" 6 "" (lambda () (evil-motion "w")))"#
        );
        let op = OpPending {
            count: None,
            force: Some("V"),
            ..op
        };
        assert_eq!(
            compose_operate(&op, None, "evil-motion \"j\""),
            r#"(evil-operate "delete" 0 "V" (lambda () (evil-motion "j")))"#
        );
        assert_eq!(combined_count(None, Some(4)), 4);
        assert_eq!(
            fill_char_arg("(evil-motion-find \"f\" %c)", "\""),
            r#"(evil-motion-find "f" "\"")"#
        );
        assert_eq!(search_prompt_expr(r#"(evil-op-search "?")"#), Some(false));
    }
}
