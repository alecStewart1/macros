//! Buffer list, file I/O, save/kill, and FS watching for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{Context, ExternalPaths, SharedString, Window, px};
use notify_debouncer_mini::notify::RecursiveMode;

use crate::buffer::Buffer;

use super::*;

/// Returns `true` when `name` looks like a special buffer (`*scratch*`,
/// `*Messages*`, etc.) — i.e. it starts and ends with `*`.
fn is_special_buffer_name(name: &str) -> bool {
    name.starts_with('*') && name.ends_with('*') && name.len() > 1
}

/// The identity of the file at `path` for "is it already open?" checks: the
/// canonical path (symlinks, `.`/`..`, and relative spellings resolved). A file
/// that doesn't exist yet canonicalizes its directory and keeps its name; a
/// relative path resolves against the process cwd (the editor never `chdir`s).
pub(super) fn file_identity(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let absolute = || std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => std::fs::canonicalize(dir)
            .map(|dir| dir.join(name))
            .unwrap_or_else(|_| absolute()),
        _ => absolute(),
    }
}

impl Editor {
    /// Names of all open buffers (active first).
    pub(super) fn buffer_names(&self) -> Vec<String> {
        let mut names = vec![self.active.name.clone()];
        names.extend(self.inactive.iter().map(|d| d.name.clone()));
        names
    }

    /// Publish the buffer list to Scheme (for `(buffer-names)`), plus the
    /// name → absolute path map for file-visiting buffers (`buffer-file-name-of`).
    pub(super) fn sync_buffers(&self) {
        self.script.set_buffer_names(self.buffer_names());
        let abs = |p: &Path| -> String {
            if p.is_absolute() {
                p.to_string_lossy().into_owned()
            } else {
                // Resolve against the startup cwd (the editor never `chdir`s),
                // avoiding a `current_dir` syscall — see `process_cwd`'s doc.
                Path::new(&self.process_cwd)
                    .join(p)
                    .to_string_lossy()
                    .into_owned()
            }
        };
        let mut paths = HashMap::new();
        if let Some(p) = self.active.path.as_deref() {
            paths.insert(self.active.name.clone(), abs(p));
        }
        for d in &self.inactive {
            if let Some(p) = d.path.as_deref() {
                paths.insert(d.name.clone(), abs(p));
            }
        }
        self.script.set_buffer_paths(paths);
    }

    pub fn buffer_name(&self) -> &str {
        &self.active.name
    }

    /// Snapshot the active buffer's state into a [`Document`], leaving a blank
    /// placeholder behind (the caller installs another document next).
    pub(super) fn snapshot_active(&mut self) -> Document {
        // Flush any pending full-sync change while this buffer is still active —
        // after the swap below `self.active` is a different document.
        self.flush_lsp_fullsync();
        std::mem::replace(
            &mut self.active,
            blank_document(String::new(), None, "", ModeStack::default()),
        )
    }

    pub(super) fn install_active(&mut self, doc: Document) {
        self.active = doc;
        // Markers are positions in the previous buffer; drop them on a switch.
        self.script.clear_markers();
        // Secondary selections / snippet fields are marker-backed, so they're
        // gone with the markers — forget their (now dangling) head+anchor pairs
        // and any avy hints.
        self.extra_cursors.clear();
        self.snippet_fields.clear();
        self.avy_hints.clear();
        // Everything else that pins a RAW byte range in the buffer we just left
        // goes too. These are re-armed per command in the normal flow, but a
        // switch can land between the arm and the use — click into another
        // pane right after `C-y`, then press `M-y`, and `yank_pop` would splice
        // the old buffer's range into the new one (a panic outright when the
        // new buffer is shorter). Same for a completion popup left open when a
        // hook or process output surfaces a different buffer.
        self.last_yank = None;
        self.last_was_yank = false;
        self.completion_popup = None;
        self.completion_popup_hit = None;
        self.completion_range = None;
        self.completion_auto_pending = false;
        // The selected window now shows this buffer (Emacs: switching the active
        // buffer changes what the selected window displays).
        let active = self.active.name.clone();
        let selected = self.selected_pane;
        if let Some(pane) = self.panes.iter_mut().find(|p| p.id == selected) {
            pane.buffer = active;
        }
        // Invalidate the layout cache; it's recomputed on the next paint.
        self.lines.clear();
        self.last_bounds = None;
        // Any buffer switch/create/kill flows through here — keep Scheme's view
        // of the buffer list current.
        self.sync_buffers();
        // An in-flight completion was for the buffer we just left; its answer
        // must not pop up (with that buffer's byte range) in this one.
        for server in self.servers.values_mut() {
            if let Some((id, _)) = server.pending_completion.take() {
                server.client.cancel(id);
            }
        }
        // A buffer shown again after its server (re)started — or after it was
        // closed in it — needs a fresh `didOpen` (no-op when already open).
        if let Some(language) = self.active_language()
            && self.servers.contains_key(&language)
        {
            self.lsp_open_active(&language);
        }
    }

    /// Make the buffer `name` active, snapshotting the current one.
    ///
    /// `deliberate` marks a user-initiated switch (C-x b, find-file reuse, an
    /// LSP jump) as opposed to an incidental activation (pane-focus dance,
    /// process-output surfacing). Both fire `buffer-switch-hook`; only a
    /// deliberate switch also fires `buffer-select-hook` — see [`switch_hooks`].
    pub(super) fn activate(&mut self, index: usize, deliberate: bool, cx: &mut Context<Self>) {
        let target = self.inactive.remove(index);
        let current = self.snapshot_active();
        self.inactive.push(current);
        self.install_active(target);
        self.parse_full();
        self.ensure_cursor_visible();
        self.run_mode_hooks_once(cx);
        self.switch_hooks(deliberate);
        cx.notify();
    }

    /// Fire the buffer-change hooks after the active buffer was replaced.
    ///
    /// Re-activating an existing buffer never fires `find-file-hook`, so
    /// anything tracking "which buffer is the user in" needs its own signal.
    /// `buffer-switch-hook` fires on EVERY activation (bookkeeping: spell,
    /// perspective resume files); `buffer-select-hook` fires only on a
    /// DELIBERATE switch, so the perspectives title bar can follow the user
    /// into another project without incidental pane-focus activations
    /// hijacking it. Both are free when nothing is registered.
    pub(super) fn switch_hooks(&mut self, deliberate: bool) {
        self.run_hook("buffer-switch-hook");
        if deliberate {
            self.run_hook("buffer-select-hook");
        }
    }

    /// `C-x <right>`: cycle forward through the buffer ring (the active buffer
    /// goes to the back, the front buffer becomes active).
    pub(super) fn next_buffer(&mut self, cx: &mut Context<Self>) {
        if self.inactive.is_empty() {
            self.echo("No other buffer", cx);
            return;
        }
        let target = self.inactive.remove(0);
        self.cycle_to(target, true, cx);
    }

    /// `C-x <left>`: cycle backward (the active buffer goes to the front, the
    /// back buffer becomes active) — the exact inverse of `next-buffer`.
    pub(super) fn previous_buffer(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.inactive.pop() else {
            self.echo("No other buffer", cx);
            return;
        };
        self.cycle_to(target, false, cx);
    }

    pub(super) fn cycle_to(
        &mut self,
        target: Document,
        current_to_back: bool,
        cx: &mut Context<Self>,
    ) {
        let current = self.snapshot_active();
        if current_to_back {
            self.inactive.push(current);
        } else {
            self.inactive.insert(0, current);
        }
        self.install_active(target);
        self.parse_full();
        self.ensure_cursor_visible();
        self.run_mode_hooks_once(cx);
        self.switch_hooks(true); // C-x <arrow> cycling is a deliberate switch
        let name = self.active.name.clone();
        self.echo(name, cx);
    }

    /// `C-x C-f`: visit a file, reusing its buffer if already open.
    ///
    /// "Already open" compares canonical paths ([`file_identity`]), so
    /// `./src/main.rs`, an absolute spelling, and a symlink to it all reuse one
    /// buffer, and a visit while the same file's read is still in flight is
    /// dropped rather than racing to create a second buffer.
    pub(super) fn find_file(&mut self, input: &str, cx: &mut Context<Self>) {
        let path = PathBuf::from(expand_path(input.trim()));
        if path.as_os_str().is_empty() {
            return;
        }
        let identity = file_identity(&path);
        if self.switch_to_visiting(&identity, cx) {
            return;
        }
        if !self.pending_opens.insert(identity.clone()) {
            return; // the first visit's read will install (and show) it
        }

        let name = path.file_name().map_or_else(
            || input.trim().to_string(),
            |f| f.to_string_lossy().into_owned(),
        );

        // Read off the UI thread so opening a large file never freezes the
        // editor; install the buffer back on the foreground when it's ready.
        let read_path = path.clone();
        let read = cx
            .background_executor()
            .spawn(async move { std::fs::read_to_string(&read_path) });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |editor, cx| {
                editor.pending_opens.remove(&identity);
                match result {
                    Ok(contents) => editor.open_file(name, path, contents, cx),
                    // A missing file is the ordinary "visit a new file" flow: an
                    // empty buffer that creates the file on first save.
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        editor.open_file(name, path, String::new(), cx)
                    }
                    // Anything else (permissions, non-UTF-8 data, …) must NOT open
                    // an empty file-visiting buffer — saving that buffer would
                    // truncate the file on disk. Report and leave the editor as-is.
                    Err(err) => editor.echo(format!("Could not open {name}: {err}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// If some buffer already visits the file `identity` (a [`file_identity`]),
    /// show it and return `true`.
    fn switch_to_visiting(&mut self, identity: &Path, cx: &mut Context<Self>) -> bool {
        let visits = |doc: &Document| {
            doc.path
                .as_deref()
                .is_some_and(|p| file_identity(p) == identity)
        };
        if visits(&self.active) {
            self.echo(format!("Already in {}", self.active.name), cx);
            return true;
        }
        if let Some(index) = self.inactive.iter().position(visits) {
            self.activate(index, true, cx);
            self.echo(format!("Switched to {}", self.active.name), cx);
            return true;
        }
        false
    }

    /// Install a freshly-read file as the active buffer (foreground half of the
    /// async `find_file`, and of LSP's `visit_path`). Re-checks for an existing
    /// buffer on the file — another route may have opened it meanwhile — and
    /// gives the new buffer a unique name (see [`Self::unique_buffer_name`]).
    pub(super) fn open_file(
        &mut self,
        name: String,
        path: PathBuf,
        contents: String,
        cx: &mut Context<Self>,
    ) {
        if self.switch_to_visiting(&file_identity(&path), cx) {
            return;
        }
        let name = self.unique_buffer_name(&name, &path);
        let doc = make_document(name.clone(), Some(path), &contents);
        let current = self.snapshot_active();
        self.inactive.push(current);
        self.install_active(doc);
        self.parse_full();
        self.move_to(0, cx);
        self.run_mode_hooks_once(cx);
        self.echo(format!("Visiting {name}"), cx);
        self.run_hook("find-file-hook"); // Emacs find-file-hook
        self.reconcile_watches(cx); // watch the newly-visited file (auto-revert)
    }

    /// A buffer name for visiting `path` that no open buffer uses. `base` (the
    /// file name) when free; otherwise Emacs uniquify's `post-forward-angle-
    /// brackets` style — `mod.rs<editor>`, adding parent directories until the
    /// suffix tells this file apart from every other open file of that name
    /// (`mod.rs<a/src>`), with a numeric `mod.rs<2>` as the last resort.
    ///
    /// Only the NEW buffer is named this way; existing buffers keep their
    /// names, since Scheme state (perspectives, buffer-locals, pane lists) is
    /// keyed by buffer name and a rename would orphan it.
    pub(super) fn unique_buffer_name(&self, base: &str, path: &Path) -> String {
        let taken = |n: &str| self.active.name == n || self.inactive.iter().any(|d| d.name == n);
        if !taken(base) {
            return base.to_string();
        }
        let identity = file_identity(path);
        // Other open files with the same file name, as canonical paths.
        let others: Vec<PathBuf> = std::iter::once(&self.active)
            .chain(&self.inactive)
            .filter_map(|d| d.path.as_deref())
            .map(file_identity)
            .filter(|p| *p != identity && p.file_name() == identity.file_name())
            .collect();
        uniquified_name(base, &identity, &others, taken)
    }
}

/// The naming rule behind [`Editor::unique_buffer_name`], pure for testing:
/// `base<dir>` with the fewest trailing parent directories of `path` that no
/// path in `others` shares, skipping names `taken` reports in use; numeric
/// `base<N>` when no directory suffix works.
fn uniquified_name(
    base: &str,
    path: &Path,
    others: &[PathBuf],
    taken: impl Fn(&str) -> bool,
) -> String {
    if !taken(base) {
        return base.to_string();
    }
    let dirs = |p: &Path| -> Vec<String> {
        p.parent()
            .map(|d| {
                d.components()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let suffix = |parts: &[String], depth: usize| -> Option<String> {
        (depth <= parts.len()).then(|| parts[parts.len() - depth..].join("/"))
    };
    let mine = dirs(path);
    let theirs: Vec<Vec<String>> = others.iter().map(|p| dirs(p)).collect();
    for depth in 1..=mine.len() {
        let Some(s) = suffix(&mine, depth) else { break };
        let candidate = format!("{base}<{s}>");
        let distinct = theirs
            .iter()
            .all(|t| suffix(t, depth).as_deref() != Some(s.as_str()));
        if distinct && !taken(&candidate) {
            return candidate;
        }
    }
    (2..)
        .map(|n| format!("{base}<{n}>"))
        .find(|c| !taken(c))
        .expect("an unused numeric suffix exists")
}

impl Editor {
    /// Open files dropped onto the window from the OS (Finder, a terminal, etc.).
    /// Each dropped file is visited in its own buffer; the last one ends up
    /// active. Directories are skipped — `find_file` only reads regular files.
    pub(super) fn on_drop_files(
        &mut self,
        paths: &ExternalPaths,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut skipped = 0;
        for path in paths.paths() {
            if path.is_dir() {
                skipped += 1;
                continue;
            }
            self.find_file(&path.to_string_lossy(), cx);
        }
        if skipped > 0 {
            self.echo(
                format!("Skipped {skipped} folder(s) — can only open files"),
                cx,
            );
        }
    }

    /// `:w <file>` — write the buffer's text to a given path (Vim save-as). The
    /// buffer keeps its own file; this just drops a copy.
    pub(super) fn write_buffer_to(&mut self, path: &str, cx: &mut Context<Self>) {
        let target = expand_path(path.trim());
        match crate::save::write_now(
            Path::new(&target),
            self.active.buffer.full_text().as_bytes(),
        ) {
            Ok(()) => self.echo(format!("Wrote {target}"), cx),
            Err(err) => self.echo(format!("Save failed: {err}"), cx),
        }
    }

    /// Open `path` in a window *other* than the selected one — the sidebar
    /// pattern (dired-sidebar/dired): `RET` on a file keeps the tree and shows the
    /// file in the main editor. Uses the first other pane, or splits if the
    /// selected pane is the only one. Focus moves to the file's window.
    pub(super) fn open_file_other_window(&mut self, path: String, cx: &mut Context<Self>) {
        let mut leaves = Vec::new();
        layout_leaves(&self.layout, &mut leaves);
        let other = leaves.iter().copied().find(|&id| id != self.selected_pane);
        let target = match other {
            Some(id) => id,
            None => {
                // Narrow split: the sidebar (the current pane) keeps this share.
                const SIDEBAR_SPLIT_RATIO: f32 = 0.22;
                let new_id = self.next_pane_id; // split assigns this id to the new pane
                self.split_window_ratio(false, SIDEBAR_SPLIT_RATIO, cx);
                new_id
            }
        };
        self.select_pane(target, cx);
        self.find_file(&path, cx);
    }

    /// `:e!` / `M-x revert-buffer`: re-read the visited file from disk,
    /// discarding unsaved changes. Point is kept (clamped to the new length).
    pub(super) fn revert_buffer(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active.path.clone() else {
            self.echo("Buffer is not visiting a file", cx);
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let point = self.point();
                self.replace_whole_buffer(&contents, point, cx);
                self.active.modified = false;
                self.echo(format!("Reverted {}", self.active.name), cx);
            }
            Err(err) => self.echo(format!("Revert failed: {err}"), cx),
        }
    }

    /// Foreground handler for a debounced batch of changed paths. For each path:
    /// auto-revert the buffer visiting it (when `auto-revert` is on), and, if it
    /// lives under a watched directory, schedule that dir's `directory-changed-hook`
    /// (deduped so a 50-file change refreshes dired-sidebar/dired once, not 50 times).
    pub(super) fn on_fs_events(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let auto_revert = cx.global::<crate::settings::Settings>().auto_revert();
        let mut changed_dirs: Vec<PathBuf> = Vec::new();
        for path in &paths {
            if auto_revert {
                self.auto_revert_file(path, cx);
            }
            // A non-recursive dir watch reports its immediate children, so the
            // watched dir is the changed path's parent — or the path itself if a
            // watched dir's own entry (rename/attrs) is what fired.
            for dir in [path.parent(), Some(path.as_path())].into_iter().flatten() {
                if self.fs.dirs.contains(dir) && !changed_dirs.iter().any(|d| d == dir) {
                    changed_dirs.push(dir.to_path_buf());
                }
            }
            // Recursive roots: a change anywhere in the subtree fires the hook
            // with the ROOT, deduped so a bulk change refreshes magit once.
            for root in &self.fs.trees {
                if path.starts_with(root) && !changed_dirs.iter().any(|d| d == root) {
                    changed_dirs.push(root.clone());
                }
            }
        }
        for dir in changed_dirs {
            self.dispatch_dir_changed(&dir);
        }
    }

    /// React to an external change to `path`: if a file-visiting buffer holds it
    /// and has no unsaved edits, re-read it from disk. Buffers with unsaved changes
    /// are left intact (a one-line warning is echoed) so we never lose the user's
    /// work. Our own saves are absorbed for free: the on-disk text then equals the
    /// buffer's, so the content check below short-circuits with no revert.
    pub(super) fn auto_revert_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        // Only touch disk if some buffer actually visits this path — a watched
        // directory reports churn in files we don't have open, and reading those
        // would be pure waste.
        let visited = self.active.path.as_deref() == Some(path)
            || self
                .inactive
                .iter()
                .any(|d| d.path.as_deref() == Some(path));
        if !visited {
            return;
        }
        let Ok(disk) = std::fs::read_to_string(path) else {
            return; // unreadable / mid-rename / deleted — leave the buffer as-is
        };
        if self.active.path.as_deref() == Some(path) {
            if self.active.buffer.full_text() == disk {
                return; // identical (e.g. our own write) — nothing to revert
            }
            if self.active.modified {
                self.echo(
                    format!("{} changed on disk (unsaved edits kept)", self.active.name),
                    cx,
                );
                return;
            }
            let point = self.point();
            self.replace_whole_buffer(&disk, point, cx);
            self.active.modified = false;
            self.echo(
                format!("Reverted {} (changed on disk)", self.active.name),
                cx,
            );
            return;
        }
        if let Some(idx) = self
            .inactive
            .iter()
            .position(|d| d.path.as_deref() == Some(path))
        {
            let doc = &self.inactive[idx];
            if doc.buffer.full_text() == disk {
                return;
            }
            let name = doc.name.clone();
            if doc.modified {
                self.echo(format!("{name} changed on disk (unsaved edits kept)"), cx);
                return;
            }
            // An inactive buffer has no live cursor/view to preserve, so rebuild it
            // wholesale — this also refreshes its mode/highlighter for the new text.
            let doc_path = doc.path.clone();
            self.inactive[idx] = make_document(name.clone(), doc_path, &disk);
            // Its servers still hold the old text under this URI.
            let uri = document_uri(&self.inactive[idx]);
            self.lsp_sync_document(&uri, &disk);
            self.echo(format!("Reverted {name} (changed on disk)"), cx);
        }
    }

    /// Fire every `directory-changed-hook` function with `dir` as its argument, so
    /// a Scheme view (dired-sidebar/dired) can re-render if `dir` is the one it shows.
    /// No-op — and no thread hop — when nothing is registered on the hook.
    pub(super) fn dispatch_dir_changed(&mut self, dir: &Path) {
        self.run_hook_arg("directory-changed-hook", &dir.to_string_lossy());
    }

    /// Call every function registered on `hook` with the single string `arg`
    /// (`directory-changed-hook`'s dir, `terminal-exit-hook`'s buffer name).
    /// No-op — and no thread hop — when nothing is registered on the hook.
    pub(super) fn run_hook_arg(&mut self, hook: &str, arg: &str) {
        let Some(functions) = self.hooks.get(hook).filter(|f| !f.is_empty()).cloned() else {
            return;
        };
        self.publish_point();
        for function in functions {
            self.script.dispatch_action(&function, Some(arg));
        }
    }

    /// Sync the OS watcher with what should be watched right now: every Scheme-
    /// requested directory, plus — when `auto-revert` is on — every file-visiting
    /// buffer. Diffs against `fs_watched` so only the delta is (un)watched. Cheap
    /// and idempotent; call it whenever the buffer set or the option changes.
    pub(super) fn reconcile_watches(&mut self, cx: &mut Context<Self>) {
        if self.fs.debouncer.is_none() {
            return;
        }
        let mut desired: HashSet<PathBuf> = self.fs.dirs.clone();
        if cx.global::<crate::settings::Settings>().auto_revert() {
            if let Some(path) = &self.active.path {
                desired.insert(path.clone());
            }
            for doc in &self.inactive {
                if let Some(path) = &doc.path {
                    desired.insert(path.clone());
                }
            }
        }
        let debouncer = self.fs.debouncer.as_mut().unwrap();
        for path in &desired {
            if !self.fs.watched.contains(path)
                && debouncer
                    .watcher()
                    .watch(path, RecursiveMode::NonRecursive)
                    .is_ok()
            {
                self.fs.watched.insert(path.clone());
            }
        }
        let stale: Vec<PathBuf> = self.fs.watched.difference(&desired).cloned().collect();
        for path in stale {
            let _ = debouncer.watcher().unwatch(&path);
            self.fs.watched.remove(&path);
        }

        // Recursive roots (magit repo watch) — same delta dance, recursive mode.
        let want_rec = self.fs.trees.clone();
        for path in &want_rec {
            if !self.fs.watched_recursive.contains(path)
                && debouncer
                    .watcher()
                    .watch(path, RecursiveMode::Recursive)
                    .is_ok()
            {
                self.fs.watched_recursive.insert(path.clone());
            }
        }
        let stale_rec: Vec<PathBuf> = self
            .fs
            .watched_recursive
            .difference(&want_rec)
            .cloned()
            .collect();
        for path in stale_rec {
            let _ = debouncer.watcher().unwatch(&path);
            self.fs.watched_recursive.remove(&path);
        }
    }

    /// `C-x b`: switch to an already-open buffer by name.
    pub(super) fn switch_to_buffer(&mut self, name: &str, cx: &mut Context<Self>) {
        self.switch_to_buffer_ex(name, true, cx);
    }

    /// [`switch_to_buffer`](Self::switch_to_buffer) with explicit intent:
    /// `deliberate` is false when a pane-focus change (`select_pane`) surfaces a
    /// buffer, so the perspectives title bar doesn't follow that incidental
    /// activation into another project.
    pub(super) fn switch_to_buffer_ex(
        &mut self,
        name: &str,
        deliberate: bool,
        cx: &mut Context<Self>,
    ) {
        let name = name.trim();
        if name == self.active.name {
            cx.notify();
            return;
        }
        match self.inactive.iter().position(|d| d.name == name) {
            Some(index) => self.activate(index, deliberate, cx),
            None => self.echo(format!("No buffer named {name}"), cx),
        }
    }

    /// `C-x C-s`: write the active buffer back to its file.
    pub(super) fn save_buffer(&mut self, cx: &mut Context<Self>) {
        if self.active.path.is_none() {
            self.echo("Buffer is not visiting a file", cx);
            return;
        }
        if self
            .active_uri()
            .is_some_and(|uri| self.formatting_for_save.contains_key(&uri))
        {
            // The format's response (or its timeout) writes this buffer.
            self.echo("Save pending (waiting on formatting)", cx);
            return;
        }
        // Run synchronous before-save hooks (e.g. a shell formatter), then — if
        // this language formats via LSP on save — kick off the (async) format
        // and defer the write to its response.
        self.run_hook_blocking("before-save-hook", cx);
        if self.start_lsp_format_on_save(cx) {
            return;
        }
        self.write_active_to_disk(cx);
    }

    /// Save every modified file-visiting buffer (Emacs `save-some-buffers`).
    /// Inactive buffers are written synchronously here; the active one goes
    /// through the normal (async, hook-running) save path.
    pub(super) fn save_some_buffers(&mut self, cx: &mut Context<Self>) {
        let mut saved = 0;
        // Failed inactive writes are reported (and keep `modified` set) rather
        // than being silently dropped from the count.
        let mut failed: Vec<String> = Vec::new();
        let mut saved_uris = Vec::new();
        let mut written: Vec<PathBuf> = Vec::new();
        for doc in &mut self.inactive {
            if doc.modified
                && let Some(path) = doc.path.clone()
            {
                match crate::save::write_now(&path, doc.buffer.full_text().as_bytes()) {
                    Ok(()) => {
                        doc.modified = false;
                        saved += 1;
                        saved_uris.push(document_uri(doc));
                        written.push(path);
                    }
                    Err(err) => failed.push(format!("{}: {err}", doc.name)),
                }
            }
        }
        for uri in saved_uris {
            self.lsp_did_save(&uri);
        }
        for path in written {
            self.rewatch_file(&path, cx);
        }
        if self.active.path.is_some() && self.active.modified {
            // Async; reports its own failure (and restores `modified`) later.
            self.write_active_to_disk(cx);
            saved += 1;
        }
        let mut message = match saved {
            0 => "No unsaved buffers".to_string(),
            1 => "Saved 1 buffer".to_string(),
            n => format!("Saved {n} buffers"),
        };
        if !failed.is_empty() {
            message.push_str(&format!("; {} failed: {}", failed.len(), failed.join("; ")));
        }
        self.echo(message, cx);
    }

    /// Write the active buffer to its file off the UI thread. Optimistically
    /// clears `modified`; restores it (and reports) if the write fails.
    ///
    /// The write is atomic and ordered per file (see [`crate::save`]): the
    /// ticket taken here, with the text snapshot, guarantees an older save
    /// still in flight can't land after this one.
    pub(super) fn write_active_to_disk(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active.path.clone() else {
            return;
        };
        let text = self.active.buffer.full_text();
        let name = self.active.name.clone();
        let uri = document_uri(&self.active);
        self.active.modified = false;
        let ticket = crate::save::ticket(&path);
        let write_path = path.clone();
        let write = cx.background_executor().spawn(async move {
            let outcome = crate::save::write_sequenced(&ticket, &write_path, text.as_bytes());
            (ticket, outcome)
        });
        cx.spawn(async move |this, cx| {
            let (ticket, result) = write.await;
            this.update(cx, |editor, cx| match result {
                Ok(crate::save::SaveOutcome::Written) => {
                    editor.rewatch_file(&path, cx);
                    editor.echo(format!("Wrote {name}"), cx);
                    editor.lsp_did_save(&uri);
                    editor.run_hook("after-save-hook"); // Emacs after-save-hook
                }
                // A newer save of this file already landed and reported.
                Ok(crate::save::SaveOutcome::Superseded) => {}
                Err(err) => {
                    // Re-dirty the buffer that was SAVED (by its file), not
                    // whatever happens to be active now — and only if no newer
                    // save owns its state.
                    if crate::save::is_latest(&ticket) {
                        editor.mark_path_modified(&path);
                    }
                    editor.echo(format!("Save failed: {name}: {err}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-arm the auto-revert watch on `path` after we saved it. An atomic save
    /// renames a new inode over the file; inode-based backends (inotify) drop
    /// the old watch with it, which would silently end auto-revert for this
    /// file. (Path-based FSEvents doesn't need it, but re-arming is harmless.)
    pub(super) fn rewatch_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.fs.watched.remove(path) {
            return;
        }
        if let Some(debouncer) = self.fs.debouncer.as_mut() {
            let _ = debouncer.watcher().unwatch(path);
        }
        self.reconcile_watches(cx);
    }

    /// Mark whichever buffer visits `path` (active or not) as modified.
    fn mark_path_modified(&mut self, path: &Path) {
        if let Some(doc) = std::iter::once(&mut self.active)
            .chain(self.inactive.iter_mut())
            .find(|d| d.path.as_deref() == Some(path))
        {
            doc.modified = true;
        }
    }

    /// If the active buffer's language formats via LSP on save and a server is
    /// ready, trigger the format (the response's edits re-enter `save_buffer` to
    /// write). Returns whether a format was started.
    pub(super) fn start_lsp_format_on_save(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(language) = self.active_language() else {
            return false;
        };
        if !self.lsp_format_on_save.contains(&language) {
            return false;
        }
        let Some(uri) = self.active_uri() else {
            return false;
        };
        let ready = self
            .servers
            .get(&language)
            .is_some_and(|s| s.ready && s.open.contains(&uri));
        if !ready {
            return false;
        }
        self.format_save_seq += 1;
        let token = self.format_save_seq;
        self.formatting_for_save.insert(uri.clone(), token);
        if !self.lsp_format_request(Some(token), cx) {
            self.formatting_for_save.remove(&uri);
            return false;
        }
        // A server that never answers must not wedge the save: after the
        // timeout, write this document unformatted (unless the response or a
        // server exit already completed this save — the token tells).
        let timer = cx.background_executor().timer(FORMAT_ON_SAVE_TIMEOUT);
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |editor, cx| {
                if editor.finish_format_on_save(&uri, token, cx) {
                    editor.echo("Formatting timed out; saved unformatted", cx);
                }
            })
            .ok();
        })
        .detach();
        true
    }

    /// Run a hook's functions synchronously (blocking on the engine thread),
    /// applying each one's enqueued commands immediately. Used for
    /// `before-save-hook`, where a formatter must modify the buffer *before* the
    /// write. Async work a hook starts (e.g. an LSP request) still completes
    /// later — only the synchronous part is guaranteed done here.
    pub(super) fn run_hook_blocking(&mut self, hook: &str, cx: &mut Context<Self>) {
        let Some(functions) = self.hooks.get(hook).filter(|f| !f.is_empty()).cloned() else {
            return;
        };
        for function in functions {
            self.publish_point();
            for command in self.script.call_action(&function, None) {
                self.apply_command(command, cx);
            }
        }
    }

    pub fn any_modified(&self) -> bool {
        let active_dirty = self.active.modified && !is_special_buffer_name(&self.active.name);
        let inactive_dirty = self
            .inactive
            .iter()
            .any(|d| d.modified && !is_special_buffer_name(&d.name));
        active_dirty || inactive_dirty
    }

    pub fn is_modified(&self) -> bool {
        self.active.modified
    }

    /// Handle an OS window-close request (the title-bar close button) and
    /// `C-x C-c`. If any buffer has unsaved changes, offer to save them first:
    /// `yes` saves all modified file buffers then quits, `no` quits discarding,
    /// `C-g` cancels. With nothing unsaved, quit straight away. Always returns
    /// `false` so the bare window-close is cancelled — quitting (`cx.quit()`)
    /// tears the whole app down rather than leaving a windowless process on
    /// macOS.
    pub fn confirm_quit(&mut self, cx: &mut Context<Self>) -> bool {
        self.confirm_quit_ex(false, cx)
    }

    /// The update pill's "relaunch into the staged build": the same unsaved-
    /// buffer prompt as a normal quit, relaunching only if the quit goes ahead
    /// (a cancelled prompt leaves no relaunch helper behind).
    pub fn confirm_quit_and_relaunch(&mut self, cx: &mut Context<Self>) {
        self.confirm_quit_ex(true, cx);
    }

    fn confirm_quit_ex(&mut self, relaunch: bool, cx: &mut Context<Self>) -> bool {
        if self.any_modified() {
            let listing = self.modified_buffer_names().join(", ");
            self.echo(format!("Modified buffers: {listing}"), cx);
            self.open_minibuffer(
                "Save modified buffers before quitting? (yes/no, C-g to cancel) ",
                MinibufferPurpose::QuitSave { relaunch },
                "",
                cx,
            );
        } else {
            self.finish_quit(relaunch, cx);
        }
        false
    }

    /// Names of the buffers [`any_modified`](Self::any_modified) counts as
    /// unsaved, active first.
    pub(super) fn modified_buffer_names(&self) -> Vec<String> {
        std::iter::once(&self.active)
            .chain(&self.inactive)
            .filter(|d| d.modified && !is_special_buffer_name(&d.name))
            .map(|d| d.name.clone())
            .collect()
    }

    /// Actually quit (the user has confirmed, or nothing was unsaved). With
    /// `relaunch`, first spawn the helper that reopens the staged update once
    /// this process exits; if that fails, stay running and say so rather than
    /// quitting into nothing.
    pub(super) fn finish_quit(&mut self, relaunch: bool, cx: &mut Context<Self>) {
        if relaunch && let Err(err) = crate::update::relaunch() {
            self.echo(format!("Relaunch failed: {err}"), cx);
            return;
        }
        self.quit_now(cx);
    }

    /// Write every modified, file-visiting buffer (active + inactive) to disk
    /// synchronously. Used on quit, so writes are blocking (no UI to keep
    /// responsive) and skip the async format-on-save path. Buffers not visiting
    /// a file (e.g. `*scratch*`) can't be saved and are left untouched.
    ///
    /// Returns whether every write succeeded; failed buffers keep `modified` set
    /// and are reported in the echo area, so the quit path can abort rather than
    /// silently discard the changes.
    pub(super) fn save_all_modified(&mut self, cx: &mut Context<Self>) -> bool {
        let mut failed: Vec<String> = Vec::new();
        if self.active.modified
            && let Some(path) = self.active.path.clone()
        {
            match crate::save::write_now(&path, self.active.buffer.full_text().as_bytes()) {
                Ok(()) => self.active.modified = false,
                Err(err) => failed.push(format!("{}: {err}", self.active.name)),
            }
        }
        for doc in &mut self.inactive {
            if doc.modified
                && let Some(path) = doc.path.clone()
            {
                match crate::save::write_now(&path, doc.buffer.full_text().as_bytes()) {
                    Ok(()) => doc.modified = false,
                    Err(err) => failed.push(format!("{}: {err}", doc.name)),
                }
            }
        }
        if failed.is_empty() {
            true
        } else {
            self.echo(format!("Save failed — {}", failed.join("; ")), cx);
            false
        }
    }

    /// Write the active buffer to its file synchronously — the quit path
    /// (`:wq` / `:x`), where the write must complete before `cx.quit()` tears
    /// the app down (the async `write_active_to_disk` would be cancelled
    /// mid-flight). Runs the synchronous `before-save-hook` but, like
    /// [`save_all_modified`](Self::save_all_modified), skips the async LSP
    /// format-on-save. Returns whether the buffer is now safely on disk.
    pub(super) fn save_active_sync(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.active.path.clone() else {
            self.echo("Buffer is not visiting a file", cx);
            return false;
        };
        self.run_hook_blocking("before-save-hook", cx);
        match crate::save::write_now(&path, self.active.buffer.full_text().as_bytes()) {
            Ok(()) => {
                self.active.modified = false;
                self.echo(format!("Wrote {}", self.active.name), cx);
                let uri = document_uri(&self.active);
                self.lsp_did_save(&uri);
                self.run_hook("after-save-hook");
                true
            }
            Err(err) => {
                self.echo(format!("Save failed: {err}"), cx);
                false
            }
        }
    }

    /// Does killing the active buffer need a confirm? Emacs semantics: only a
    /// FILE-VISITING buffer guards unsaved changes — a scratch-like buffer
    /// (*scratch*, a package's query pane) has nowhere to save to, so killing
    /// it just discards, no prompt.
    fn kill_needs_confirm(&self) -> bool {
        self.active.modified && self.active.path.is_some()
    }

    /// `C-x k`: kill the active buffer (confirming first if it's a modified
    /// file buffer), then show the most-recent other buffer (or a fresh scratch).
    pub(super) fn kill_buffer(&mut self, cx: &mut Context<Self>) {
        if self.kill_needs_confirm() {
            self.open_confirm(
                "Buffer modified; kill anyway?",
                PendingAction::KillActiveBuffer,
                cx,
            );
        } else {
            self.kill_active_buffer(cx);
        }
    }

    /// Kill the active buffer *and* close its window (Emacs `kill-buffer-and-
    /// window`), confirming first if it's a modified file buffer. With a sole
    /// window, the buffer is killed and `delete_window` just declines.
    pub(super) fn kill_buffer_and_window(&mut self, cx: &mut Context<Self>) {
        if self.kill_needs_confirm() {
            self.open_confirm(
                "Buffer modified; kill anyway?",
                PendingAction::KillBufferAndWindow,
                cx,
            );
        } else {
            self.kill_active_buffer(cx);
            self.delete_window(cx);
        }
    }

    pub(super) fn kill_active_buffer(&mut self, cx: &mut Context<Self>) {
        let killed = self.active.name.clone();
        // `didClose` it — which also discards any pending full-sync for it, so
        // the replacement's text is never flushed under the killed URI (this
        // path swaps buffers without `snapshot_active`'s flush).
        let killed_uri = document_uri(&self.active);
        self.lsp_close_document(&killed_uri);
        let replacement = self
            .inactive
            .pop()
            .unwrap_or_else(|| make_document("*scratch*".to_string(), None, ""));
        self.install_active(replacement);
        self.drop_terminal_for_buffer(&killed);
        self.parse_full();
        self.ensure_cursor_visible();
        self.echo(format!("Killed {killed}"), cx);
        self.reconcile_watches(cx); // stop watching the killed buffer's file
    }

    /// Shut down and forget any embedded terminal backing buffer `name` (its
    /// `Drop` closes the PTY and ends the event-loop thread), and terminate any
    /// `make-process` child bound to it via `set-process-buffer`.
    pub(super) fn drop_terminal_for_buffer(&mut self, name: &str) {
        self.kill_processes_for_buffer(name);
        if let Some(id) = self
            .terminals
            .values()
            .find(|t| t.buffer_name == name)
            .map(|t| t.id)
        {
            self.terminals.remove(&id);
            self.terminal_geom.remove(&id);
            if self.terminal_drag == Some(id) {
                self.terminal_drag = None;
            }
        }
    }

    /// Kill a buffer by name: the active one (via the normal path), or just drop
    /// an inactive [`Document`] (ibuffer/dired). Last buffer stays as scratch.
    pub(super) fn kill_buffer_named(&mut self, name: &str, cx: &mut Context<Self>) {
        if name == self.active.name {
            self.kill_active_buffer(cx);
        } else if let Some(idx) = self.inactive.iter().position(|d| d.name == name) {
            let doc = self.inactive.remove(idx);
            self.lsp_close_document(&document_uri(&doc));
            self.drop_terminal_for_buffer(name);
            self.sync_buffers();
            self.echo(format!("Killed {name}"), cx);
            self.reconcile_watches(cx); // stop watching the killed buffer's file
        }
    }

    /// `C-x C-c` / `cmd-q`: quit, offering to save first if any buffer is
    /// unsaved (same save/discard/cancel prompt as the title-bar close button).
    pub(super) fn quit(&mut self, cx: &mut Context<Self>) {
        self.confirm_quit(cx);
    }

    // --- Special (read-only, program-generated) buffers & transients ------

    /// Render `text` into the read-only buffer `name` (major mode `mode`). If the
    /// buffer is already on screen — the active buffer, or shown in another pane
    /// (e.g. a *compilation* window) — it refreshes in place wherever it lives,
    /// preserving the point line and NOT stealing focus (magit's revert-and-keep-
    /// position). Only when it isn't shown anywhere does it switch the selected
    /// window to it.
    pub(super) fn render_buffer(
        &mut self,
        name: String,
        mode: String,
        text: String,
        line_faces: Vec<(usize, String)>,
        cx: &mut Context<Self>,
    ) {
        // Shown somewhere (active pane or another window): refresh it in place via
        // the quiet path, which never changes focus.
        if self.panes.iter().any(|p| p.buffer == name) {
            self.render_buffer_quiet(name, mode, text, line_faces, cx);
            return;
        }
        let faces: HashMap<usize, SharedString> = line_faces
            .into_iter()
            .map(|(line, face)| (line, SharedString::from(face)))
            .collect();
        // Not shown anywhere: switch the selected window to a fresh buffer.
        let current = self.snapshot_active();
        // Replace any stale buffer of the same name, then install fresh.
        self.inactive.retain(|d| d.name != name);
        self.inactive.push(current);
        self.install_active(make_special_document(name, &mode, &text));
        self.active.line_faces = faces;
        self.move_to(0, cx);
        self.active.modified = false;
        cx.notify();
    }

    /// Like [`render_buffer`](Self::render_buffer), but never switches buffers or
    /// moves focus: it refreshes the named buffer *wherever it already lives* —
    /// in place if it's active, or in its inactive [`Document`] (the dired-sidebar/dired
    /// sidebar pane) otherwise — and no-ops if the buffer isn't shown at all. This
    /// is the auto-refresh path (a watched directory changed on disk), so a passive
    /// FS event never yanks the user out of the buffer they're editing. Icon/overlay
    /// commands later in the same batch are redirected to the sidebar via
    /// [`render_redirect`](Self::render_redirect).
    pub(super) fn render_buffer_quiet(
        &mut self,
        name: String,
        mode: String,
        text: String,
        line_faces: Vec<(usize, String)>,
        cx: &mut Context<Self>,
    ) {
        let faces: HashMap<usize, SharedString> = line_faces
            .into_iter()
            .map(|(line, face)| (line, SharedString::from(face)))
            .collect();
        if self.active.name == name {
            // Active: identical to the in-place branch of `render_buffer`.
            self.active.line_icons.clear();
            let line = self.active.buffer.line_index_at_byte(self.point());
            // A refresh keeps the view where the user left it: restoring point's
            // line must not scroll back to point (they may have wheel-scrolled
            // away from it, which leaves point off-screen on purpose).
            let scroll = self.active.scroll_offset;
            self.active.buffer = Buffer::new(&text);
            // The old undo records address the replaced text; replaying one
            // against the new text corrupts it (or panics). Start fresh.
            self.active.reset_undo();
            self.undo_group_started = false;
            self.active.line_faces = faces;
            self.lines.clear();
            let last = self.active.buffer.line_count().saturating_sub(1);
            let offset = self.active.buffer.line_range(line.min(last)).start;
            self.move_to(offset, cx);
            self.cursor_follow_pending = false;
            let max = self.max_scroll_offset(f32::from(self.viewport_height));
            self.active.scroll_offset = px(f32::from(scroll).clamp(0.0, max.max(0.0)));
            self.active.modified = false;
            self.render_redirect = None;
        } else if let Some(idx) = self.inactive.iter().position(|d| d.name == name) {
            // Shown in a non-selected pane: rebuild that inactive buffer in place,
            // keeping the viewing pane's cursor on the same line (clamped).
            let viewing_point = self
                .panes
                .iter()
                .find(|p| p.buffer == name)
                .map(|p| p.point);
            let doc = &mut self.inactive[idx];
            let old_line = viewing_point
                .map(|pt| doc.buffer.line_index_at_byte(pt.min(doc.buffer.len())))
                .unwrap_or(0);
            doc.buffer = Buffer::new(&text);
            doc.reset_undo(); // see the active branch above
            doc.line_faces = faces;
            doc.line_icons.clear();
            doc.overlays = Overlays::default();
            doc.modified = false;
            doc.special = Some(mode);
            let last = doc.buffer.line_count().saturating_sub(1);
            let offset = doc.buffer.line_range(old_line.min(last)).start;
            let new_len = doc.buffer.len();
            for pane in self.panes.iter_mut().filter(|p| p.buffer == name) {
                pane.point = offset.min(new_len);
            }
            self.render_redirect = Some(idx);
            cx.notify();
        }
        // Not shown anywhere → nothing to refresh passively.
    }

    /// Append `text` to the end of buffer `name` (creating it if needed) and
    /// switch to it — an output-buffer primitive (Emacs `append-to-buffer`).
    /// Unlike `render-buffer`, the target is a normal editable buffer.
    pub(super) fn append_to_buffer(&mut self, name: String, text: String, cx: &mut Context<Self>) {
        if self.active.name != name {
            // If the buffer is displayed in another (non-selected) window, append
            // there in place — output flows into the buffer's own window without
            // yanking focus away from what the user is editing (e.g. a streaming
            // *compilation* pane). Mirrors `render_buffer`'s follow-the-buffer rule.
            if self
                .panes
                .iter()
                .any(|p| p.buffer == name && p.id != self.selected_pane)
                && self.inactive.iter().any(|d| d.name == name)
            {
                let (new_end, line_count) = {
                    let doc = self.inactive.iter_mut().find(|d| d.name == name).unwrap();
                    let end = doc.buffer.len();
                    doc.buffer.replace(end..end, &text);
                    (doc.buffer.len(), doc.buffer.line_count())
                };
                let line_h = f32::from(self.line_height).max(1.0);
                let content_h = line_count as f32 * line_h;
                for pane in self.panes.iter_mut().filter(|p| p.buffer == name) {
                    // Point at the tail so focusing the window lands on the latest
                    // output; scroll the (unfocused) pane to keep that tail in view
                    // as it streams, so the window doesn't look frozen.
                    pane.point = new_end;
                    if let Some((_, b)) = self.pane_bounds.iter().find(|(pid, _)| *pid == pane.id) {
                        pane.scroll = px((content_h - f32::from(b.size.height)).max(0.0));
                    }
                }
                cx.notify();
                return;
            }
            if let Some(idx) = self.inactive.iter().position(|d| d.name == name) {
                self.activate(idx, false, cx); // process output surfacing, not a user switch
            } else {
                let current = self.snapshot_active();
                self.inactive.push(current);
                self.install_active(make_document(name, None, ""));
            }
        }
        let end = self.active.buffer.len();
        self.active.buffer.replace(end..end, &text);
        self.lines.clear();
        let new_end = self.active.buffer.len();
        self.move_to(new_end, cx);
        self.parse_full();
        cx.notify();
    }

    /// Re-fontify Org buffer `name` in place, whether or not it is active. The
    /// active path is the ordinary current-buffer fontify; for a buffer living in
    /// an inactive [`Document`] (e.g. agent-shell streaming a reply into a
    /// non-selected pane) we publish a snapshot *of that buffer* so `org--fontify`'s
    /// reads (`buffer-string`, `current-buffer`, `buffer-local-get`) see it, and
    /// point [`render_redirect`](Self::render_redirect) at it so the overlay/indent
    /// writes it emits land there too — then restore the active snapshot. This is
    /// the `run_hook_blocking` pattern (publish → `call_action` → apply), so it
    /// reuses the exact Org fontifier without disturbing focus or the active buffer.
    pub(super) fn fontify_org_buffer(&mut self, name: String, cx: &mut Context<Self>) {
        if self.active.name == name {
            self.publish_point();
            for command in self.script.call_action("org--fontify", None) {
                self.apply_command(command, cx);
            }
            return;
        }
        let Some(idx) = self.inactive.iter().position(|d| d.name == name) else {
            // Not shown anywhere (or unknown) → nothing on screen to refresh.
            return;
        };
        self.publish_inactive_snapshot(idx);
        self.render_redirect = Some(idx);
        // org--fontify emits only overlay/indent writes (no buffer create/kill),
        // so `idx` stays valid across the apply loop.
        for command in self.script.call_action("org--fontify", None) {
            self.apply_command(command, cx);
        }
        self.render_redirect = None;
        // Restore the active-buffer snapshot for the next read bridge.
        self.publish_point();
    }

    /// The [`Document`] named `name` — the active buffer or an inactive
    /// snapshot. The one lookup every "operate on buffer N by name" method
    /// resolves through, so the active-vs-inactive dispatch lives here.
    pub(super) fn doc(&self, name: &str) -> Option<&Document> {
        if self.active.name == name {
            Some(&self.active)
        } else {
            self.inactive.iter().find(|d| d.name == name)
        }
    }

    /// Mutable twin of [`Self::doc`]. NOTE: text edits made through the
    /// returned document bypass the active buffer's edit pipeline (undo, LSP
    /// did-change, marker adjustment, re-highlight) — callers that modify the
    /// ACTIVE buffer's *text* must use the `edit`-based paths instead; this is
    /// for decorations/overlays and for inactive-buffer surgery.
    pub(super) fn doc_mut(&mut self, name: &str) -> Option<&mut Document> {
        if self.active.name == name {
            Some(&mut self.active)
        } else {
            self.inactive.iter_mut().find(|d| d.name == name)
        }
    }

    /// Borrow the rope buffer named `name` — the active buffer or any inactive
    /// pane buffer. Used by the cross-buffer ediff primitives.
    pub(super) fn buffer_named(&self, name: &str) -> Option<&Buffer> {
        self.doc(name).map(|d| &d.buffer)
    }

    /// Diff buffers `a` and `b` and hand each non-equal hunk to Steel `callback`.
    pub(super) fn diff_buffers(&self, a: &str, b: &str, callback: &str) {
        let records = match (self.buffer_named(a), self.buffer_named(b)) {
            (Some(abuf), Some(bbuf)) => {
                let (ta, tb) = (abuf.full_text(), bbuf.full_text());
                let diff = similar::TextDiff::from_lines(&ta, &tb);
                diff.ops()
                    .iter()
                    .filter_map(|op| {
                        let (tag, ar, br) = op.as_tag_tuple();
                        let name = match tag {
                            similar::DiffTag::Equal => return None,
                            similar::DiffTag::Delete => "delete",
                            similar::DiffTag::Insert => "insert",
                            similar::DiffTag::Replace => "replace",
                        };
                        let (abs, abe) = line_span(abuf, ar.start, ar.end);
                        let (bbs, bbe) = line_span(bbuf, br.start, br.end);
                        // Hunk text, newlines escaped to U+001F so each hunk
                        // stays a single tab-delimited record for Scheme.
                        let atext = abuf.slice_to_string(abs..abe).replace('\n', "\u{1f}");
                        let btext = bbuf.slice_to_string(bbs..bbe).replace('\n', "\u{1f}");
                        Some(format!(
                            "{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                            ar.start, ar.end, br.start, br.end, abs, abe, bbs, bbe, atext, btext
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => String::new(),
        };
        self.script.dispatch_action(callback, Some(&records));
    }

    /// Replace lines [ds,de) of buffer `dst` with lines [ss,se) of buffer `src`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn buffer_copy_lines(
        &mut self,
        src: &str,
        ss: usize,
        se: usize,
        dst: &str,
        ds: usize,
        de: usize,
        cx: &mut Context<Self>,
    ) {
        let text = match self.buffer_named(src) {
            Some(buf) => {
                let (s, e) = line_span(buf, ss, se);
                buf.slice_to_string(s..e)
            }
            None => return,
        };
        if self.active.name == dst {
            let (s, e) = line_span(&self.active.buffer, ds, de);
            self.replace_region(s, e, &text, cx);
        } else if let Some(idx) = self.inactive.iter().position(|d| d.name == dst) {
            let len = {
                let doc = &mut self.inactive[idx];
                let (s, e) = line_span(&doc.buffer, ds, de);
                doc.buffer.replace(s..e, &text);
                doc.reset_undo(); // unrecorded edit: old records are stale
                doc.modified = true;
                if let Some(h) = doc.highlighter.as_mut() {
                    h.set_text(doc.buffer.rope());
                }
                let len = doc.buffer.len();
                if doc.selected_range.start > len {
                    doc.selected_range.start = len;
                }
                if doc.selected_range.end > len {
                    doc.selected_range.end = len;
                }
                len
            };
            // Keep any pane viewing this buffer in bounds after the edit.
            for p in &mut self.panes {
                if p.buffer == dst && p.point > len {
                    p.point = len;
                }
            }
            cx.notify();
        }
    }

    /// Paint `face` over lines [sline,eline) of buffer `name` (active or not).
    pub(super) fn buffer_highlight_lines(
        &mut self,
        name: &str,
        sline: usize,
        eline: usize,
        face: String,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.doc_mut(name) else {
            return;
        };
        let (s, e) = line_span(&doc.buffer, sline, eline);
        if s < e {
            doc.overlays.faces.push((s..e, SharedString::from(face)));
            cx.notify();
        }
    }

    /// Drop all face overlays from buffer `name` (active or inactive).
    pub(super) fn buffer_clear_highlights(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(doc) = self.doc_mut(name)
            && !doc.overlays.faces.is_empty()
        {
            doc.overlays.faces.clear();
            cx.notify();
        }
    }

    // --- yasnippet session ------------------------------------------------
}

#[cfg(test)]
mod tests {
    use super::{file_identity, uniquified_name};
    use std::path::PathBuf;

    #[test]
    fn duplicate_file_names_get_distinguishing_dir_suffixes() {
        let p = PathBuf::from;
        let taken = |names: &'static [&'static str]| move |n: &str| names.contains(&n);
        // Free name → as-is.
        assert_eq!(
            uniquified_name("mod.rs", &p("/x/src/script/mod.rs"), &[], taken(&[])),
            "mod.rs"
        );
        // The nearest differing parent dir.
        assert_eq!(
            uniquified_name(
                "mod.rs",
                &p("/x/src/script/mod.rs"),
                &[p("/x/src/editor/mod.rs")],
                taken(&["mod.rs"]),
            ),
            "mod.rs<script>"
        );
        // Same immediate parent → go up until they differ.
        assert_eq!(
            uniquified_name(
                "lib.rs",
                &p("/w/b/src/lib.rs"),
                &[p("/w/a/src/lib.rs")],
                taken(&["lib.rs"]),
            ),
            "lib.rs<b/src>"
        );
        // A non-file buffer holding the name (no path to compare) still gets
        // a dir suffix; an occupied suffixed name falls through to numbers.
        assert_eq!(
            uniquified_name("notes", &p("/n/notes"), &[], taken(&["notes"])),
            "notes<n>"
        );
        assert_eq!(
            uniquified_name("notes", &p("/notes"), &[], taken(&["notes"])),
            "notes<2>"
        );
    }

    #[test]
    fn file_identity_unifies_spellings_of_one_file() {
        let dir = std::env::temp_dir().join(format!("macros-identity-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("sub/f.txt");
        std::fs::write(&file, "x").unwrap();
        let dotted = dir.join("sub/../sub/./f.txt");
        assert_eq!(file_identity(&dotted), file_identity(&file));
        #[cfg(unix)]
        {
            let link = dir.join("link.txt");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert_eq!(file_identity(&link), file_identity(&file));
        }
        // Not yet on disk: canonical dir + name, still matching other spellings.
        assert_eq!(
            file_identity(&dir.join("sub/../new.txt")),
            file_identity(&dir.join("new.txt"))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
