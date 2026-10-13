//! Embedded terminals for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use gpui::{Context, Pixels, Point};

use crate::terminal::{Side, TermEvent, Terminal};

use super::*;

/// Encode a [`Keystroke`] as the bytes a PTY expects, for the embedded terminal.
/// Returns `None` for keystrokes that produce nothing (e.g. an unhandled control
/// chord), which are then swallowed. Meta (`alt`) is sent as an ESC prefix.
pub(super) fn keystroke_to_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let key = ks.key.as_str();

    // Named / navigation keys → control bytes or CSI sequences.
    let named: Option<Vec<u8>> = match key {
        "enter" | "return" => Some(vec![b'\r']),
        "escape" => Some(vec![0x1b]),
        "backspace" => Some(vec![0x7f]),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "tab" => Some(if m.shift {
            b"\x1b[Z".to_vec()
        } else {
            vec![b'\t']
        }),
        "up" => Some(b"\x1b[A".to_vec()),
        "down" => Some(b"\x1b[B".to_vec()),
        "right" => Some(b"\x1b[C".to_vec()),
        "left" => Some(b"\x1b[D".to_vec()),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        _ => None,
    };
    if let Some(named) = named {
        return Some(named);
    }

    // Control chords → C0 control bytes (C-a = 0x01, … C-/ = 0x1f).
    if m.control {
        let byte = match key {
            "space" | "@" => Some(0x00u8),
            k if k.len() == 1 => match k.as_bytes()[0] {
                c @ b'a'..=b'z' => Some(c - b'a' + 1),
                b'[' => Some(0x1b),
                b'\\' => Some(0x1c),
                b']' => Some(0x1d),
                b'^' => Some(0x1e),
                b'_' | b'/' => Some(0x1f),
                _ => None,
            },
            _ => None,
        };
        return byte.map(|b| if m.alt { vec![0x1b, b] } else { vec![b] });
    }

    // Printable text (shifted char from the IME, else a single-char key name).
    let text = ks
        .key_char
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| (key.chars().count() == 1).then(|| key.to_string()))?;
    let mut bytes = text.into_bytes();
    if m.alt {
        let mut v = vec![0x1b];
        v.append(&mut bytes);
        return Some(v);
    }
    Some(bytes)
}

impl Editor {
    // --- Embedded terminal ------------------------------------------------

    /// Spawn a terminal (`make-terminal`): start the PTY/shell and show it in a
    /// read-only `term-mode` buffer named `name`. If a terminal buffer of that
    /// name already exists, just switch to it (reuse the running shell).
    pub(super) fn make_terminal(
        &mut self,
        spec: crate::script::MakeTerminalArgs,
        cx: &mut Context<Self>,
    ) {
        let crate::script::MakeTerminalArgs {
            id,
            name,
            program,
            args,
            directory,
            env,
        } = spec;
        if self.terminals.values().any(|t| t.buffer_name == name) {
            self.switch_to_buffer(&name, cx);
            return;
        }

        // Initial grid size from the current cell metrics; the element re-fits it
        // on the first frame, so rough numbers are fine.
        let (cols, rows) = self.terminal_fit_size();
        let cell_w = (f32::from(self.char_width).max(1.0)) as u16;
        let cell_h = (f32::from(self.line_height).max(1.0)) as u16;
        let program = if program.trim().is_empty() {
            None
        } else {
            Some((program, args))
        };
        // Prefer an explicit directory (term.scm passes the current project's
        // root so terminals follow the title-bar project); otherwise fall back
        // to the active buffer's directory, then the process cwd.
        let cwd = if directory.trim().is_empty() {
            self.active
                .path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        } else {
            Some(std::path::PathBuf::from(directory))
        };

        match Terminal::new(
            id,
            name.clone(),
            cols,
            rows,
            cell_w,
            cell_h,
            program,
            cwd,
            &env,
            self.term_events_tx.clone(),
        ) {
            Ok(terminal) => {
                self.terminals.insert(id, terminal);
                // A read-only special buffer drives input routing and the mode
                // line; its rope text is unused (the grid is rendered directly).
                self.render_buffer(name, "term".to_string(), String::new(), Vec::new(), cx);
            }
            Err(e) => self.echo(format!("make-terminal: {e}"), cx),
        }
    }

    /// A starting `(columns, rows)` for a new terminal from the live viewport,
    /// defaulting to 80×24 before the first frame has measured anything.
    pub(super) fn terminal_fit_size(&self) -> (usize, usize) {
        let cw = f32::from(self.char_width);
        let lh = f32::from(self.line_height);
        let cols = if cw > 0.0 {
            (f32::from(self.viewport_width) / cw) as usize
        } else {
            0
        };
        let rows = if lh > 0.0 {
            (f32::from(self.viewport_height) / lh) as usize
        } else {
            0
        };
        // Before the first frame the viewport is unmeasured; fall back to 80×24.
        (
            if cols == 0 { 80 } else { cols },
            if rows == 0 { 24 } else { rows },
        )
    }

    /// The terminal shown in pane `pane`, if its buffer is a terminal buffer.
    pub fn pane_terminal_id(&self, pane: usize) -> Option<u64> {
        let name = self.pane(pane).map(|p| p.buffer.as_str())?;
        self.terminals
            .values()
            .find(|t| t.buffer_name == name)
            .map(|t| t.id)
    }

    /// The terminal in the selected pane (the live one), if any. Includes a
    /// terminal whose shell has exited — its scrollback is still readable, so
    /// copy/scroll keep working on the leftover buffer.
    pub(super) fn active_terminal_id(&self) -> Option<u64> {
        self.terminals
            .values()
            .find(|t| t.buffer_name == self.active.name)
            .map(|t| t.id)
    }

    /// The active terminal only while its shell is still running. Input paths
    /// (key grab, action keys, paste) use this so a dead terminal releases the
    /// keyboard and normal editor dispatch (M-x, C-x k, …) resumes.
    pub(super) fn active_running_terminal_id(&self) -> Option<u64> {
        self.active_terminal_id()
            .filter(|tid| !self.terminals.get(tid).is_some_and(|t| t.exited))
    }

    /// Send already-encoded key bytes to terminal `id`, snapping the view back to
    /// the live region first (typing scrolls out of scrollback).
    pub(super) fn terminal_input(&mut self, id: u64, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if let Some(t) = self.terminals.get(&id) {
            t.selection_clear(); // typing dismisses any mouse selection
            t.scroll_to_bottom();
            t.input(bytes);
            cx.notify();
        }
    }

    /// If a terminal is active, encode the named OS-special key and send it to
    /// the PTY, returning `true`. This is the *only* dispatch path for keys bound
    /// to GPUI actions (Backspace/Enter/Tab/arrows/…): gpui stops propagation
    /// after running an action, so `on_key_down` never sees them — the `act_*`
    /// handlers call this instead. Plain text and Ctrl/Alt chords still go through
    /// `on_key_down`'s own terminal branch.
    pub(super) fn terminal_action_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_running_terminal_id() else {
            return false;
        };
        let bytes: Vec<u8> = match key {
            "enter" => vec![b'\r'],
            "tab" => vec![b'\t'],
            "backtab" => b"\x1b[Z".to_vec(),
            "backspace" => vec![0x7f],
            "delete" => b"\x1b[3~".to_vec(),
            "up" => b"\x1b[A".to_vec(),
            "down" => b"\x1b[B".to_vec(),
            "right" => b"\x1b[C".to_vec(),
            "left" => b"\x1b[D".to_vec(),
            "home" => b"\x1b[H".to_vec(),
            "end" => b"\x1b[F".to_vec(),
            _ => return false,
        };
        self.terminal_input(tid, bytes, cx);
        true
    }

    /// Scroll terminal `id`'s scrollback by `delta` lines (positive = older).
    pub(super) fn terminal_scroll(&mut self, id: u64, delta: i32, cx: &mut Context<Self>) {
        if let Some(t) = self.terminals.get(&id) {
            t.scroll(delta);
            cx.notify();
        }
    }

    /// Record a terminal pane's painted geometry (called by its element each
    /// frame) so mouse positions can be mapped to grid cells.
    pub fn set_terminal_geometry(
        &mut self,
        id: u64,
        origin: Point<Pixels>,
        cell_w: Pixels,
        cell_h: Pixels,
        cols: usize,
        rows: usize,
    ) {
        self.terminal_geom.insert(
            id,
            TermGeom {
                origin,
                cell_w,
                cell_h,
                cols,
                rows,
            },
        );
    }

    /// Map a window position to a `(row, col, right_half)` cell in terminal `id`,
    /// clamped to the grid. `None` if the geometry isn't known yet.
    pub(super) fn terminal_cell_at(
        &self,
        id: u64,
        pos: Point<Pixels>,
    ) -> Option<(usize, usize, Side)> {
        let g = self.terminal_geom.get(&id)?;
        let cw = f32::from(g.cell_w).max(1.0);
        let ch = f32::from(g.cell_h).max(1.0);
        let relx = (f32::from(pos.x) - f32::from(g.origin.x)) / cw;
        let rely = (f32::from(pos.y) - f32::from(g.origin.y)) / ch;
        let col = (relx.floor().max(0.0) as usize).min(g.cols.saturating_sub(1));
        let row = (rely.floor().max(0.0) as usize).min(g.rows.saturating_sub(1));
        let side = if relx - relx.floor() >= 0.5 {
            Side::Right
        } else {
            Side::Left
        };
        Some((row, col, side))
    }

    /// Begin a mouse text selection in the terminal under `pos` (if any). Returns
    /// whether a terminal selection was started.
    pub(super) fn terminal_selection_begin(
        &mut self,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pane) = self.pane_at_position(pos) else {
            return false;
        };
        let Some(tid) = self.pane_terminal_id(pane) else {
            return false;
        };
        if pane != self.selected_pane {
            self.select_pane(pane, cx);
        }
        if let Some((row, col, side)) = self.terminal_cell_at(tid, pos)
            && let Some(t) = self.terminals.get(&tid)
        {
            t.selection_start(row, col, side);
            self.terminal_drag = Some(tid);
            cx.notify();
        }
        true
    }

    /// Extend the active terminal selection to `pos` while dragging.
    pub(super) fn terminal_selection_drag(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(tid) = self.terminal_drag else {
            return;
        };
        if let Some((row, col, side)) = self.terminal_cell_at(tid, pos)
            && let Some(t) = self.terminals.get(&tid)
        {
            t.selection_update(row, col, side);
            cx.notify();
        }
    }

    /// Copy the active terminal's selection to the system clipboard. Returns
    /// whether a terminal was active (so the caller skips the buffer copy path).
    pub(super) fn terminal_copy(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_terminal_id() else {
            return false;
        };
        match self.terminals.get(&tid).and_then(|t| t.selection_text()) {
            Some(text) => {
                self.set_clipboard(&text, cx);
                if let Some(t) = self.terminals.get(&tid) {
                    t.selection_clear();
                }
                self.echo("Copied", cx);
                cx.notify();
            }
            None => self.echo("No selection", cx),
        }
        true
    }

    /// Paste the system clipboard into the active terminal. Returns whether a
    /// terminal was active.
    pub(super) fn terminal_paste(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_running_terminal_id() else {
            return false;
        };
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text())
            && !text.is_empty()
        {
            self.terminal_input(tid, text.into_bytes(), cx);
        }
        true
    }

    /// Re-fit terminal `id` to a measured cell grid (called by the element each
    /// frame); a no-op when the size is unchanged.
    pub fn terminal_resize_fit(
        &mut self,
        id: u64,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
    ) {
        if let Some(t) = self.terminals.get_mut(&id) {
            t.resize(cols, rows, cell_w, cell_h);
        }
    }

    /// A render snapshot of terminal `id`'s visible grid, with theme defaults for
    /// default-colored cells.
    pub fn terminal_snapshot(
        &self,
        id: u64,
        default_fg: u32,
        default_bg: u32,
    ) -> Option<crate::terminal::TermSnapshot> {
        self.terminals
            .get(&id)
            .map(|t| t.snapshot(default_fg, default_bg))
    }

    /// Apply a terminal event from its background event loop.
    pub(super) fn on_terminal_event(&mut self, id: u64, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Wakeup => {}
            TermEvent::Title(title) => {
                if let Some(t) = self.terminals.get_mut(&id) {
                    t.title = title;
                }
            }
            TermEvent::Exit => {
                // alacritty reports a finished child twice (`ChildExit`, then
                // `Exit`); act on the first only, or `terminal-exit-hook` runs
                // twice and its second window dance targets a pane the first
                // already closed.
                if let Some(t) = self.terminals.get_mut(&id)
                    && !t.exited
                {
                    t.exited = true;
                    let name = t.buffer_name.clone();
                    self.echo(format!("Terminal {name} exited"), cx);
                    // Let Scheme react (close the pane, restart, …) — called
                    // with the terminal's buffer name.
                    self.run_hook_arg("terminal-exit-hook", &name);
                }
            }
        }
        cx.notify();
    }
}
