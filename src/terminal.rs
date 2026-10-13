//! Embedded terminal emulator backend, built on `alacritty_terminal`.
//!
//! `alacritty_terminal` gives us the full VT/ANSI core — PTY management, the
//! parser, and the scrollback grid — but no rendering. This module owns one
//! [`Terminal`] per running shell: it spawns the PTY, runs alacritty's event
//! loop on a background thread (reading the PTY, parsing, mutating the grid),
//! and forwards the handful of events the editor cares about (redraw, title,
//! child-exit) over a channel. Rendering is done by `TerminalElement` in
//! `element.rs`, which reads a plain-data [`TermSnapshot`] produced here — so no
//! alacritty types leak past this module and the editor's terminal methods.

use std::sync::Arc;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
/// Which half of a cell a selection endpoint sits on.
pub use alacritty_terminal::index::Side;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use futures::channel::mpsc::UnboundedSender;

/// How many lines of scrollback each terminal keeps.
const SCROLLING_HISTORY: usize = 10_000;

/// Grid dimensions handed to alacritty's `Term` (columns × visible lines). The
/// scrollback history is configured separately, so `total_lines == screen_lines`
/// here — the grid grows its own history as content scrolls off.
#[derive(Clone, Copy)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// The terminal events the editor reacts to. Alacritty raises many more (mouse
/// cursor shape, clipboard, color queries…); we collapse them to the three that
/// matter for a basic embedded terminal and drop the rest.
#[derive(Debug, Clone)]
pub enum TermEvent {
    /// New grid content — the editor should re-render.
    Wakeup,
    /// The program set the window title (e.g. shell `\e]0;…\a`).
    Title(String),
    /// The child process exited; the terminal is now dead.
    Exit,
}

/// Bridges alacritty's event-loop thread to the editor. `send_event` is called
/// from that thread, so it only forwards over the channel (tagged with the
/// terminal id) — the editor drains it on the UI thread and reacts.
#[derive(Clone)]
struct EventProxy {
    id: u64,
    tx: UnboundedSender<(u64, TermEvent)>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        let mapped = match event {
            AlacEvent::Wakeup => TermEvent::Wakeup,
            AlacEvent::Title(t) => TermEvent::Title(t),
            AlacEvent::ResetTitle => TermEvent::Title(String::new()),
            AlacEvent::Exit | AlacEvent::ChildExit(_) => TermEvent::Exit,
            // Bell, clipboard, color requests, cursor-shape, PTY-write callbacks:
            // nothing the basic embedded terminal needs to act on.
            _ => return,
        };
        let _ = self.tx.unbounded_send((self.id, mapped));
    }
}

/// One running terminal: the shared grid, a notifier for input/resize/shutdown,
/// and the current cell-grid dimensions.
pub struct Terminal {
    pub id: u64,
    /// Name of the editor buffer this terminal is displayed in.
    pub buffer_name: String,
    /// The latest window/shell title, shown in the mode line.
    pub title: String,
    /// Set once the child process has exited.
    pub exited: bool,
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    size: TermSize,
}

impl Terminal {
    /// Spawn a shell in a new PTY and start its event loop.
    ///
    /// `columns`/`screen_lines` are the initial cell grid; `cell_width`/
    /// `cell_height` are the pixel size of one cell (so programs that query the
    /// pixel viewport — e.g. image protocols — get sane numbers). `program` of
    /// `None` uses the user's login shell.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        buffer_name: String,
        columns: usize,
        screen_lines: usize,
        cell_width: u16,
        cell_height: u16,
        program: Option<(String, Vec<String>)>,
        working_directory: Option<std::path::PathBuf>,
        extra_env: &crate::process::EnvOverrides,
        tx: UnboundedSender<(u64, TermEvent)>,
    ) -> std::io::Result<Self> {
        let columns = columns.max(1);
        let screen_lines = screen_lines.max(1);

        // Advertise a widely-supported terminal and truecolor via the child's
        // environment (rather than mutating our own process env, which is
        // unsound once threads are running).
        let mut env = std::collections::HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        extra_env.apply_to_map(&mut env);

        let options = PtyOptions {
            shell: program.map(|(p, a)| Shell::new(p, a)),
            working_directory,
            drain_on_exit: false,
            env,
            #[cfg(target_os = "windows")]
            escape_args: true,
        };

        let window_size = WindowSize {
            num_lines: screen_lines as u16,
            num_cols: columns as u16,
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
        };

        let pty = tty::new(&options, window_size, id)?;

        let proxy = EventProxy { id, tx };
        let config = Config {
            scrolling_history: SCROLLING_HISTORY,
            ..Config::default()
        };
        let size = TermSize {
            columns,
            screen_lines,
        };
        let term = Term::new(config, &size, proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        // `drain_on_exit` kept `false` so a finished child stops promptly
        // rather than blocking to flush.
        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)?;
        let notifier = Notifier(event_loop.channel());
        // The join handle is intentionally dropped: the loop runs until it gets a
        // `Shutdown` (sent on `Drop`) or the child exits.
        let _ = event_loop.spawn();

        Ok(Self {
            id,
            buffer_name,
            title: String::new(),
            exited: false,
            term,
            notifier,
            size,
        })
    }

    /// Send raw bytes to the PTY (already-encoded key input).
    pub fn input(&self, bytes: Vec<u8>) {
        self.notifier.notify(bytes);
    }

    /// Scroll the display by `delta` lines (positive = toward older scrollback).
    pub fn scroll(&self, delta: i32) {
        if delta != 0 {
            self.term.lock().scroll_display(Scroll::Delta(delta));
        }
    }

    /// Reset the view to the bottom (live) region — called on input so typing
    /// snaps back from scrollback, matching every real terminal.
    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// Resize the grid to `columns × screen_lines`, no-op if unchanged. Resizes
    /// both the in-memory grid and the PTY (so the child gets `SIGWINCH`).
    pub fn resize(
        &mut self,
        columns: usize,
        screen_lines: usize,
        cell_width: u16,
        cell_height: u16,
    ) {
        let columns = columns.max(1);
        let screen_lines = screen_lines.max(1);
        if columns == self.size.columns && screen_lines == self.size.screen_lines {
            return;
        }
        self.size = TermSize {
            columns,
            screen_lines,
        };
        self.term.lock().resize(self.size);
        self.notifier.on_resize(WindowSize {
            num_lines: screen_lines as u16,
            num_cols: columns as u16,
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
        });
    }

    // --- Mouse selection (grid-based, via alacritty's `Selection`) --------

    /// Begin a text selection at viewport cell `(row, col)`. `side` picks the
    /// half of the cell (so the anchor sits between cells naturally).
    pub fn selection_start(&self, row: usize, col: usize, side: Side) {
        let mut term = self.term.lock();
        let point = self.viewport_point(&term, row, col);
        term.selection = Some(Selection::new(SelectionType::Simple, point, side));
    }

    /// Extend the in-progress selection to viewport cell `(row, col)`.
    pub fn selection_update(&self, row: usize, col: usize, side: Side) {
        let mut term = self.term.lock();
        let point = self.viewport_point(&term, row, col);
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn selection_clear(&self) {
        self.term.lock().selection = None;
    }

    /// The current selection's text, or `None` when there's no (non-empty)
    /// selection.
    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }

    /// Convert a viewport cell to an absolute grid [`Point`], accounting for the
    /// current scrollback offset.
    fn viewport_point(&self, term: &Term<EventProxy>, row: usize, col: usize) -> Point {
        let off = term.grid().display_offset() as i32;
        Point::new(
            Line(row as i32 - off),
            Column(col.min(self.size.columns.saturating_sub(1))),
        )
    }

    /// Build a plain-data snapshot of the visible grid for rendering. `default_fg`
    /// / `default_bg` are the editor theme's terminal colors (`0xRRGGBB`), used
    /// for cells that reference the default foreground/background.
    pub fn snapshot(&self, default_fg: u32, default_bg: u32) -> TermSnapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let dfg = unpack(default_fg);
        let dbg = unpack(default_bg);
        let display_offset = content.display_offset as i32;
        let rows = self.size.screen_lines;
        let cols = self.size.columns;

        let mut cells = Vec::new();
        for indexed in content.display_iter {
            let cell = indexed.cell;
            // The trailing half of a wide (CJK/emoji) glyph carries no char.
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let row = indexed.point.line.0 + display_offset;
            if row < 0 || row as usize >= rows {
                continue;
            }
            let col = indexed.point.column.0;
            if col >= cols {
                continue;
            }

            let inverse = cell.flags.contains(Flags::INVERSE);
            let (mut fg, bg_src) = (cell.fg, cell.bg);
            let mut bg = bg_src;
            if inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut fg_rgb = resolve(fg, content.colors, dfg, dbg);
            if cell.flags.contains(Flags::DIM) {
                fg_rgb = dim(fg_rgb);
            }
            let bg_rgb = resolve(bg, content.colors, dfg, dbg);
            let paint_bg = bg_rgb != dbg;

            let ch = cell.c;
            // Nothing to draw: a blank cell over the default background.
            if ch == ' ' && !paint_bg && !cell.flags.contains(Flags::UNDERLINE) {
                continue;
            }
            cells.push(TermCell {
                col,
                row: row as usize,
                ch,
                fg: pack(fg_rgb),
                bg: if paint_bg { Some(pack(bg_rgb)) } else { None },
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                wide: cell.flags.contains(Flags::WIDE_CHAR),
            });
        }

        // Cursor: only when shown and within the visible region (it falls outside
        // when the user has scrolled up into history).
        let cursor = if matches!(content.cursor.shape, CursorShape::Hidden) {
            None
        } else {
            let crow = content.cursor.point.line.0 + display_offset;
            let ccol = content.cursor.point.column.0;
            if crow >= 0 && (crow as usize) < rows && ccol < cols {
                Some(TermCursor {
                    col: ccol,
                    row: crow as usize,
                    block: !matches!(content.cursor.shape, CursorShape::Beam),
                })
            } else {
                None
            }
        };

        // Selection range, mapped from absolute grid lines to viewport rows
        // (rows may be off-screen — negative or >= rows — and the renderer clamps).
        let selection = content.selection.map(|r| TermSelection {
            start_row: r.start.line.0 + display_offset,
            start_col: r.start.column.0,
            end_row: r.end.line.0 + display_offset,
            end_col: r.end.column.0,
            is_block: r.is_block,
        });

        TermSnapshot {
            cols,
            rows,
            cells,
            cursor,
            selection,
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Ask the event loop to shut down; it closes the PTY and ends the thread.
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// A plain-data render snapshot of the visible terminal grid — no alacritty
/// types, so `element.rs` stays decoupled from the backend.
pub struct TermSnapshot {
    pub cols: usize,
    pub rows: usize,
    /// Only the cells worth drawing (non-blank glyph, non-default background, or
    /// underline). Everything else is the terminal background.
    pub cells: Vec<TermCell>,
    pub cursor: Option<TermCursor>,
    /// The active mouse selection, if any (viewport rows; may extend off-screen).
    pub selection: Option<TermSelection>,
}

/// A selection region in viewport coordinates. Rows are `i32` because the
/// selection may start above or end below the visible area when scrolled.
pub struct TermSelection {
    pub start_row: i32,
    pub start_col: usize,
    pub end_row: i32,
    pub end_col: usize,
    pub is_block: bool,
}

/// One drawable terminal cell at grid position `(col, row)`.
pub struct TermCell {
    pub col: usize,
    pub row: usize,
    pub ch: char,
    /// Foreground color, `0xRRGGBB`.
    pub fg: u32,
    /// Background color, `0xRRGGBB`; `None` means the default terminal background.
    pub bg: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// A double-width (CJK/emoji) glyph: it owns this column *and* the next,
    /// whose spacer cell is omitted from the snapshot.
    pub wide: bool,
}

pub struct TermCursor {
    pub col: usize,
    pub row: usize,
    /// Block cursor (vs. a thin beam).
    pub block: bool,
}

// --- Color resolution -------------------------------------------------------

fn pack((r, g, b): (u8, u8, u8)) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

fn unpack(c: u32) -> (u8, u8, u8) {
    ((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Dim a color to ~2/3 brightness (the SGR 2 "faint" attribute).
fn dim((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    (
        (r as u16 * 2 / 3) as u8,
        (g as u16 * 2 / 3) as u8,
        (b as u16 * 2 / 3) as u8,
    )
}

/// Resolve an ANSI [`Color`](AnsiColor) to concrete RGB, honoring any palette
/// overrides the program installed (OSC 4) and falling back to the standard
/// xterm-256 palette / the theme defaults.
fn resolve(
    color: AnsiColor,
    palette: &Colors,
    dfg: (u8, u8, u8),
    dbg: (u8, u8, u8),
) -> (u8, u8, u8) {
    match color {
        AnsiColor::Spec(Rgb { r, g, b }) => (r, g, b),
        AnsiColor::Named(NamedColor::Foreground) => dfg,
        AnsiColor::Named(NamedColor::Background) => dbg,
        AnsiColor::Named(NamedColor::Cursor) => dfg,
        AnsiColor::Named(named) => {
            let idx = named as usize;
            if idx < 256 {
                palette_lookup(palette, idx)
            } else {
                // Dim/Bright foreground variants without an explicit color.
                dfg
            }
        }
        AnsiColor::Indexed(i) => palette_lookup(palette, i as usize),
    }
}

/// A palette index, preferring a program-installed override, else the built-in
/// xterm-256 color for that index.
fn palette_lookup(palette: &Colors, idx: usize) -> (u8, u8, u8) {
    if idx < 256
        && let Some(Rgb { r, g, b }) = palette[idx]
    {
        return (r, g, b);
    }
    ansi256(idx)
}

/// The standard xterm 256-color palette: 16 system colors, a 6×6×6 cube, then a
/// 24-step grayscale ramp.
fn ansi256(idx: usize) -> (u8, u8, u8) {
    const SYSTEM: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00), // 0 black
        (0x80, 0x00, 0x00), // 1 red
        (0x00, 0x80, 0x00), // 2 green
        (0x80, 0x80, 0x00), // 3 yellow
        (0x00, 0x00, 0x80), // 4 blue
        (0x80, 0x00, 0x80), // 5 magenta
        (0x00, 0x80, 0x80), // 6 cyan
        (0xc0, 0xc0, 0xc0), // 7 white
        (0x80, 0x80, 0x80), // 8 bright black
        (0xff, 0x00, 0x00), // 9 bright red
        (0x00, 0xff, 0x00), // 10 bright green
        (0xff, 0xff, 0x00), // 11 bright yellow
        (0x00, 0x00, 0xff), // 12 bright blue
        (0xff, 0x00, 0xff), // 13 bright magenta
        (0x00, 0xff, 0xff), // 14 bright cyan
        (0xff, 0xff, 0xff), // 15 bright white
    ];
    match idx {
        0..=15 => SYSTEM[idx],
        16..=231 => {
            let i = idx - 16;
            let levels = [0u8, 95, 135, 175, 215, 255];
            (levels[(i / 36) % 6], levels[(i / 6) % 6], levels[i % 6])
        }
        232..=255 => {
            let v = 8 + (idx - 232) as u8 * 10;
            (v, v, v)
        }
        _ => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The xterm palette ramps are correct at a few known points.
    #[test]
    fn ansi256_palette_points() {
        assert_eq!(ansi256(0), (0, 0, 0));
        assert_eq!(ansi256(15), (0xff, 0xff, 0xff));
        // First cube entry (index 16) is black; last (231) is white.
        assert_eq!(ansi256(16), (0, 0, 0));
        assert_eq!(ansi256(231), (255, 255, 255));
        // Grayscale ramp endpoints.
        assert_eq!(ansi256(232), (8, 8, 8));
        assert_eq!(ansi256(255), (8 + 23 * 10, 8 + 23 * 10, 8 + 23 * 10));
    }

    /// End-to-end: spawn a real shell, run a command, and confirm its output
    /// reaches the grid snapshot — exercises PTY + parser + grid + snapshot.
    // Reliable locally but flaky in headless CI containers (no controlling TTY →
    // the PTY child's output never reaches the grid, so the snapshot comes back
    // empty). Run locally with `cargo test -- --ignored`.
    #[test]
    #[ignore = "real-PTY integration test; unreliable in headless CI"]
    fn shell_output_lands_in_grid() {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let term = Terminal::new(
            1,
            "*t*".to_string(),
            40,
            10,
            8,
            16,
            // A non-interactive shell that prints a marker then exits.
            Some((
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "printf ZXCV".to_string()],
            )),
            None,
            &Default::default(),
            tx,
        )
        .expect("spawn pty");

        // The event loop reads + parses on its own thread; poll the grid until
        // the marker appears (or give up after a generous timeout).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut found = String::new();
        while std::time::Instant::now() < deadline {
            let snap = term.snapshot(0xffffff, 0x000000);
            found = snap.cells.iter().map(|c| c.ch).collect();
            if found.contains("ZXCV") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(
            found.contains("ZXCV"),
            "expected shell output in grid, got {found:?}"
        );
    }

    /// A grid selection extracts the selected text — drives `selection_*`.
    // Same headless-CI limitation as `shell_output_lands_in_grid`: needs a real
    // PTY to render the marker before selecting. Run with `--ignored` locally.
    #[test]
    #[ignore = "real-PTY integration test; unreliable in headless CI"]
    fn selection_extracts_text() {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let term = Terminal::new(
            2,
            "*t*".to_string(),
            40,
            10,
            8,
            16,
            Some((
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "printf ABCD".to_string()],
            )),
            None,
            &Default::default(),
            tx,
        )
        .expect("spawn pty");

        // Wait for the marker to render on row 0.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let snap = term.snapshot(0xffffff, 0x000000);
            let row0: String = snap.cells.iter().map(|c| c.ch).collect();
            if row0.contains("ABCD") || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        // Select cells (0,0)..(0,3) and read the text back.
        term.selection_start(0, 0, Side::Left);
        term.selection_update(0, 3, Side::Right);
        let text = term.selection_text().unwrap_or_default();
        assert!(text.contains("ABCD"), "selection text was {text:?}");

        term.selection_clear();
        assert_eq!(term.selection_text(), None);
    }
}
