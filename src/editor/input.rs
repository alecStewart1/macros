//! Keyboard, mouse, scroll, and OS action handlers for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::time::Duration;

use gpui::{
    Bounds, Context, KeyDownEvent, Keystroke, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollWheelEvent, SharedString, Window, px,
};

use crate::script::EditorCommand;

use super::*;

/// Emacs universal-argument (`C-u` / `M-<digit>`) reading state. While
/// `reading`, digit keys accumulate into `digits` and repeated `C-u` raises
/// `cu` (giving the 4/16/64 default). When a non-argument key follows, the
/// computed count lands in `pending` for the next command to consume as a
/// repeat count. The three reading fields reset together via
/// [`Editor::reset_uarg`].
#[derive(Default)]
pub(super) struct UargState {
    pub(super) reading: bool,
    pub(super) digits: Option<i64>,
    pub(super) cu: u32,
    pub(super) pending: Option<i64>,
}

/// Whether a modal key layer consumed the keystroke. `Yes` stops the router
/// (the layer already called `cx.stop_propagation()` if the event must not
/// reach the IME / OS); `No` hands the key to the next layer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Handled {
    Yes,
    No,
}

impl Editor {
    /// One ASCII digit `0`–`9` if `key` is exactly that, else `None`.
    pub(super) fn ascii_digit(key: &str) -> Option<i64> {
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_digit() => Some((c as u8 - b'0') as i64),
            _ => None,
        }
    }

    /// Handle a keystroke while reading (or about to start) a universal-argument.
    /// Returns `true` if the key was consumed as part of the argument (the caller
    /// stops here); `false` if the key should proceed to normal dispatch — in
    /// which case `self.uarg.pending` has been set so the command repeats.
    pub(super) fn handle_prefix_arg(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        let only_ctrl = ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.platform;
        let only_alt = ks.modifiers.alt && !ks.modifiers.control && !ks.modifiers.platform;
        let plain = !ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.platform;
        let digit = Self::ascii_digit(&ks.key);

        // `C-u`: begin reading, or multiply the running default by 4 (4, 16, …).
        if only_ctrl && ks.key == "u" {
            self.uarg.reading = true;
            self.uarg.digits = None;
            self.uarg.cu = self.uarg.cu.saturating_add(1);
            self.echo_prefix_arg(cx);
            return true;
        }

        if self.uarg.reading {
            // A digit (plain, or `M-<digit>`) extends the explicit number.
            if (plain || only_alt)
                && let Some(d) = digit
            {
                let acc = self
                    .uarg
                    .digits
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(d);
                self.uarg.digits = Some(acc);
                self.echo_prefix_arg(cx);
                return true;
            }
            // Any other key ends the argument: stash the count and fall through
            // so this key runs as the command.
            self.uarg.pending = Some(prefix_arg_value(self.uarg.digits, self.uarg.cu));
            self.reset_uarg();
            return false;
        }

        // Not yet reading: `M-<digit>` starts a digit-argument (Emacs).
        if only_alt && let Some(d) = digit {
            self.uarg.reading = true;
            self.uarg.digits = Some(d);
            self.uarg.cu = 0;
            self.echo_prefix_arg(cx);
            return true;
        }

        false
    }

    pub(super) fn reset_uarg(&mut self) {
        self.uarg.reading = false;
        self.uarg.digits = None;
        self.uarg.cu = 0;
    }

    /// Echo the argument being built in the status line, Emacs-style.
    pub(super) fn echo_prefix_arg(&mut self, cx: &mut Context<Self>) {
        let v = prefix_arg_value(self.uarg.digits, self.uarg.cu);
        self.status = format!("C-u {v}").into();
        cx.notify();
    }

    /// Mode-driven dispatch for `Ctrl-`/`Alt-` chords and prefix sequences.
    /// Plain printable keys are left to the IME (self-insert); OS-special keys
    /// (arrows, Backspace, …) are handled by GPUI actions in `main`.
    ///
    /// This is a pure router: each modal layer lives in its own `key_*` method,
    /// tried in precedence order. A layer returns [`Handled::Yes`] when it
    /// consumed the keystroke (having stopped propagation itself if the event
    /// must not reach the IME / OS) and [`Handled::No`] to pass it on. The final
    /// Emacs dispatch (`key_emacs`) is the catch-all.
    pub(super) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        if ks.key.is_empty() {
            return; // modifier-only press
        }

        // Any keystroke is cursor activity: keep the cursor solid and restart the
        // blink cycle (no-op timer when `cursor-blink` is off).
        self.reset_blink(cx);

        // Precedence: startup gate → transient → helm → minibuffer → completion
        // popup → terminal → evil replace-pending → evil Normal/Visual → evil
        // Insert → special-buffer chord overlay → special buffer → universal
        // argument → Emacs dispatch.
        let layers = [
            Self::key_config_pending,
            Self::key_transient,
            Self::key_helm,
            Self::key_minibuffer,
            Self::key_completion_popup,
            Self::key_terminal,
            Self::key_evil_replace,
            Self::key_evil_modal,
            Self::key_evil_insert,
            Self::key_special_chord_overlay,
            Self::key_special_buffer,
            Self::key_universal_argument,
        ];
        for layer in layers {
            if layer(self, ks, cx) == Handled::Yes {
                return;
            }
        }
        self.key_emacs(ks, cx);
    }

    /// Startup config hasn't been applied yet (the window may have been
    /// revealed by the load watchdog before `init.scm` finished). Swallow
    /// plain keys so they can't self-insert before `(evil-mode)` and the
    /// keymaps take effect; Ctrl/Alt/Cmd chords still pass (C-g, C-x C-c, …).
    fn key_config_pending(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.config_pending
            && !ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform
        {
            cx.stop_propagation();
            return Handled::Yes;
        }
        Handled::No
    }

    /// A transient popup is modal: C-g / Escape close it; a row's key runs its
    /// command (the popup closes first); everything else is swallowed.
    fn key_transient(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.transient.is_none() {
            return Handled::No;
        }
        if (ks.modifiers.control && ks.key == "g") || ks.key == "escape" {
            self.transient_quit(cx);
        } else if let Some(command) = self.transient_command(&chord_string(ks)) {
            // A sticky popup stays open after a row runs (the command re-opens
            // it to refresh, or calls `transient-quit`); a normal one closes
            // first so its command runs without the menu in the way.
            if !self.transient_is_sticky() {
                self.transient = None;
            }
            cx.notify();
            // Transient rows store a bare command name (e.g. `magit-do-commit`);
            // wrap it in parens so it's *called*, not just evaluated to the
            // closure — the same convention `DefineKey` uses for keymaps.
            self.run_script(&format!("({command})"), cx);
        }
        cx.stop_propagation();
        Handled::Yes
    }

    /// Helm is modal: C-g aborts, chords resolve against the Scheme `helm`
    /// keymap, any other Ctrl/Alt chord is swallowed, and plain text falls
    /// through to the IME (→ `helm_input`).
    fn key_helm(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.helm.is_none() {
            return Handled::No;
        }
        if ks.modifiers.control && ks.key == "g" {
            self.helm_abort(cx);
            cx.stop_propagation();
            return Handled::Yes;
        }
        let chord = chord_string(ks);
        if let Some(expr) = self.keymap.resolve_mode("helm", &chord).map(str::to_string) {
            self.run_script(&expr, cx);
            cx.stop_propagation();
            return Handled::Yes;
        }
        if ks.modifiers.control || ks.modifiers.alt {
            cx.stop_propagation();
        }
        Handled::Yes
    }

    /// While the minibuffer is open, C-g aborts it (Emacs); other chords are
    /// swallowed so they can't reach the main buffer. Plain and OS-special keys
    /// fall through to the IME / GPUI actions, which route to the minibuffer.
    /// Under evil ONLY, Escape also aborts, so the `:` ex prompt (and any other
    /// read) is dismissed with a single Esc (vim). In pure Emacs mode Escape is
    /// left alone — there it is the Meta prefix, and C-g is the canceller.
    fn key_minibuffer(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.minibuffer.is_none() {
            return Handled::No;
        }
        let evil_escape = ks.key == "escape" && self.evil != EvilState::Disabled;
        if (ks.modifiers.control && ks.key == "g") || evil_escape {
            self.mb_abort(cx);
            cx.stop_propagation();
        } else if self.is_isearch() && ks.modifiers.control && ks.key == "s" {
            // C-s within isearch: repeat forward (Emacs).
            self.isearch_repeat(true, cx);
            cx.stop_propagation();
        } else if self.is_isearch() && ks.modifiers.control && ks.key == "r" {
            // C-r within isearch: repeat backward.
            self.isearch_repeat(false, cx);
            cx.stop_propagation();
        } else if ks.modifiers.alt && ks.key == "p" {
            self.mb_history(-1, cx); // M-p: older history
            cx.stop_propagation();
        } else if ks.modifiers.alt && ks.key == "n" {
            self.mb_history(1, cx); // M-n: newer history
            cx.stop_propagation();
        } else if ks.modifiers.control || ks.modifiers.alt {
            cx.stop_propagation();
        }
        Handled::Yes
    }

    /// While the completion popup is open, `Ctrl-`/`Alt-` chords resolve
    /// against the Scheme-defined `completion` keymap first (C-n/C-p/C-g, …).
    /// An unbound chord dismisses the popup and is then handled normally; plain
    /// text keys fall through to self-insert, which re-filters the popup.
    fn key_completion_popup(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.completion_popup.is_none() || !(ks.modifiers.control || ks.modifiers.alt) {
            return Handled::No;
        }
        let chord = chord_string(ks);
        if let Some(expr) = self
            .keymap
            .resolve_mode("completion", &chord)
            .map(str::to_string)
        {
            self.pending_keys.clear();
            self.run_script(&expr, cx);
            cx.stop_propagation();
            return Handled::Yes;
        }
        self.completion_popup = None; // dismiss; fall through to normal dispatch
        Handled::No
    }

    /// An embedded terminal grabs the keyboard: keys are translated to PTY
    /// bytes rather than editor commands. `C-x` stays the editor prefix (so
    /// window/buffer management still works), as does any in-progress prefix
    /// sequence — everything else (including C-c, arrows, C-r…) goes to the shell.
    /// Once the shell has exited the terminal is dead: it releases the keyboard
    /// so normal editor dispatch (M-x, C-x k, …) resumes in the leftover buffer.
    fn key_terminal(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        let Some(tid) = self.active_running_terminal_id() else {
            return Handled::No;
        };
        // `C-x` is the editor prefix, an in-progress sequence continues, and
        // platform (cmd) chords fall through to the editor — so cmd-c/cmd-v
        // (terminal copy/paste) and cmd-q still work.
        let editor_prefix = !self.pending_keys.is_empty()
            || (ks.modifiers.control && ks.key == "x")
            || ks.modifiers.platform;
        if editor_prefix {
            return Handled::No;
        }
        if let Some(bytes) = keystroke_to_bytes(ks) {
            self.terminal_input(tid, bytes, cx);
        }
        cx.stop_propagation();
        Handled::Yes
    }

    /// Vim `r`: while replace-pending is armed (by `evil-replace-char`), the
    /// very next keystroke is consumed as the literal replacement character —
    /// it must NOT reach the evil keymap, or `r` followed by `x`/`d`/… would
    /// run that command instead of overwriting with the letter. A printable key
    /// overwrites the char under the cursor; any other chord (Escape, a
    /// Ctrl/Alt combo, …) cancels with no edit, matching vim.
    fn key_evil_replace(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if !self.evil_replace_pending {
            return Handled::No;
        }
        self.evil_replace_pending = false;
        self.status = SharedString::default();
        let is_text = ks.key_char.is_some()
            && !ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform;
        if is_text && let Some(ch) = ks.key_char.clone() {
            self.evil_replace_char(&ch, cx);
        }
        cx.notify();
        cx.stop_propagation();
        Handled::Yes
    }

    /// Evil Normal/Visual: the evil keymap is consulted FIRST for every chord,
    /// so it can override ANY Emacs default — plain keys, Ctrl/Alt chords, and
    /// multi-key sequences alike. An evil binding wins; an evil prefix pends
    /// (`g g`, leader `space …`, or a user-defined `C-x …`). Otherwise a plain
    /// key is evil-owned, so an unbound one is a no-op (vim doesn't self-insert
    /// in normal/visual) — UNLESS it continues an Emacs prefix already in
    /// progress (so `C-x b`, `C-x k`, … still work); a Ctrl/Alt chord evil
    /// doesn't bind falls through to the Emacs dispatch, so unshadowed
    /// `C-x C-s`, `C-g`, `M-x`, … keep working. Platform (cmd) chords always
    /// fall through to the OS / Emacs.
    ///
    /// Special read-only buffers (magit, *Help*, …) are skipped: they are
    /// already fully modal via their own keymap, which binds plain keys like
    /// `s` (stage) that evil-normal doesn't know about. Without this guard the
    /// evil intercept would swallow those keys before the special-buffer layer
    /// ever runs. Their keymap takes precedence over evil.
    fn key_evil_modal(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if !self.evil_intercepts() || self.active.special.is_some() || ks.modifiers.platform {
            return Handled::No;
        }
        let chord = chord_string(ks);
        let candidate = if self.pending_keys.is_empty() {
            chord.clone()
        } else {
            format!("{} {}", self.pending_keys.join(" "), chord)
        };
        // The active major mode (top of the stack) scopes bindings, so an
        // `evil-normal:rust` keymap can shadow the global `evil-normal`.
        let state = self.evil_keymap();
        let major: Option<String> = self.active.modes.names().last().map(|s| s.to_string());
        if let Some(expr) = self
            .keymap
            .resolve_evil(state, major.as_deref(), &candidate)
            .map(str::to_string)
        {
            self.pending_keys.clear();
            self.status = SharedString::default();
            self.dispatch_recorded(&expr, cx);
            cx.stop_propagation();
            return Handled::Yes;
        }
        if self
            .keymap
            .is_prefix_evil(state, major.as_deref(), &candidate)
        {
            self.pending_keys.push(chord);
            self.status = format!("{}-", pretty_sequence(&candidate)).into();
            cx.notify();
            cx.stop_propagation();
            return Handled::Yes;
        }
        // No evil binding/prefix for this sequence. A plain (non-Ctrl/Alt) key
        // is evil-owned: swallow it as a no-op — unless it's completing an
        // Emacs prefix already pending (then let the Emacs dispatch finish it).
        // Ctrl/Alt chords always fall through so unshadowed Emacs keys work.
        if !ks.modifiers.control && !ks.modifiers.alt {
            let continues_emacs = !self.pending_keys.is_empty()
                && (self
                    .keymap
                    .resolve(&self.active.modes, &candidate)
                    .is_some()
                    || self.keymap.is_prefix(&self.active.modes, &candidate));
            if !continues_emacs {
                self.pending_keys.clear();
                cx.notify();
                cx.stop_propagation();
                return Handled::Yes;
            }
        }
        // Fall through to the Emacs dispatch with `pending_keys` intact.
        Handled::No
    }

    /// Evil Insert: a chord bound in `evil-insert` (e.g. Escape → normal) wins;
    /// otherwise fall through (plain keys self-insert, C-x etc. still work).
    fn key_evil_insert(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.evil == EvilState::Insert
            && self.pending_keys.is_empty()
            && let Some(expr) = self
                .keymap
                .resolve_mode(self.evil_insert_keymap(), &chord_string(ks))
                .map(str::to_string)
        {
            self.run_script(&expr, cx);
            cx.stop_propagation();
            return Handled::Yes;
        }
        Handled::No
    }

    /// Under evil, a special buffer may bind a Ctrl/Alt chord in its
    /// `evil-normal:<major>` overlay (e.g. `C-d`/`C-u` half-page scroll in a
    /// *compilation* window). Consult that overlay FIRST for a lone chord; an
    /// unbound one falls through to the normal dispatch, so C-x C-c, C-g, and
    /// any ctrl-bound mode keys still work. Additive: nothing binds ctrl chords
    /// here unless the evil layer opted in.
    fn key_special_chord_overlay(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.active.special.is_some()
            && self.evil_intercepts()
            && (ks.modifiers.control || ks.modifiers.alt)
            && !ks.modifiers.platform
            && self.pending_keys.is_empty()
            && let Some(km) = self
                .active
                .modes
                .names()
                .last()
                .map(|m| format!("{}:{m}", self.modal_collection_prefix()))
            && let Some(expr) = self
                .keymap
                .resolve_mode(&km, &chord_string(ks))
                .map(str::to_string)
        {
            self.dispatch_recorded(&expr, cx);
            cx.stop_propagation();
            return Handled::Yes;
        }
        Handled::No
    }

    /// Special read-only buffers (magit-style): every plain key is a command
    /// resolved against the buffer's keymap — never self-insert. Ctrl/Alt
    /// chords fall through to the normal dispatch (so C-x C-c, C-g, and any
    /// ctrl-bound mode keys still work). Mirrors Evil Normal.
    fn key_special_buffer(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.active.special.is_none()
            || ks.modifiers.control
            || ks.modifiers.alt
            || ks.modifiers.platform
        {
            return Handled::No;
        }
        self.pending_keys.push(chord_string(ks));
        let sequence = self.pending_keys.join(" ");
        // Emacs keybindings are the default: plain (non-modal) editing
        // resolves straight against the buffer's own keymap, untouched. Under
        // evil-mode or helix-mode ONLY, a `<style>-normal:<major>` keymap is
        // consulted FIRST (see `modal_collection_prefix`), so the Scheme modal
        // layer (`evil-collection-init` / `helix-collection-init`) can overlay
        // vim/helix keys on these read-only buffers — magit, dired, … —
        // without disturbing their Emacs-default keymaps. The overlay is pure
        // policy in Scheme; this is only the mechanism. Inert when modal off.
        let scoped: Option<String> = self
            .evil_intercepts()
            .then(|| {
                self.active
                    .modes
                    .names()
                    .last()
                    .map(|m| format!("{}:{m}", self.modal_collection_prefix()))
            })
            .flatten();
        if let Some(expr) = scoped
            .as_deref()
            .and_then(|m| self.keymap.resolve_mode(m, &sequence))
            .map(str::to_string)
        {
            self.pending_keys.clear();
            self.status = SharedString::default();
            self.dispatch_recorded(&expr, cx);
        } else if scoped
            .as_deref()
            .is_some_and(|m| self.keymap.is_prefix_mode(m, &sequence))
        {
            // The evil overlay has a LONGER binding for this sequence (e.g.
            // `g g` / `g r` in magit). Pend for the next key even though the
            // base keymap may bind this exact key on its own (single `g` =
            // refresh). This is how evil-collection turns `g` into a prefix
            // in magit without disturbing the Emacs-default keymap beneath —
            // checked before the base exact-match below so the prefix wins.
            self.status = format!("{}-", pretty_sequence(&sequence)).into();
            cx.notify();
        } else if let Some(expr) = self
            .keymap
            .resolve(&self.active.modes, &sequence)
            .map(str::to_string)
        {
            self.pending_keys.clear();
            self.status = SharedString::default();
            self.dispatch_recorded(&expr, cx);
        } else if self.keymap.is_prefix(&self.active.modes, &sequence) {
            self.status = format!("{}-", pretty_sequence(&sequence)).into();
            cx.notify();
        } else {
            self.pending_keys.clear();
            cx.notify();
        }
        cx.stop_propagation();
        Handled::Yes
    }

    /// Emacs universal-argument: `C-u` and `M-<digit>` begin a numeric prefix
    /// that the next command consumes as a repeat count. While the argument
    /// is being read, digits (and further `C-u`) are swallowed here; any
    /// other key ends the argument and falls through to run as the command.
    fn key_universal_argument(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.handle_prefix_arg(ks, cx) {
            cx.stop_propagation();
            return Handled::Yes;
        }
        Handled::No
    }

    /// The final catch-all: platform (cmd) chords, then ordinary Emacs prefix
    /// sequences resolved against the mode keymaps. Plain printable keys are
    /// left to the IME (self-insert) and OS-special keys to the GPUI actions.
    fn key_emacs(&mut self, ks: &Keystroke, cx: &mut Context<Self>) {
        let is_text = ks.key_char.is_some()
            && !ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform;

        // Platform (cmd) chords: dispatch when bound (e.g. cmd-c/x/v), otherwise
        // fall through to the OS WITHOUT consuming the event, so system shortcuts
        // (cmd-q, cmd-w, cmd-m, cmd-h, …) still work. No multi-key cmd sequences.
        if self.pending_keys.is_empty() && ks.modifiers.platform {
            let chord = chord_string(ks);
            if let Some(expr) = self
                .keymap
                .resolve(&self.active.modes, &chord)
                .map(str::to_string)
            {
                self.dispatch_recorded(&expr, cx);
                cx.stop_propagation();
            }
            return;
        }

        if self.pending_keys.is_empty() {
            if is_text {
                return; // self-insert via the IME
            }
            if !(ks.modifiers.control || ks.modifiers.alt) {
                return; // OS-special key; handled as a GPUI action
            }
        }

        self.pending_keys.push(chord_string(ks));
        let sequence = self.pending_keys.join(" ");

        if let Some(expr) = self
            .keymap
            .resolve(&self.active.modes, &sequence)
            .map(str::to_string)
        {
            self.pending_keys.clear();
            self.status = SharedString::default(); // clear any prefix echo
            self.dispatch_recorded(&expr, cx);
        } else if self.keymap.is_prefix(&self.active.modes, &sequence) {
            self.status = format!("{}-", pretty_sequence(&sequence)).into();
            cx.notify();
        } else {
            self.status = format!("{} is undefined", pretty_sequence(&sequence)).into();
            self.pending_keys.clear();
            cx.notify();
        }
        cx.stop_propagation();
    }

    // --- Mouse ------------------------------------------------------------

    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A click inside the completion popup picks that candidate (rather than
        // dismissing the popup and moving the cursor).
        if let Some(hit) = self.completion_popup_hit
            && hit.bounds.contains(&event.position)
        {
            let row = ((event.position.y - hit.bounds.origin.y) / hit.row_height) as usize;
            if row < hit.rows {
                self.completion_click(hit.start + row, cx);
                return;
            }
        }
        self.completion_popup = None;
        self.completion_popup_hit = None;
        // A click on a gutter fold chevron toggles that line's fold. Policy lives
        // in Scheme: we just route the line to `fold-toggle-at-line`. Checked
        // before cursor positioning so the click doesn't also move point.
        if let Some(line) = self
            .fold_hitboxes
            .iter()
            .find(|h| h.bounds.contains(&event.position))
            .map(|h| h.line)
        {
            self.run_script(&format!("(fold-toggle-at-line {line})"), cx);
            return;
        }
        // A click in a terminal pane starts a grid text selection (and focuses it).
        if self.terminal_selection_begin(event.position, cx) {
            return;
        }
        // A click in another pane focuses it (Emacs: mouse selects the window);
        // for a normal buffer the cursor is positioned by a *subsequent* click.
        // Special buffers (dired-sidebar/dired/…) act on the first click, so fall
        // through to position the cursor and run their `mouse-1` binding below.
        if let Some(pane) = self.pane_at_position(event.position)
            && pane != self.selected_pane
        {
            self.select_pane(pane, cx);
            if self.active.special.is_none() {
                return;
            }
        }
        self.is_selecting = true;
        self.active.mark_active = false;
        // In a special (read-only) buffer, a plain left click acts like a key:
        // position the cursor on the clicked line, then resolve a `mouse-1`
        // binding against the mode keymap and run it (dired-sidebar-visit, etc.). The
        // line is mapped from pane geometry rather than `self.lines`, so a click
        // that *also* focuses the pane (cursor was in another buffer) still lands
        // on the right line this frame instead of needing a second click.
        if self.active.special.is_some() && !event.modifiers.shift {
            // Normally map the click through the painted line layout (handles
            // wrapped lines exactly). On the frame a click *also* focuses this
            // pane, `self.lines` was just cleared and isn't repainted yet, so fall
            // back to mapping straight from pane geometry.
            let offset = if self.lines.is_empty() {
                // `self.line_height` is the real height from the last paint;
                // `window.line_height()` in an event handler is the unstyled
                // default and would scale every row off (clicks land one line up).
                self.line_start_for_click_y(event.position.y, self.line_height)
            } else {
                Some(self.offset_for_position(event.position))
            };
            if let Some(offset) = offset {
                self.move_to(offset, cx);
            }
            if let Some(expr) = self
                .keymap
                .resolve(&self.active.modes, "mouse-1")
                .map(str::to_string)
            {
                self.dispatch_recorded(&expr, cx);
            }
            return;
        }
        let offset = self.offset_for_position(event.position);
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Dragging a split divider: set the split's ratio from the pointer's
        // position within the split's bounds (union of the panes under it).
        if let Some(drag) = self.resizing.clone() {
            let mut leaves = Vec::new();
            if let Some(node) = node_at(&self.layout, &drag.path) {
                layout_leaves(node, &mut leaves);
            }
            let rects: Vec<Bounds<Pixels>> = leaves
                .iter()
                .filter_map(|id| {
                    self.pane_bounds
                        .iter()
                        .find(|(p, _)| p == id)
                        .map(|(_, b)| *b)
                })
                .collect();
            if let Some(area) = union_bounds(&rects) {
                let pos = event.position;
                let ratio = if drag.vertical {
                    (f32::from(pos.y) - f32::from(area.top()))
                        / f32::from(area.size.height).max(1.0)
                } else {
                    (f32::from(pos.x) - f32::from(area.left()))
                        / f32::from(area.size.width).max(1.0)
                };
                if set_ratio_at(&mut self.layout, &drag.path, ratio) {
                    cx.notify();
                }
            }
            return;
        }
        if self.terminal_drag.is_some() {
            self.terminal_selection_drag(event.position, cx);
            return;
        }
        if self.is_selecting {
            let offset = self.offset_for_position(event.position);
            self.select_to(offset, cx);
            // If the pointer is near/past the top or bottom edge, keep scrolling
            // (and extending the selection) until it comes back inside or the
            // drag ends — driven by a self-rescheduling timer so it continues
            // even while the pointer is held still.
            self.autoscroll.pos = event.position;
            self.autoscroll.dy = self.drag_autoscroll_velocity(event.position);
            if self.autoscroll.dy != 0.0 && !self.autoscroll.active {
                self.autoscroll.active = true;
                self.schedule_drag_autoscroll(cx);
            }
            return;
        }
        // Hover: echo the diagnostic under the pointer (only over the selected
        // pane, so an adjacent pane's text doesn't show the wrong message).
        let over_selected = self
            .pane_at_position(event.position)
            .is_none_or(|p| p == self.selected_pane);
        let message = if over_selected {
            let offset = self.offset_for_position(event.position);
            self.diagnostic_at_offset(offset)
        } else {
            None
        };
        match message {
            Some(m) => {
                // Only re-render when the hovered message actually changes.
                let changed = self
                    .hover_message
                    .as_ref()
                    .is_none_or(|s| s.as_ref() != m.as_str());
                if changed {
                    self.status = m.clone().into();
                    self.hover_message = Some(m.into());
                    cx.notify();
                }
            }
            None => {
                if let Some(prev) = self.hover_message.take() {
                    if self.status == prev {
                        self.status = SharedString::default();
                    }
                    cx.notify();
                }
            }
        }
    }

    pub(super) fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        // The autoscroll timer chain checks `is_selecting` and stops itself.
        self.autoscroll.dy = 0.0;
        self.resizing = None;
        // End any terminal drag; the selection stays so it can be copied.
        self.terminal_drag = None;
    }

    /// Per-tick vertical autoscroll velocity (signed pixels) for a drag-select,
    /// given the pointer position. Zero when the pointer is comfortably inside
    /// the text area; otherwise it ramps up the further past the edge the
    /// pointer is, so flicking well below the view scrolls faster than hugging
    /// the edge. Negative scrolls up, positive scrolls down.
    pub(super) fn drag_autoscroll_velocity(&self, pos: Point<Pixels>) -> f32 {
        let Some(bounds) = self.last_bounds else {
            return 0.0;
        };
        let lh = f32::from(self.line_height).max(1.0);
        // Start scrolling once the pointer is within a line of the edge (so you
        // don't have to push fully past the window), ramping over ~4 lines.
        let margin = lh;
        let ramp = lh * 4.0;
        let min_speed = lh * 0.4;
        let max_speed = lh * 2.5;
        let speed = |over: f32| (min_speed + (over / ramp) * max_speed).min(max_speed);
        let y = f32::from(pos.y);
        let top = f32::from(bounds.top());
        let bottom = f32::from(bounds.bottom());
        if y > bottom - margin {
            speed(y - (bottom - margin))
        } else if y < top + margin {
            -speed((top + margin) - y)
        } else {
            0.0
        }
    }

    /// Schedule the next drag-autoscroll tick. Each tick re-checks that a drag
    /// is still active and the pointer is still off the edge; otherwise it stops
    /// the chain. Mirrors [`schedule_blink`]'s self-rescheduling timer.
    pub(super) fn schedule_drag_autoscroll(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(Duration::from_millis(16));
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |editor, cx| {
                if !editor.is_selecting || editor.autoscroll.dy == 0.0 {
                    editor.autoscroll.active = false;
                    return;
                }
                if editor.apply_drag_autoscroll(cx) {
                    editor.schedule_drag_autoscroll(cx);
                } else {
                    // Hit the top/bottom of the buffer — nothing left to reveal.
                    editor.autoscroll.active = false;
                }
            })
            .ok();
        })
        .detach();
    }

    /// Apply one autoscroll step: scroll by the current velocity, then re-extend
    /// the selection to the (now newly revealed) line under the last pointer
    /// position. Returns `false` when the view can't scroll any further in that
    /// direction, so the caller can stop the timer chain.
    pub(super) fn apply_drag_autoscroll(&mut self, cx: &mut Context<Self>) -> bool {
        let vh = f32::from(self.viewport_height);
        let max = self.max_scroll_offset(vh);
        let cur = f32::from(self.active.scroll_offset);
        let next = (cur + self.autoscroll.dy).clamp(0.0, max);
        if (next - cur).abs() < 0.01 {
            return false;
        }
        self.active.scroll_offset = px(next);
        // The pointer sits past the edge, so this hit-tests to the first/last
        // visible line; after the scroll + repaint that line has moved, so the
        // selection grows by roughly the scrolled distance each tick.
        let offset = self.offset_for_position(self.autoscroll.pos);
        self.select_to(offset, cx);
        cx.notify();
        true
    }

    pub(super) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = window.line_height();
        let pixel_delta = event.delta.pixel_delta(line_height);
        // Over a terminal pane, the wheel scrolls the terminal's scrollback.
        if let Some(pane) = self.pane_at_position(event.position)
            && let Some(tid) = self.pane_terminal_id(pane)
        {
            let lines = (f32::from(pixel_delta.y) / f32::from(line_height)).round() as i32;
            if lines != 0 {
                self.terminal_scroll(tid, lines, cx);
            }
            return;
        }
        // `window.line_height()` above is only for the wheel's line→pixel delta
        // conversion; the scroll CLAMP uses the shared display-row helper (real
        // painted line height, folds collapsed) so the wheel can't over-scroll
        // into blank space below a folded buffer.
        let vh = f32::from(self.viewport_height);
        let max = self.max_scroll_offset(vh);
        // A downward wheel delta is negative, which scrolls content up.
        let scroll =
            (f32::from(self.active.scroll_offset) - f32::from(pixel_delta.y)).clamp(0.0, max);
        self.active.scroll_offset = px(scroll);
        // Horizontal scrolling only when word-wrap is off (otherwise lines never
        // extend past the right edge). The clamp upper bound is generous — the
        // true line width isn't known here — and h-scroll is re-clamped to the
        // widest visible line during paint.
        if !self.word_wrap {
            let h = (f32::from(self.h_scroll_offset) - f32::from(pixel_delta.x)).max(0.0);
            self.h_scroll_offset = px(h);
        }
        cx.notify();
    }

    // --- GPUI action handlers (OS-special keys -> commands) ----------------

    pub(super) fn act_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.transient.is_some() {
            // transient is modal; ignore editing keys
        } else if self.helm.is_some() {
            self.helm_backspace(cx);
        } else if self.minibuffer.is_some() {
            self.mb_backspace(cx);
        } else if self.terminal_action_key("backspace", cx) {
            // terminal: sent to the shell (DEL)
        } else if self.special_key("backspace", cx) {
            // read-only buffer: consumed (ran any binding)
        } else if self.evil_intercepts() {
            self.apply_command(EditorCommand::BackwardChar, cx); // vim: move left
        } else {
            self.apply_command(EditorCommand::DeleteBackwardChar, cx);
            if self.completion_popup.is_some() {
                self.popup_refilter(cx);
            }
        }
    }
    pub(super) fn act_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.transient.is_some() {
            // transient is modal; ignore editing keys
        } else if self.helm.is_some() {
            // Helm is modal (mirrors act_backspace): its input has no
            // forward-delete, but the key must not edit the buffer underneath.
        } else if self.minibuffer.is_some() {
            self.mb_delete(cx);
        } else if self.terminal_action_key("delete", cx) {
            // terminal: sent to the shell
        } else if self.special_key("delete", cx) {
            // read-only buffer: consumed
        } else {
            // No `evil_intercepts` branch on purpose: in vim, Del in Normal mode
            // deletes the char under the cursor (like `x`), unlike Backspace
            // which only moves left — see act_backspace.
            self.apply_command(EditorCommand::DeleteChar, cx);
        }
    }
    pub(super) fn act_left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.completion_popup = None; // horizontal motion dismisses the popup
        if self.minibuffer.is_some() {
            self.mb_left(cx);
        } else if self.terminal_action_key("left", cx) {
            // terminal: arrow sent to the shell
        } else {
            self.apply_command(EditorCommand::BackwardChar, cx);
        }
    }
    pub(super) fn act_right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.completion_popup = None; // horizontal motion dismisses the popup
        if self.minibuffer.is_some() {
            self.mb_right(cx);
        } else if self.terminal_action_key("right", cx) {
            // terminal: arrow sent to the shell
        } else {
            self.apply_command(EditorCommand::ForwardChar, cx);
        }
    }
    pub(super) fn act_up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.helm.is_some() {
            self.helm_previous(cx);
            return;
        }
        if self.popup_special_key("up", EditorCommand::CompletionPrevious, cx) {
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_history(-1, cx); // older
        } else if self.terminal_action_key("up", cx) {
            // terminal: arrow sent to the shell (history)
        } else {
            self.apply_command(EditorCommand::PreviousLine, cx);
        }
    }
    pub(super) fn act_down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.helm.is_some() {
            self.helm_next(cx);
            return;
        }
        if self.popup_special_key("down", EditorCommand::CompletionNext, cx) {
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_history(1, cx); // newer
        } else if self.terminal_action_key("down", cx) {
            // terminal: arrow sent to the shell (history)
        } else {
            self.apply_command(EditorCommand::NextLine, cx);
        }
    }
    pub(super) fn act_home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        if self.minibuffer.is_some() {
            self.mb_move(0, cx);
        } else if self.terminal_action_key("home", cx) {
            // terminal: sent to the shell
        } else {
            self.apply_command(EditorCommand::BeginningOfLine, cx);
        }
    }
    pub(super) fn act_end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(len) = self.minibuffer.as_ref().map(|mb| mb.input.len()) {
            self.mb_move(len, cx);
        } else if self.terminal_action_key("end", cx) {
            // terminal: sent to the shell
        } else {
            self.apply_command(EditorCommand::EndOfLine, cx);
        }
    }
    pub(super) fn act_newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.transient.is_some() {
            return; // transient is modal
        }
        if self.helm.is_some() {
            self.helm_accept(cx);
            return;
        }
        if self.popup_special_key("enter", EditorCommand::CompletionAccept, cx) {
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_submit(cx);
        } else if self.terminal_action_key("enter", cx) {
            // terminal: RET sent to the shell (carriage return)
        } else if self.special_key("enter", cx) {
            // read-only buffer: RET runs its binding (e.g. visit/expand)
        } else if !self.evil_intercepts() && !self.mode_key("enter", cx) {
            // a mode may rebind RET (comint send-input); else insert a newline
            self.apply_command(EditorCommand::Newline, cx);
        }
    }
    pub(super) fn act_tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.transient.is_some() {
            return; // transient is modal
        }
        if self.helm.is_some() {
            self.helm_complete(cx); // TAB → complete to selection / descend folders
            return;
        }
        if self.popup_special_key("tab", EditorCommand::CompletionNext, cx) {
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_complete(cx);
        } else if self.terminal_action_key("tab", cx) {
            // terminal: TAB sent to the shell (completion)
        } else if self.special_key("tab", cx) {
            // read-only buffer: TAB runs its binding (e.g. toggle section)
        } else if !self.evil_intercepts() && !self.mode_key("tab", cx) {
            self.apply_command(EditorCommand::IndentForTab, cx);
        }
    }
    pub(super) fn act_select_left(
        &mut self,
        _: &SelectLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.minibuffer.is_none() {
            self.apply_command(EditorCommand::ExtendBackwardChar, cx);
        }
    }
    pub(super) fn act_select_right(
        &mut self,
        _: &SelectRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.minibuffer.is_none() {
            self.apply_command(EditorCommand::ExtendForwardChar, cx);
        }
    }
    pub(super) fn act_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.minibuffer.is_none() {
            self.apply_command(EditorCommand::MarkWholeBuffer, cx);
        }
    }
}
