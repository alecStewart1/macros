//! App-wide quit coordination across every [`Workspace`] window.

use super::*;

/// App-wide quit coordination across every [`Workspace`] window. Each window
/// has its own editor (and buffers), so quitting must resolve the unsaved
/// buffers of *all* of them — one prompt at a time — before `cx.quit()` tears
/// the whole app down, and must run every window's `kill-emacs-hook`.
#[derive(Default)]
struct QuitFlow {
    /// Editors (by entity id) whose unsaved buffers the user already resolved
    /// (saved or discarded) during the quit in progress.
    confirmed: HashSet<gpui::EntityId>,
    /// The quit came from the update pill: relaunch into the staged build.
    relaunch: bool,
    /// A close-button prompt is pending in this editor: answering it closes
    /// just its window instead of continuing an app quit.
    close_window: Option<gpui::EntityId>,
}

impl gpui::Global for QuitFlow {}

/// Every open [`Workspace`] window.
fn workspace_windows(cx: &App) -> Vec<gpui::WindowHandle<Workspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|w| w.downcast::<Workspace>())
        .collect()
}

/// How many [`Workspace`] windows are open (the close button only closes a
/// single window while another remains).
pub fn workspace_window_count(cx: &App) -> usize {
    workspace_windows(cx).len()
}

/// Start a fresh, user-initiated app quit, forgetting any earlier cancelled
/// quit's per-window answers.
pub fn begin_app_quit(relaunch: bool, cx: &mut App) {
    cx.set_global(QuitFlow {
        relaunch,
        ..QuitFlow::default()
    });
}

/// A close-button prompt for one dirty window (with others still open) is
/// starting in the editor `editor`.
pub fn begin_window_close(editor: gpui::EntityId, cx: &mut App) {
    cx.set_global(QuitFlow {
        close_window: Some(editor),
        ..QuitFlow::default()
    });
}

/// The editor `editor` resolved its quit prompt (or had nothing unsaved).
/// Either close just its window (a close-button prompt, while another window
/// remains), or record the answer and carry the app quit on.
pub fn quit_confirmed(editor: gpui::EntityId, relaunch: bool, cx: &mut App) {
    let windows = workspace_windows(cx);
    if cx.default_global::<QuitFlow>().close_window == Some(editor) {
        cx.global_mut::<QuitFlow>().close_window = None;
        let target = windows.iter().find(|w| {
            w.read_with(cx, |ws, _| ws.editor.entity_id() == editor)
                .unwrap_or(false)
        });
        if windows.len() > 1
            && let Some(target) = target
        {
            target
                .update(cx, |_, window, _| window.remove_window())
                .ok();
            return;
        }
        // It became the last window meanwhile: closing it quits the app.
    }
    let flow = cx.default_global::<QuitFlow>();
    flow.confirmed.insert(editor);
    flow.relaunch |= relaunch;
    continue_app_quit(cx);
}

/// Carry an app quit forward: prompt the next window that still has unsaved
/// buffers the user hasn't answered for, or — once none remain — run every
/// window's `kill-emacs-hook` and quit.
pub fn continue_app_quit(cx: &mut App) {
    let windows = workspace_windows(cx);
    let confirmed = cx.default_global::<QuitFlow>().confirmed.clone();
    let relaunch = cx.global::<QuitFlow>().relaunch;
    let pending = windows.iter().find(|w| {
        w.read_with(cx, |ws, cx| {
            !confirmed.contains(&ws.editor.entity_id()) && ws.has_unsaved_buffers(cx)
        })
        .unwrap_or(false)
    });
    if let Some(pending) = pending {
        pending
            .update(cx, |ws, window, cx| {
                window.activate_window();
                ws.editor
                    .update(cx, |editor, cx| editor.prompt_quit_save(relaunch, cx));
            })
            .ok();
        return;
    }
    if relaunch && let Err(err) = crate::update::relaunch() {
        // Stay running rather than quitting into nothing.
        cx.set_global(QuitFlow::default());
        if let Some(w) = windows.first() {
            w.update(cx, |ws, _, cx| {
                ws.log_message(format!("Relaunch failed: {err}"), cx)
            })
            .ok();
        }
        return;
    }
    for w in &windows {
        w.update(cx, |ws, _, cx| {
            ws.editor.update(cx, |editor, cx| editor.prepare_quit(cx))
        })
        .ok();
    }
    cx.set_global(QuitFlow::default());
    cx.quit();
}
