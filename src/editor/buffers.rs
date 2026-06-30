//! Buffer list, file I/O, save/kill, and FS watching for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{Context, ExternalPaths, SharedString, Window, px};
use notify_debouncer_mini::notify::RecursiveMode;

use crate::buffer::Buffer;

use super::*;

impl Editor {
    /// Names of all open buffers (active first).
    pub(super) fn buffer_names(&self) -> Vec<String> {
        let mut names = vec![self.active_name.clone()];
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
                std::env::current_dir()
                    .map_or_else(|_| p.to_path_buf(), |cwd| cwd.join(p))
                    .to_string_lossy()
                    .into_owned()
            }
        };
        let mut paths = HashMap::new();
        if let Some(p) = self.active_path.as_deref() {
            paths.insert(self.active_name.clone(), abs(p));
        }
        for d in &self.inactive {
            if let Some(p) = d.path.as_deref() {
                paths.insert(d.name.clone(), abs(p));
            }
        }
        self.script.set_buffer_paths(paths);
    }

    pub fn buffer_name(&self) -> &str {
        &self.active_name
    }

    /// Snapshot the active buffer's state into a [`Document`], emptying the
    /// active fields (the caller installs another document next).
    pub(super) fn snapshot_active(&mut self) -> Document {
        // Flush any pending full-sync change while this buffer is still active —
        // after the swap below `self.buffer` is a different document.
        self.flush_lsp_fullsync();
        Document {
            name: std::mem::take(&mut self.active_name),
            path: self.active_path.take(),
            buffer: std::mem::take(&mut self.buffer),
            selected_range: std::mem::replace(&mut self.selected_range, 0..0),
            selection_reversed: std::mem::take(&mut self.selection_reversed),
            marked_range: self.marked_range.take(),
            mark_active: std::mem::take(&mut self.mark_active),
            undo_tree: std::mem::replace(&mut self.undo_tree, new_undo_tree()),
            undo_current: std::mem::take(&mut self.undo_current),
            scroll_offset: self.scroll_offset,
            modes: std::mem::take(&mut self.modes),
            highlighter: self.highlighter.take(),
            modified: self.modified,
            hooks_ran: self.hooks_ran,
            special: self.special.take(),
            line_faces: std::mem::take(&mut self.line_faces),
            line_icons: std::mem::take(&mut self.line_icons),
            line_images: std::mem::take(&mut self.line_images),
            overlays: std::mem::take(&mut self.overlays),
            locals: std::mem::take(&mut self.locals),
            diff_marks: std::mem::take(&mut self.diff_marks),
        }
    }

    pub(super) fn install_active(&mut self, doc: Document) {
        // Destructured (not field-by-field on `doc`) so adding a buffer-local
        // field to `Document` is a compile error here until it's restored too —
        // otherwise a new field would silently leak across buffer switches.
        let Document {
            name,
            path,
            buffer,
            selected_range,
            selection_reversed,
            marked_range,
            mark_active,
            undo_tree,
            undo_current,
            scroll_offset,
            modes,
            highlighter,
            modified,
            hooks_ran,
            special,
            line_faces,
            line_icons,
            line_images,
            overlays,
            locals,
            diff_marks,
        } = doc;
        self.active_name = name;
        self.active_path = path;
        self.buffer = buffer;
        self.selected_range = selected_range;
        self.selection_reversed = selection_reversed;
        self.marked_range = marked_range;
        self.mark_active = mark_active;
        self.undo_tree = undo_tree;
        self.undo_current = undo_current;
        self.scroll_offset = scroll_offset;
        self.modes = modes;
        self.highlighter = highlighter;
        self.modified = modified;
        self.hooks_ran = hooks_ran;
        self.special = special;
        self.line_faces = line_faces;
        self.line_icons = line_icons;
        self.line_images = line_images;
        self.overlays = overlays;
        self.locals = locals;
        self.diff_marks = diff_marks;
        // Markers are positions in the previous buffer; drop them on a switch.
        self.script.clear_markers();
        // Secondary cursors / snippet fields are marker-backed, so they're gone
        // with the markers — forget their (now dangling) ids and any avy hints.
        self.extra_cursors.clear();
        self.snippet_fields.clear();
        self.avy_hints.clear();
        // The selected window now shows this buffer (Emacs: switching the active
        // buffer changes what the selected window displays).
        let active = self.active_name.clone();
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
    }

    /// Make the buffer `name` active, snapshotting the current one.
    pub(super) fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        let target = self.inactive.remove(index);
        let current = self.snapshot_active();
        self.inactive.push(current);
        self.install_active(target);
        self.parse_full();
        self.ensure_cursor_visible();
        self.run_mode_hooks_once(cx);
        cx.notify();
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
        let name = self.active_name.clone();
        self.echo(name, cx);
    }

    /// `C-x C-f`: visit a file, reusing its buffer if already open.
    pub(super) fn find_file(&mut self, input: &str, cx: &mut Context<Self>) {
        let path = PathBuf::from(expand_path(input.trim()));
        if path.as_os_str().is_empty() {
            return;
        }
        if self.active_path.as_deref() == Some(path.as_path()) {
            self.echo(format!("Already in {}", self.active_name), cx);
            return;
        }
        if let Some(index) = self
            .inactive
            .iter()
            .position(|d| d.path.as_deref() == Some(path.as_path()))
        {
            self.activate(index, cx);
            self.echo(format!("Switched to {}", self.active_name), cx);
            return;
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
            .spawn(async move { std::fs::read_to_string(&read_path).unwrap_or_default() });
        cx.spawn(async move |this, cx| {
            let contents = read.await;
            this.update(cx, |editor, cx| editor.open_file(name, path, contents, cx))
                .ok();
        })
        .detach();
    }

    /// Install a freshly-read file as the active buffer (foreground half of the
    /// async `find_file`).
    pub(super) fn open_file(
        &mut self,
        name: String,
        path: PathBuf,
        contents: String,
        cx: &mut Context<Self>,
    ) {
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
        match std::fs::write(&target, self.buffer.full_text()) {
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
                let new_id = self.next_pane_id; // split assigns this id to the new pane
                // Narrow split: the sidebar (the current pane) keeps ~22%.
                self.split_window_ratio(false, 0.22, cx);
                new_id
            }
        };
        self.select_pane(target, cx);
        self.find_file(&path, cx);
    }

    /// `:e!` / `M-x revert-buffer`: re-read the visited file from disk,
    /// discarding unsaved changes. Point is kept (clamped to the new length).
    pub(super) fn revert_buffer(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active_path.clone() else {
            self.echo("Buffer is not visiting a file", cx);
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let point = self.point().min(contents.len());
                let len = self.buffer.len();
                self.edit(0..len, &contents);
                self.parse_full();
                self.move_to(point.min(self.buffer.len()), cx);
                self.modified = false;
                self.echo(format!("Reverted {}", self.active_name), cx);
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
                if self.fs_watch_dirs.contains(dir) && !changed_dirs.iter().any(|d| d == dir) {
                    changed_dirs.push(dir.to_path_buf());
                }
            }
            // Recursive roots: a change anywhere in the subtree fires the hook
            // with the ROOT, deduped so a bulk change refreshes magit once.
            for root in &self.fs_watch_trees {
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
        let visited = self.active_path.as_deref() == Some(path)
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
        if self.active_path.as_deref() == Some(path) {
            if self.buffer.full_text() == disk {
                return; // identical (e.g. our own write) — nothing to revert
            }
            if self.modified {
                self.echo(
                    format!("{} changed on disk (unsaved edits kept)", self.active_name),
                    cx,
                );
                return;
            }
            let point = self.point().min(disk.len());
            let len = self.buffer.len();
            self.edit(0..len, &disk);
            self.parse_full();
            self.move_to(point.min(self.buffer.len()), cx);
            self.modified = false;
            self.echo(
                format!("Reverted {} (changed on disk)", self.active_name),
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
            self.echo(format!("Reverted {name} (changed on disk)"), cx);
        }
    }

    /// Fire every `directory-changed-hook` function with `dir` as its argument, so
    /// a Scheme view (dired-sidebar/dired) can re-render if `dir` is the one it shows.
    /// No-op — and no thread hop — when nothing is registered on the hook.
    pub(super) fn dispatch_dir_changed(&mut self, dir: &Path) {
        let Some(functions) = self.hooks.get("directory-changed-hook") else {
            return;
        };
        if functions.is_empty() {
            return;
        }
        let functions = functions.clone();
        self.publish_point();
        let arg = dir.to_string_lossy();
        for function in functions {
            self.script.dispatch_action(&function, Some(&arg));
        }
    }

    /// Sync the OS watcher with what should be watched right now: every Scheme-
    /// requested directory, plus — when `auto-revert` is on — every file-visiting
    /// buffer. Diffs against `fs_watched` so only the delta is (un)watched. Cheap
    /// and idempotent; call it whenever the buffer set or the option changes.
    pub(super) fn reconcile_watches(&mut self, cx: &mut Context<Self>) {
        if self.fs_debouncer.is_none() {
            return;
        }
        let mut desired: HashSet<PathBuf> = self.fs_watch_dirs.clone();
        if cx.global::<crate::settings::Settings>().auto_revert() {
            if let Some(path) = &self.active_path {
                desired.insert(path.clone());
            }
            for doc in &self.inactive {
                if let Some(path) = &doc.path {
                    desired.insert(path.clone());
                }
            }
        }
        let debouncer = self.fs_debouncer.as_mut().unwrap();
        for path in &desired {
            if !self.fs_watched.contains(path)
                && debouncer
                    .watcher()
                    .watch(path, RecursiveMode::NonRecursive)
                    .is_ok()
            {
                self.fs_watched.insert(path.clone());
            }
        }
        let stale: Vec<PathBuf> = self.fs_watched.difference(&desired).cloned().collect();
        for path in stale {
            let _ = debouncer.watcher().unwatch(&path);
            self.fs_watched.remove(&path);
        }

        // Recursive roots (magit repo watch) — same delta dance, recursive mode.
        let want_rec = self.fs_watch_trees.clone();
        for path in &want_rec {
            if !self.fs_watched_recursive.contains(path)
                && debouncer
                    .watcher()
                    .watch(path, RecursiveMode::Recursive)
                    .is_ok()
            {
                self.fs_watched_recursive.insert(path.clone());
            }
        }
        let stale_rec: Vec<PathBuf> = self
            .fs_watched_recursive
            .difference(&want_rec)
            .cloned()
            .collect();
        for path in stale_rec {
            let _ = debouncer.watcher().unwatch(&path);
            self.fs_watched_recursive.remove(&path);
        }
    }

    /// `C-x b`: switch to an already-open buffer by name.
    pub(super) fn switch_to_buffer(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if name == self.active_name {
            cx.notify();
            return;
        }
        match self.inactive.iter().position(|d| d.name == name) {
            Some(index) => self.activate(index, cx),
            None => self.echo(format!("No buffer named {name}"), cx),
        }
    }

    /// `C-x C-s`: write the active buffer back to its file.
    pub(super) fn save_buffer(&mut self, cx: &mut Context<Self>) {
        if self.active_path.is_none() {
            self.echo("Buffer is not visiting a file", cx);
            return;
        }
        if self.formatting_for_save {
            // Re-entry after an LSP format-on-save completed: just write now.
            self.formatting_for_save = false;
        } else {
            // First entry: run synchronous before-save hooks (e.g. a shell
            // formatter), then — if this language formats via LSP on save —
            // kick off the (async) format and defer the write to its response.
            self.run_hook_blocking("before-save-hook", cx);
            if self.start_lsp_format_on_save(cx) {
                return;
            }
        }
        self.write_active_to_disk(cx);
    }

    /// Save every modified file-visiting buffer (Emacs `save-some-buffers`).
    /// Inactive buffers are written synchronously here; the active one goes
    /// through the normal (async, hook-running) save path.
    pub(super) fn save_some_buffers(&mut self, cx: &mut Context<Self>) {
        let mut saved = 0;
        for doc in &mut self.inactive {
            if doc.modified
                && let Some(path) = doc.path.clone()
                && std::fs::write(&path, doc.buffer.full_text()).is_ok()
            {
                doc.modified = false;
                saved += 1;
            }
        }
        if self.active_path.is_some() && self.modified {
            self.write_active_to_disk(cx);
            saved += 1;
        }
        self.echo(
            match saved {
                0 => "No unsaved buffers".to_string(),
                1 => "Saved 1 buffer".to_string(),
                n => format!("Saved {n} buffers"),
            },
            cx,
        );
    }

    /// Write the active buffer to its file off the UI thread. Optimistically
    /// clears `modified`; restores it (and reports) if the write fails.
    pub(super) fn write_active_to_disk(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active_path.clone() else {
            return;
        };
        let text = self.buffer.full_text();
        let name = self.active_name.clone();
        self.modified = false;
        let write = cx
            .background_executor()
            .spawn(async move { std::fs::write(&path, text) });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |editor, cx| match result {
                Ok(()) => {
                    editor.echo(format!("Wrote {name}"), cx);
                    editor.run_hook("after-save-hook"); // Emacs after-save-hook
                }
                Err(err) => {
                    editor.modified = true;
                    editor.echo(format!("Save failed: {err}"), cx);
                }
            })
            .ok();
        })
        .detach();
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
        self.formatting_for_save = true;
        self.lsp_format(cx);
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

    pub(super) fn any_modified(&self) -> bool {
        self.modified || self.inactive.iter().any(|d| d.modified)
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Handle an OS window-close request (the title-bar close button) and
    /// `C-x C-c`. If any buffer has unsaved changes, offer to save them first:
    /// `yes` saves all modified file buffers then quits, `no` quits discarding,
    /// `C-g` cancels. With nothing unsaved, quit straight away. Always returns
    /// `false` so the bare window-close is cancelled — quitting (`cx.quit()`)
    /// tears the whole app down rather than leaving a windowless process on
    /// macOS.
    pub fn confirm_quit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.any_modified() {
            self.open_minibuffer(
                "Save modified buffers before quitting? (yes/no, C-g to cancel) ",
                MinibufferPurpose::QuitSave,
                "",
                cx,
            );
        } else {
            cx.quit();
        }
        false
    }

    /// Write every modified, file-visiting buffer (active + inactive) to disk
    /// synchronously. Used on quit, so writes are blocking (no UI to keep
    /// responsive) and skip the async format-on-save path. Buffers not visiting
    /// a file (e.g. `*scratch*`) can't be saved and are left untouched.
    pub(super) fn save_all_modified(&mut self, cx: &mut Context<Self>) {
        if self.modified
            && let Some(path) = self.active_path.clone()
        {
            match std::fs::write(&path, self.buffer.full_text()) {
                Ok(()) => self.modified = false,
                Err(err) => self.echo(format!("Save failed: {err}"), cx),
            }
        }
        for doc in &mut self.inactive {
            if doc.modified
                && let Some(path) = doc.path.clone()
                && std::fs::write(&path, doc.buffer.full_text()).is_ok()
            {
                doc.modified = false;
            }
        }
    }

    /// `C-x k`: kill the active buffer (confirming first if it has unsaved
    /// changes), then show the most-recent other buffer (or a fresh scratch).
    pub(super) fn kill_buffer(&mut self, cx: &mut Context<Self>) {
        if self.modified {
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
    /// window`), confirming first if the buffer has unsaved changes. With a sole
    /// window, the buffer is killed and `delete_window` just declines.
    pub(super) fn kill_buffer_and_window(&mut self, cx: &mut Context<Self>) {
        if self.modified {
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
        let killed = self.active_name.clone();
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
    /// `Drop` closes the PTY and ends the event-loop thread).
    pub(super) fn drop_terminal_for_buffer(&mut self, name: &str) {
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
        if name == self.active_name {
            self.kill_active_buffer(cx);
        } else if let Some(idx) = self.inactive.iter().position(|d| d.name == name) {
            self.inactive.remove(idx);
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
        self.line_faces = faces;
        self.move_to(0, cx);
        self.modified = false;
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
        if self.active_name == name {
            // Active: identical to the in-place branch of `render_buffer`.
            self.line_icons.clear();
            let line = self.buffer.line_index_at_byte(self.point());
            self.buffer = Buffer::new(&text);
            self.line_faces = faces;
            self.lines.clear();
            let last = self.buffer.line_count().saturating_sub(1);
            let offset = self.buffer.line_range(line.min(last)).start;
            self.move_to(offset, cx);
            self.modified = false;
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
        if self.active_name != name {
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
                self.activate(idx, cx);
            } else {
                let current = self.snapshot_active();
                self.inactive.push(current);
                self.install_active(make_document(name, None, ""));
            }
        }
        let end = self.buffer.len();
        self.buffer.replace(end..end, &text);
        self.lines.clear();
        let new_end = self.buffer.len();
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
        if self.active_name == name {
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

    /// Borrow the rope buffer named `name` — the active buffer or any inactive
    /// pane buffer. Used by the cross-buffer ediff primitives.
    pub(super) fn buffer_named(&self, name: &str) -> Option<&Buffer> {
        if self.active_name == name {
            Some(&self.buffer)
        } else {
            self.inactive
                .iter()
                .find(|d| d.name == name)
                .map(|d| &d.buffer)
        }
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
        if self.active_name == dst {
            let (s, e) = line_span(&self.buffer, ds, de);
            self.replace_region(s, e, &text, cx);
        } else if let Some(idx) = self.inactive.iter().position(|d| d.name == dst) {
            let len = {
                let doc = &mut self.inactive[idx];
                let (s, e) = line_span(&doc.buffer, ds, de);
                doc.buffer.replace(s..e, &text);
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
        if self.active_name == name {
            let (s, e) = line_span(&self.buffer, sline, eline);
            if s < e {
                self.overlays.faces.push((s..e, SharedString::from(face)));
                cx.notify();
            }
        } else if let Some(doc) = self.inactive.iter_mut().find(|d| d.name == name) {
            let (s, e) = line_span(&doc.buffer, sline, eline);
            if s < e {
                doc.overlays.faces.push((s..e, SharedString::from(face)));
                cx.notify();
            }
        }
    }

    /// Drop all face overlays from buffer `name` (active or inactive).
    pub(super) fn buffer_clear_highlights(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.active_name == name {
            if !self.overlays.faces.is_empty() {
                self.overlays.faces.clear();
                cx.notify();
            }
        } else if let Some(doc) = self.inactive.iter_mut().find(|d| d.name == name)
            && !doc.overlays.faces.is_empty()
        {
            doc.overlays.faces.clear();
            cx.notify();
        }
    }

    // --- yasnippet session ------------------------------------------------
}
