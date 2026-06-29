//! Subprocesses and embedded terminals for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use futures::StreamExt;
use gpui::{Context, Pixels, Point};

use crate::terminal::{TermEvent, Terminal};

use super::*;

impl Editor {
    /// Spawn `program` asynchronously (Emacs `make-process`): a worker thread
    /// reads its stdout line by line and forwards each line to the foreground,
    /// which calls the Steel `on_output` fn with it; `on_exit` runs when the
    /// process finishes. The UI never blocks, and the callbacks' commands flow
    /// through the normal outcome drain — so `(append-to-buffer …)` etc. work.
    pub(super) fn make_process(
        &mut self,
        id: u64,
        program: String,
        args: Vec<String>,
        on_output: String,
        on_exit: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // Spawn here (not on the worker) so we can keep the child for stdin/kill.
        let mut child = match Command::new(&program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                self.echo(format!("make-process: {e}"), cx);
                return;
            }
        };
        let stdout = child.stdout.take();
        let stdin = child.stdin.take();
        self.processes.insert(id, ProcessHandle { child, stdin });

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            if let Some(out) = stdout {
                for line in BufReader::new(out).lines() {
                    let Ok(l) = line else { break };
                    if tx.unbounded_send(ProcMsg::Line(l)).is_err() {
                        break;
                    }
                }
            }
            // The editor wait()s for the exit code on Exit.
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| match &msg {
                        ProcMsg::Line(line) => {
                            editor.script.dispatch_action(&on_output, Some(line))
                        }
                        ProcMsg::Exit => {
                            let code = editor
                                .processes
                                .remove(&id)
                                .and_then(|mut h| h.child.wait().ok())
                                .and_then(|status| status.code())
                                .unwrap_or(-1);
                            if let Some(cb) = &on_exit {
                                editor.script.dispatch_action(cb, Some(&code.to_string()));
                            }
                        }
                    })
                    .is_ok();
                if !alive || matches!(msg, ProcMsg::Exit) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Write `text` to a live process's stdin (Emacs `process-send-string`).
    pub(super) fn send_to_process(&mut self, id: u64, text: &str, cx: &mut Context<Self>) {
        if let Some(handle) = self.processes.get_mut(&id)
            && let Some(stdin) = handle.stdin.as_mut()
            && let Err(e) = stdin.write_all(text.as_bytes())
        {
            self.echo(format!("process-send-string: {e}"), cx);
        }
    }

    /// Kill a live process; its reader thread then reports exit (Emacs `kill-process`).
    pub(super) fn kill_process(&mut self, id: u64) {
        if let Some(handle) = self.processes.get_mut(&id) {
            let _ = handle.child.kill();
        }
    }

    /// Open a streaming HTTP POST (`make-http-stream`): a worker thread sends the
    /// request with `ureq` (blocking, rustls TLS), then reads the response body
    /// line by line and forwards each line to the foreground, which calls the
    /// Steel `on_line` fn with it. `on_done` runs with the stream id when the body
    /// closes. The HTTP analogue of [`make_process`](Self::make_process) — same
    /// channel + `cx.spawn` drain, so the callbacks' editor commands flow through
    /// the normal outcome drain (`append-to-buffer`, etc.). The UI never blocks.
    ///
    /// A non-2xx status surfaces its body as one `error: HTTP <code>: <body>`
    /// line; a transport failure as `error: <e>`. Cancellation flips the shared
    /// flag (see [`http_stream_cancel`](Self::http_stream_cancel)); the reader
    /// checks it between lines and drops the connection.
    pub(super) fn http_stream(
        &mut self,
        id: u64,
        url: String,
        headers: Vec<(String, String)>,
        body: String,
        on_line: String,
        on_done: String,
        cx: &mut Context<Self>,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let cancel = Arc::new(AtomicBool::new(false));
        self.http_streams.insert(id, cancel.clone());

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            // No read timeout: a model stream can pause between tokens. A connect
            // timeout still bounds the initial handshake.
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(15))
                .build();
            let mut req = agent.post(&url);
            for (k, v) in &headers {
                req = req.set(k, v);
            }
            match req.send_string(&body) {
                Ok(resp) => {
                    for line in BufReader::new(resp.into_reader()).lines() {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok(l) = line else { break };
                        if tx.unbounded_send(ProcMsg::Line(l)).is_err() {
                            break;
                        }
                    }
                }
                // Non-2xx: ureq returns the response so we can relay the API's own
                // error JSON (e.g. a 401 from a bad key) into the transcript.
                Err(ureq::Error::Status(code, resp)) => {
                    let detail = resp.into_string().unwrap_or_default();
                    let _ =
                        tx.unbounded_send(ProcMsg::Line(format!("error: HTTP {code}: {detail}")));
                }
                Err(e) => {
                    let _ = tx.unbounded_send(ProcMsg::Line(format!("error: {e}")));
                }
            }
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });

        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| match &msg {
                        ProcMsg::Line(line) => editor.script.dispatch_action(&on_line, Some(line)),
                        ProcMsg::Exit => {
                            editor.http_streams.remove(&id);
                            if !on_done.is_empty() {
                                editor
                                    .script
                                    .dispatch_action(&on_done, Some(&id.to_string()));
                            }
                        }
                    })
                    .is_ok();
                if !alive || matches!(msg, ProcMsg::Exit) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Cancel an in-flight HTTP stream (`http-stream-cancel`): flip its flag so the
    /// reader thread stops forwarding lines and drops the connection on its next
    /// loop. The `on_done` callback still fires when the body closes.
    pub(super) fn http_stream_cancel(&mut self, id: u64) {
        if let Some(flag) = self.http_streams.get(&id) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // --- Embedded terminal ------------------------------------------------

    /// Spawn a terminal (`make-terminal`): start the PTY/shell and show it in a
    /// read-only `term-mode` buffer named `name`. If a terminal buffer of that
    /// name already exists, just switch to it (reuse the running shell).
    pub(super) fn make_terminal(
        &mut self,
        id: u64,
        name: String,
        program: String,
        args: Vec<String>,
        cx: &mut Context<Self>,
    ) {
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
        let cwd = self
            .active_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);

        match Terminal::new(
            id,
            name.clone(),
            cols,
            rows,
            cell_w,
            cell_h,
            program,
            cwd,
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
            .find(|t| t.buffer_name == self.active_name)
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
    ) -> Option<(usize, usize, bool)> {
        let g = self.terminal_geom.get(&id)?;
        let cw = f32::from(g.cell_w).max(1.0);
        let ch = f32::from(g.cell_h).max(1.0);
        let relx = (f32::from(pos.x) - f32::from(g.origin.x)) / cw;
        let rely = (f32::from(pos.y) - f32::from(g.origin.y)) / ch;
        let col = (relx.floor().max(0.0) as usize).min(g.cols.saturating_sub(1));
        let row = (rely.floor().max(0.0) as usize).min(g.rows.saturating_sub(1));
        let right = (relx - relx.floor()) >= 0.5;
        Some((row, col, right))
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
        if let Some((row, col, right)) = self.terminal_cell_at(tid, pos)
            && let Some(t) = self.terminals.get(&tid)
        {
            t.selection_start(row, col, right);
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
        if let Some((row, col, right)) = self.terminal_cell_at(tid, pos)
            && let Some(t) = self.terminals.get(&tid)
        {
            t.selection_update(row, col, right);
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
                if let Some(t) = self.terminals.get_mut(&id) {
                    t.exited = true;
                    let name = t.buffer_name.clone();
                    self.echo(format!("Terminal {name} exited"), cx);
                }
            }
        }
        cx.notify();
    }
}
