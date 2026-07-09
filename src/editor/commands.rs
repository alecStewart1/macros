//! Editor command dispatch for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::path::PathBuf;

use gpui::{Context, SharedString, prelude::*, px};

use crate::highlight::Highlighter;
use crate::mode::{Mode, ModeStack};
use crate::script::EditorCommand;
use crate::theme::Theme;

use super::*;

impl Editor {
    /// Apply a command, honoring a pending universal-argument (Emacs `C-u N`):
    /// a repeatable command runs `N` times (as one undo group for edits); others
    /// run once and the argument is discarded. Every command — from a key, a
    /// script, or an OS-special action — flows through here.
    pub(super) fn apply_command(&mut self, command: EditorCommand, cx: &mut Context<Self>) {
        match self.uarg.pending.take() {
            Some(n) if n > 1 && is_repeatable(&command) => {
                self.status = SharedString::default();
                self.begin_undo_group();
                for _ in 0..n {
                    self.apply_command_once(command.clone(), cx);
                }
                self.end_undo_group();
            }
            _ => self.apply_command_once(command, cx),
        }
    }

    /// Apply a single command exactly once.
    pub(super) fn apply_command_once(&mut self, command: EditorCommand, cx: &mut Context<Self>) {
        let yank_like = matches!(command, EditorCommand::Yank | EditorCommand::YankPop);
        self.dispatch_command(command, cx);
        self.last_was_yank = yank_like;
    }

    /// Route a command to the first category dispatcher that owns it. Each
    /// `cmd_*` handler returns `Err(command)` for variants it does not own, so
    /// the command falls through to the next; the partition is exhaustive.
    fn dispatch_command(&mut self, command: EditorCommand, cx: &mut Context<Self>) {
        let command = match self.cmd_motion(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_edit(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_view(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_buffer(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_window(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_lsp(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_completion(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_helm(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_evil(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_overlay(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_proc(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_extras(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        let command = match self.cmd_misc(command, cx) {
            Ok(()) => return,
            Err(c) => c,
        };
        debug_assert!(false, "unhandled editor command: {command:?}");
        let _ = command;
    }

    /// Cursor motion, selection, marks, and goto.
    fn cmd_motion(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            ForwardChar => {
                let o = self.active.buffer.next_grapheme(self.point());
                self.goto(o, cx);
            }
            BackwardChar => {
                let o = self.active.buffer.prev_grapheme(self.point());
                self.goto(o, cx);
            }
            NextLine => {
                let o = self.line_offset(1);
                self.goto(o, cx);
            }
            PreviousLine => {
                let o = self.line_offset(-1);
                self.goto(o, cx);
            }
            ForwardWord => {
                let o = self.active.buffer.forward_word(self.point());
                self.goto(o, cx);
            }
            ForwardWordStart => {
                let o = self.active.buffer.forward_word_start(self.point());
                self.goto(o, cx);
            }
            BackwardWord => {
                let o = self.active.buffer.backward_word(self.point());
                self.goto(o, cx);
            }
            BeginningOfLine => {
                let o = self.active.buffer.line_range_at(self.point()).start;
                self.goto(o, cx);
            }
            EndOfLine => {
                let o = self.active.buffer.line_range_at(self.point()).end;
                self.goto(o, cx);
            }
            BeginningOfBuffer => self.goto(0, cx),
            EndOfBuffer => {
                let o = self.active.buffer.len();
                self.goto(o, cx);
            }
            ExtendBackwardChar => {
                let o = self.active.buffer.prev_grapheme(self.point());
                self.select_to(o, cx);
            }
            ExtendForwardChar => {
                let o = self.active.buffer.next_grapheme(self.point());
                self.select_to(o, cx);
            }
            SetMark => self.set_mark(cx),
            ExchangePointAndMark => self.exchange_point_and_mark(cx),
            MarkWholeBuffer => self.mark_whole_buffer(cx),
            GotoLine(n) => {
                let last = self.active.buffer.line_count().saturating_sub(1);
                let offset = self.active.buffer.line_range(n.min(last)).start;
                self.move_to(offset, cx);
            }
            ScrollCursorToRow(row) => self.scroll_cursor_to_row(row, cx),
            GotoChar(n) => {
                let offset = n.min(self.active.buffer.len());
                self.move_to(offset, cx);
            }
            SelectTo(n) => {
                let offset = n.min(self.active.buffer.len());
                self.select_to(offset, cx);
            }
            SetRegion { start, end } => {
                let len = self.active.buffer.len();
                self.active.mark_active = true;
                self.active.selected_range = start.min(len)..end.min(len);
                self.active.selection_reversed = false;
                self.ensure_cursor_visible();
                cx.notify();
            }
            other => return Err(other),
        }
        Ok(())
    }

    /// Text insertion, deletion, kill ring, and undo.
    fn cmd_edit(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            Insert(text) => self.insert_text(&text, cx),
            Newline => self.newline(cx),
            OpenLine => self.open_line(cx),
            IndentForTab => self.indent_for_tab(cx),
            DeleteChar => self.delete_char_forward(cx),
            DeleteBackwardChar => self.delete_char_backward(cx),
            TransposeChars => self.transpose_chars(cx),
            KillLine => self.kill_line(cx),
            KillWord => self.kill_word(cx),
            BackwardKillWord => self.backward_kill_word(cx),
            KillRegion => self.kill_region(cx),
            KillRingSave => self.kill_ring_save(cx),
            ClipboardCopy => self.clipboard_copy(cx),
            ClipboardCut => self.clipboard_cut(cx),
            ClipboardPaste => self.clipboard_paste(cx),
            ClipboardSet(text) => self.set_clipboard(&text, cx),
            Yank => self.yank(cx),
            YankPop => self.yank_pop(cx),
            Undo => self.undo(cx),
            Redo => self.redo(cx),
            UndoTreeDump(callback) => {
                let dump = self.undo_tree_dump();
                self.script.dispatch_action(&callback, Some(&dump));
            }
            DeleteRegion { start, end } => self.delete_region(start, end, cx),
            KillRegionBetween { start, end } => self.kill_range(start.min(end)..start.max(end), cx),
            ReplaceRegion { start, end, text } => self.replace_region(start, end, &text, cx),
            InsertAt { pos, text } => self.insert_at(pos, &text, cx),
            UndoBoundary => self.undo_boundary(),
            other => return Err(other),
        }
        Ok(())
    }

    /// Scrolling, toggles, and per-line decorations.
    fn cmd_view(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            ScrollUp => self.scroll_page(1, cx),
            ScrollDown => self.scroll_page(-1, cx),
            Recenter => self.recenter(cx),
            SetLineIcon(line, col, name) => {
                self.redirect_target()
                    .line_icons
                    .insert(line, (col, SharedString::from(name)));
                cx.notify();
            }
            ClearLineIcons => {
                self.redirect_target().line_icons.clear();
                cx.notify();
            }
            SetLineImage {
                line,
                path,
                max_width,
            } => self.set_line_image(line, path, max_width, cx),
            ClearLineImages => {
                let redirected = self.render_redirect.is_some();
                let images = self.redirect_target().line_images;
                if !images.is_empty() {
                    images.clear();
                    // Only the active buffer's rows moved under the cursor.
                    if !redirected {
                        self.ensure_cursor_visible();
                    }
                    cx.notify();
                }
            }
            SetLineIndent { line, cols } => {
                let indents = &mut self.redirect_target().overlays.indents;
                if cols == 0 {
                    indents.remove(&line);
                } else {
                    indents.insert(line, cols);
                }
                cx.notify();
            }
            ClearLineIndents => {
                let indents = &mut self.redirect_target().overlays.indents;
                if !indents.is_empty() {
                    indents.clear();
                    cx.notify();
                }
            }
            SetLineScale { line, scale } => {
                let scales = &mut self.redirect_target().overlays.scales;
                if scale <= 1.0 {
                    scales.remove(&line);
                } else {
                    scales.insert(line, scale);
                }
                cx.notify();
            }
            ClearLineScales => {
                let scales = &mut self.redirect_target().overlays.scales;
                if !scales.is_empty() {
                    scales.clear();
                    cx.notify();
                }
            }
            ToggleLineNumbers => {
                // Toggle for the current buffer's major mode, so it mirrors
                // Emacs's per-mode `display-line-numbers-mode`.
                let major = self.active.modes.names().last().map(|s| s.to_string());
                let key = match &major {
                    Some(m) => format!("line-numbers:{m}"),
                    None => "line-numbers".to_string(),
                };
                let on = cx
                    .global::<crate::settings::Settings>()
                    .line_numbers_for(major.as_deref());
                self.toggle_setting(key, on, "Line numbers", cx);
            }
            ToggleScrollBar => {
                let on = cx.global::<crate::settings::Settings>().scroll_bar();
                self.toggle_setting("scroll-bar", on, "Scroll bar", cx);
            }
            ToggleWordWrap => {
                // Toggle for the current buffer's major mode, mirroring Emacs's
                // per-mode `visual-line-mode`.
                let major = self.active.modes.names().last().map(|s| s.to_string());
                let key = match &major {
                    Some(m) => format!("word-wrap:{m}"),
                    None => "word-wrap".to_string(),
                };
                let on = cx
                    .global::<crate::settings::Settings>()
                    .word_wrap_for(major.as_deref());
                // Leaving word-wrap resets any horizontal scroll to a clean state.
                self.h_scroll_offset = px(0.);
                self.toggle_setting(key, on, "Word wrap", cx);
            }
            ToggleGitGutter => {
                let on = cx.global::<crate::settings::Settings>().magit_gutter();
                self.toggle_setting("magit-gutter", on, "Git gutter", cx);
                self.refresh_git_gutter(cx);
            }
            GitGutterRefresh => self.refresh_git_gutter(cx),
            ToggleElectricPair => {
                self.electric_pair = !self.electric_pair;
                self.echo(
                    format!(
                        "electric-pair-mode {}",
                        if self.electric_pair {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ),
                    cx,
                );
            }
            other => return Err(other),
        }
        Ok(())
    }

    /// Buffer list, files, saving, and directory watching.
    fn cmd_buffer(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            FindFile => self.find_file_prompt(cx),
            OpenFile(path) => self.find_file(&path, cx),
            SwitchBuffer => {
                self.open_minibuffer(
                    "Switch to buffer: ",
                    MinibufferPurpose::SwitchBuffer,
                    "",
                    cx,
                );
            }
            SwitchToBufferNamed(name) => self.switch_to_buffer(&name, cx),
            KillBufferNamed(name) => self.kill_buffer_named(&name, cx),
            SetMajorMode(mode) => {
                let mut modes = ModeStack::default();
                modes.push(Mode::new("fundamental"));
                // A mode whose name matches a bundled tree-sitter language carries
                // that language, so highlighting AND the language server follow the
                // mode — `M-x rust-mode` on any buffer colors it and (via the
                // `fundamental` `enable-lsp` hook below) starts rust-analyzer.
                // Modes with no grammar (shell-mode, git-commit-mode, …) install
                // only the keymap and leave any existing highlighter alone.
                let language_highlighter = Highlighter::for_language(&mode);
                let mut new_mode = Mode::new(&mode);
                if language_highlighter.is_some() {
                    new_mode = new_mode.with_language(&mode);
                }
                modes.push(new_mode);
                self.active.modes = modes;
                if let Some(h) = language_highlighter {
                    self.active.highlighter = Some(h);
                    self.parse_full();
                } else if mode == "anvil-repl-mode" {
                    // The ANVIL REPL displays Scheme, so highlight it in place — no
                    // need to swap to `macros-steel`, which would discard the
                    // anvil-repl keymap (RET → anvil-send-input) and kill the REPL.
                    //
                    // Drive the highlighter DIRECTLY rather than tagging the mode
                    // with a `language`: a mode language also feeds
                    // `active_language`, which `enable-lsp` keys off — so it would
                    // attach the builtin Steel LSP and lint the transcript (the
                    // `;; =>` scrollback isn't a program). The REPL introspects its
                    // live image instead (completion, doc, apropos — see
                    // steel-anvil).
                    self.active.highlighter = Highlighter::scheme();
                    self.parse_full();
                }
                // Entering a mode runs its hooks (Emacs `run-mode-hooks`): a user's
                // `(add-hook "rust" …)` fires whether the buffer reached rust-mode
                // by file detection or `M-x rust-mode`, and `enable-lsp` brings up
                // the server for the now-active language. Idempotent — re-running
                // `fundamental`'s hooks is a no-op when nothing changed.
                self.active.hooks_ran = false;
                self.run_mode_hooks_once(cx);
                cx.notify();
            }
            NextBuffer => self.next_buffer(cx),
            PreviousBuffer => self.previous_buffer(cx),
            SaveBuffer => self.save_buffer(cx),
            SaveSomeBuffers => self.save_some_buffers(cx),
            RevertBuffer => self.revert_buffer(cx),
            OpenFileOtherWindow(path) => self.open_file_other_window(path, cx),
            KillBuffer => self.kill_buffer(cx),
            KillBufferAndWindow => self.kill_buffer_and_window(cx),
            Quit => self.quit(cx),
            SetExtension(ext, language) => {
                self.extensions.insert(ext, language);
            }
            WatchDirectories(owner, dirs) => {
                let dirs: Vec<PathBuf> = dirs.into_iter().map(PathBuf::from).collect();
                if dirs.is_empty() {
                    self.fs.owners.remove(&owner);
                } else {
                    self.fs.owners.insert(owner, dirs);
                }
                // Rebuild the flattened membership set, then sync the watcher.
                self.fs.dirs = self.fs.owners.values().flatten().cloned().collect();
                self.reconcile_watches(cx);
            }
            WatchTree(owner, dirs) => {
                let dirs: Vec<PathBuf> = dirs.into_iter().map(PathBuf::from).collect();
                if dirs.is_empty() {
                    self.fs.tree_owners.remove(&owner);
                } else {
                    self.fs.tree_owners.insert(owner, dirs);
                }
                self.fs.trees = self.fs.tree_owners.values().flatten().cloned().collect();
                self.reconcile_watches(cx);
            }
            RenderBuffer {
                name,
                mode,
                text,
                line_faces,
            } => self.render_buffer(name, mode, text, line_faces, cx),
            RenderBufferQuiet {
                name,
                mode,
                text,
                line_faces,
            } => self.render_buffer_quiet(name, mode, text, line_faces, cx),
            AppendToBuffer { name, text } => self.append_to_buffer(name, text, cx),
            FontifyOrgBuffer(name) => self.fontify_org_buffer(name, cx),
            DiffBuffers { a, b, callback } => self.diff_buffers(&a, &b, &callback),
            BufferCopyLines {
                src,
                src_start,
                src_end,
                dst,
                dst_start,
                dst_end,
            } => self.buffer_copy_lines(&src, src_start, src_end, &dst, dst_start, dst_end, cx),
            BufferHighlightLines {
                name,
                start,
                end,
                face,
            } => self.buffer_highlight_lines(&name, start, end, face, cx),
            BufferClearHighlights { name } => self.buffer_clear_highlights(&name, cx),
            BufferLocalSet { key, value } => {
                self.active.locals.insert(key, value);
            }
            ReadFileName { prompt, callback } => {
                let initial = self.default_directory_input();
                self.open_minibuffer(
                    &prompt,
                    MinibufferPurpose::SchemeReadFile(callback),
                    &initial,
                    cx,
                );
            }
            other => return Err(other),
        }
        Ok(())
    }

    /// Window splits and the transient menu.
    fn cmd_window(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            SelectWindow(id) => self.select_pane(id, cx),
            Transient {
                title,
                columns,
                sticky,
            } => self.open_transient(title, columns, sticky, cx),
            TransientQuit => self.transient_quit(cx),
            SplitWindowBelow => self.split_window(true, cx),
            SplitWindowRight => self.split_window(false, cx),
            SplitWindowRightRatio(ratio) => self.split_window_ratio(false, ratio as f32, cx),
            OtherWindow => self.other_window(cx),
            DeleteWindow => self.delete_window(cx),
            DeleteOtherWindows => self.delete_other_windows(cx),
            EnlargeWindow => self.resize_window(true, 0.05, cx),
            ShrinkWindow => self.resize_window(true, -0.05, cx),
            EnlargeWindowHorizontally => self.resize_window(false, 0.05, cx),
            ShrinkWindowHorizontally => self.resize_window(false, -0.05, cx),
            other => return Err(other),
        }
        Ok(())
    }

    /// Language server, xref, and tree-sitter requests.
    fn cmd_lsp(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            SetLsp(language, command) => {
                self.lsp_commands.insert(language, command);
            }
            SetLanguageId(ext, language_id) => {
                self.language_ids.insert(ext, language_id);
            }
            LspDefinition => self.lsp_goto("textDocument/definition", "definition", cx),
            LspDeclaration => self.lsp_goto("textDocument/declaration", "declaration", cx),
            LspTypeDefinition => {
                self.lsp_goto("textDocument/typeDefinition", "type definition", cx)
            }
            LspImplementation => self.lsp_goto("textDocument/implementation", "implementation", cx),
            LspReferences => self.lsp_references(cx),
            LspHover => self.lsp_hover(cx),
            LspSignatureHelp => self.lsp_signature_help(cx),
            LspDocumentSymbols => self.lsp_document_symbols(cx),
            LspCodeAction => self.lsp_code_action(cx),
            LspFormatBuffer => self.lsp_format(cx),
            LspCallHierarchyIncoming => self.lsp_call_hierarchy(true, cx),
            LspCallHierarchyOutgoing => self.lsp_call_hierarchy(false, cx),
            LspPrepareRename => self.lsp_prepare_rename(cx),
            LspCodeLens => self.lsp_code_lens(cx),
            LspXrefVisit => self.lsp_xref_visit(cx),
            LspApplyCodeAction => self.lsp_apply_code_action(cx),
            LspRename(name) => self.lsp_rename(name, cx),
            LspWorkspaceSymbol(query) => self.lsp_workspace_symbol(query, cx),
            LspRequest {
                method,
                kind,
                params,
                callback,
            } => self.lsp_script_request(method, kind, params, callback, cx),
            LspGoto {
                uri,
                line,
                character,
            } => self.goto_file_position(uri, line, character, cx),
            LspApplyWorkspaceEdit(json) => self.lsp_apply_workspace_edit_json(json, cx),
            LspApplyTextEdits(json) => self.lsp_apply_text_edits_json(json, cx),
            XrefPushMarker => self.xref_push_marker(),
            XrefPopMarker => self.xref_pop_marker(cx),
            SetLspFormatOnSave(lang, on) => {
                if on {
                    self.lsp_format_on_save.insert(lang);
                } else {
                    self.lsp_format_on_save.remove(&lang);
                }
            }
            LspSemanticTokens(callback) => self.lsp_semantic_tokens(callback, cx),
            LspSemanticTokensRange(callback) => self.lsp_semantic_tokens_range(callback, cx),
            LspSemanticTokensDelta(callback) => self.lsp_semantic_tokens_delta(callback, cx),
            TreeSitterQuery { query, callback } => self.tree_sitter_query(query, callback),
            TreeSitterNodeAt { byte, callback } => self.tree_sitter_node_at(byte, callback),
            EnableLsp => self.maybe_start_lsp(cx),
            other => return Err(other),
        }
        Ok(())
    }

    /// In-buffer completion.
    fn cmd_completion(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            CompletionAtPoint => self.completion_at_point(cx),
            CompletionNext => self.completion_next(cx),
            CompletionPrevious => self.completion_previous(cx),
            CompletionAccept => self.completion_accept(cx),
            CompletionAbort => self.completion_abort(cx),
            AddCapf(function) => {
                if !self.capf_functions.contains(&function) {
                    self.capf_functions.push(function);
                }
            }
            other => return Err(other),
        }
        Ok(())
    }

    /// Helm narrowing UI.
    fn cmd_helm(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            Helm(specs) => {
                let sources = helm_sources_from_specs(specs);
                self.helm_open("Helm: ".to_string(), sources, String::new(), cx);
            }
            HelmFrom(initial, specs) => {
                let sources = helm_sources_from_specs(specs);
                self.helm_open("Helm: ".to_string(), sources, initial, cx);
            }
            HelmMx => self.helm_mx(cx),
            HelmNext => self.helm_next(cx),
            HelmPrevious => self.helm_previous(cx),
            HelmAccept => self.helm_accept(cx),
            HelmAbort => self.helm_abort(cx),
            HelmActions => self.helm_action_menu(cx),
            HelmToggleMark => self.helm_toggle_mark(cx),
            other => return Err(other),
        }
        Ok(())
    }

    /// Evil modal states, motions, and incremental search.
    fn cmd_evil(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            HelixMode => self.helix_enable(cx),
            HelixDisable => self.helix_disable(cx),
            HelixNormalState => self.helix_normal_state(cx),
            EvilMode => self.evil_enable(cx),
            EvilNormalState => self.evil_normal_state(cx),
            EvilInsertState => self.evil_insert_state(cx),
            EvilVisualState => self.evil_visual_state(cx),
            EvilVisualLineState => self.evil_visual_line_state(cx),
            EvilVisualInsert => self.evil_visual_insert(false, cx),
            EvilVisualAppend => self.evil_visual_insert(true, cx),
            EvilVisualBlockState => self.evil_visual_block_state(cx),
            EvilBlockInsert => self.evil_block_insert(false, cx),
            EvilBlockAppend => self.evil_block_insert(true, cx),
            EvilBlockDelete => self.evil_block_delete(cx),
            EvilBlockYank => self.evil_block_yank(cx),
            EvilDisable => self.evil_disable(cx),
            EvilScrollDown => self.scroll_half(1, cx),
            EvilScrollUp => self.scroll_half(-1, cx),
            EvilSearchForward => self.evil_search_forward(cx),
            EvilSearchBackward => self.evil_search_backward(cx),
            EvilSearchNext => self.evil_search_next(cx),
            EvilSearchPrevious => self.evil_search_previous(cx),
            EvilPasteAfter => self.evil_paste(true, cx),
            EvilPasteBefore => self.evil_paste(false, cx),
            EvilReplaceChar => {
                // Arm replace-pending; the dispatcher captures the next key.
                self.evil_replace_pending = true;
                self.status = "r-".into();
                cx.notify();
            }
            ReadChar { prompt, callback } => {
                // Arm the capture; the key dispatcher feeds the next key to it.
                // If a keyboard macro is recording, drop the just-recorded
                // arming expr ("(evil-find-char-forward)"): replaying it would
                // re-arm the capture and hang waiting for a keystroke macro
                // playback can never provide. key_read_char records the
                // RESOLVED callback call in its place, so `f o` replays as one
                // self-contained command.
                if let Some(events) = self.macro_recording.as_mut() {
                    events.pop();
                }
                self.read_char_pending = Some(callback);
                self.status = prompt.into();
                cx.notify();
            }
            SetSearchQuery { query, forward } => {
                self.search_query = Some(query);
                self.evil_search_forward = forward;
            }
            EvilSetLinewise => self.evil_yank_linewise = true,
            EvilSetCharwise => self.evil_yank_linewise = false,
            EvilExCommand => {
                self.open_minibuffer(":", MinibufferPurpose::ExCommand, "", cx);
            }
            IsearchForward => self.isearch_start(true, cx),
            IsearchBackward => self.isearch_start(false, cx),
            other => return Err(other),
        }
        Ok(())
    }

    /// Overlays, folds, faces, and text properties.
    fn cmd_overlay(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            OverlayFace { start, end, face } => {
                let t = self.redirect_target();
                let len = t.buffer.len();
                let (start, end) = (start.min(len), end.min(len));
                if start < end {
                    t.overlays
                        .faces
                        .push((start..end, SharedString::from(face)));
                    cx.notify();
                }
            }
            OverlayHide { start, end } => {
                let t = self.redirect_target();
                let len = t.buffer.len();
                let (start, end) = (start.min(len), end.min(len));
                if start < end {
                    t.overlays.hidden.push(start..end);
                    cx.notify();
                }
            }
            FoldLines {
                start_line,
                end_line,
            } => self.overlay_fold_lines(start_line, end_line, cx),
            UnfoldAll => self.overlay_unfold_all(cx),
            SetFoldMarker { line, open } => self.overlay_set_fold_marker(line, open, cx),
            ClearFoldMarkers => self.overlay_clear_fold_markers(cx),
            ClearOverlays => {
                let redirected = self.render_redirect.is_some();
                *self.redirect_target().overlays = Overlays::default();
                // Only the active buffer's folds/rows moved under the cursor.
                if !redirected {
                    self.ensure_cursor_visible();
                }
                cx.notify();
            }
            ClearOverlayFaces => {
                // Faces only — NOT hides. Several features (rainbow-delimiters,
                // LSP highlight) repaint their faces on every edit via this; if
                // it also cleared hides it would wipe another feature's
                // concealment (e.g. markdown/org heading markers). Clear hides
                // with `clear-overlay-hides`.
                self.redirect_target().overlays.faces.clear();
                cx.notify();
            }
            ClearOverlayHides => {
                self.redirect_target().overlays.hidden.clear();
                cx.notify();
            }
            ClearLineSuffixes => {
                self.redirect_target().overlays.suffixes.clear();
                cx.notify();
            }
            PutTextProperty {
                start,
                end,
                key,
                value,
            } => {
                self.put_text_property(start, end, key, value);
            }
            SetLineSuffix { line, text, face } => {
                self.redirect_target()
                    .overlays
                    .suffixes
                    .insert(line, (text.into(), face.into()));
                cx.notify();
            }
            other => return Err(other),
        }
        Ok(())
    }

    /// Subprocesses, terminals, Anvil, and timers.
    fn cmd_proc(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            MakeProcess {
                id,
                program,
                args,
                on_output,
                on_exit,
            } => {
                self.make_process(id, program, args, on_output, on_exit, cx);
            }
            SendProcess { id, text } => self.send_to_process(id, &text, cx),
            KillProcess { id } => self.kill_process(id),
            HttpStream(args) => self.http_stream(*args, cx),
            HttpStreamCancel { id } => self.http_stream_cancel(id),
            HttpRequest(args) => self.http_request(*args, cx),
            HttpServe(args) => self.http_serve(*args, cx),
            HttpServerStop { port } => self.http_server_stop(port),
            AnvilEval { source, on_result } => self.anvil.eval(&source, &on_result),
            AnvilInterrupt => self.anvil.interrupt(),
            AnvilReset => self.anvil.reset(),
            AnvilEvalDefun => {
                let text = self.active.buffer.full_text();
                match top_level_form_at(&text, self.point()) {
                    Some(r) => self.anvil.eval(text[r].trim(), "anvil-on-result-echo"),
                    None => self.echo("anvil: no top-level form at point", cx),
                }
            }
            AnvilLoadFile => match &self.active.path {
                Some(path) => {
                    // `{:?}` renders a quoted, escaped Scheme string literal.
                    let form = format!("(load {:?})", path.to_string_lossy());
                    self.anvil.eval(&form, "anvil-on-result-echo");
                }
                None => self.echo("anvil: buffer is not visiting a file", cx),
            },
            AnvilDoc => {
                let text = self.active.buffer.full_text();
                match symbol_at(&text, self.point()) {
                    Some(sym) => self
                        .anvil
                        .query(crate::anvil::AnvilQuery::Doc(sym), "anvil-on-describe"),
                    None => self.echo("anvil: no symbol at point", cx),
                }
            }
            AnvilMacroexpand => {
                let text = self.active.buffer.full_text();
                let form = match self.region() {
                    Some(r) => text.get(r).map(str::to_string),
                    None => top_level_form_at(&text, self.point()).map(|r| text[r].to_string()),
                };
                match form.filter(|f| !f.trim().is_empty()) {
                    Some(form) => self.anvil.query(
                        crate::anvil::AnvilQuery::Macroexpand(form),
                        "anvil-on-describe",
                    ),
                    None => self.echo("anvil: no form to expand", cx),
                }
            }
            AnvilApropos(pattern) => self.anvil.query(
                crate::anvil::AnvilQuery::Apropos(pattern),
                "anvil-on-describe",
            ),
            AnvilSignature => {
                let text = self.active.buffer.full_text();
                match symbol_at(&text, self.point()) {
                    Some(sym) => self
                        .anvil
                        .query(crate::anvil::AnvilQuery::Signature(sym), "anvil-on-eldoc"),
                    // No symbol under point: clear any stale eldoc. The auto
                    // refresh (post-command-hook) calls this on every cursor-line
                    // move, so silence — not "no symbol" — is the right response.
                    None => self.echo("", cx),
                }
            }
            MakeTerminal {
                id,
                name,
                program,
                args,
            } => {
                self.make_terminal(id, name, program, args, cx);
            }
            RunWithTimer { ms, callback } => self.run_with_timer(ms, callback, cx),
            other => return Err(other),
        }
        Ok(())
    }

    /// Snippets, multiple cursors, avy, modeline, and macros.
    fn cmd_extras(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            SnippetExpand(template) => self.snippet_expand(template, cx),
            SnippetNextField => self.snippet_step_field(1, cx),
            SnippetPrevField => self.snippet_step_field(-1, cx),
            AvyShowHints(records) => self.avy_show_hints(&records, cx),
            AvyClearHints => self.avy_clear_hints(cx),
            McAddBelow => self.mc_add_line(1, cx),
            McAddAbove => self.mc_add_line(-1, cx),
            McMarkNext => self.mc_mark_next(cx),
            McClear => self.mc_clear(cx),
            SetSelections(sels) => self.set_selections(sels, cx),
            // Back-compat sugar over the generic slot mechanism.
            SetModelineProject(name) => {
                self.set_mode_line_slot("project".into(), name);
                cx.notify();
            }
            SetModelineGit(branch) => {
                self.set_mode_line_slot("git".into(), branch);
                cx.notify();
            }
            SetModeLineSlot(key, value) => {
                self.set_mode_line_slot(key, value);
                cx.notify();
            }
            SetModeLineFormat(tokens) => {
                self.mode_line_format = parse_mode_line_format(tokens);
                cx.notify();
            }
            SetTitlebarDropdown {
                items,
                active,
                callback,
            } => {
                // Re-publishing while the menu is unfolded (a find-file-hook
                // firing mid-browse) keeps it unfolded — filter, caret, and
                // selection included — rather than snapping shut.
                let (open, filter, cursor, selected) = self
                    .titlebar_dropdown
                    .as_ref()
                    .map(|d| (d.open, d.filter.clone(), d.cursor, d.selected))
                    .unwrap_or_default();
                self.titlebar_dropdown = if items.is_empty() {
                    None
                } else {
                    Some(TitlebarDropdownState {
                        items: items.into_iter().map(SharedString::from).collect(),
                        active: active.into(),
                        callback,
                        open,
                        filter,
                        cursor,
                        selected,
                    })
                };
                cx.notify();
            }
            StartKbdMacro => self.start_kbd_macro(cx),
            EndKbdMacro => self.end_kbd_macro(cx),
            CallLastKbdMacro => self.call_last_kbd_macro(cx),
            other => return Err(other),
        }
        Ok(())
    }

    /// Scripting, settings, hooks, and prompts.
    fn cmd_misc(
        &mut self,
        command: EditorCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), EditorCommand> {
        use EditorCommand::*;
        match command {
            KeyboardQuit => self.keyboard_quit(cx),
            ExecuteExtendedCommand => {
                self.open_minibuffer("M-x ", MinibufferPurpose::ExecuteCommand, "", cx);
            }
            ListMessages => self.list_messages(cx),
            ListLspLog => self.list_lsp_log(cx),
            CheckForUpdates => self.check_for_updates(cx),
            InstallGrammar(args) => {
                // Empty strings are the Scheme wrapper's "use the default" sentinel.
                let non_empty = |s: String| (!s.is_empty()).then_some(s);
                self.install_grammar(
                    crate::grammar::InstallSpec {
                        id: args.id,
                        repo: args.repo,
                        rev: args.rev,
                        extensions: args
                            .extensions
                            .split_whitespace()
                            .map(str::to_string)
                            .collect(),
                        symbol: non_empty(args.symbol),
                        subdir: non_empty(args.subdir),
                        highlights_path: None,
                    },
                    cx,
                );
            }
            EnterMacrosSteel => {
                // Drop diagnostics the previous server (e.g. plain `scheme`, which
                // doesn't know the editor API) already published for this buffer, so
                // its "unknown function" warnings clear immediately; the now-active
                // `macros-scheme` server republishes via the mode hook below.
                if let Some(uri) = self.active_uri() {
                    self.diagnostics.remove(&uri);
                }
                let mut modes = ModeStack::default();
                modes.push(Mode::new("fundamental"));
                modes.push(Mode::new("macros-steel").with_language("macros-scheme"));
                self.active.modes = modes;
                // `macros-scheme` has no grammar of its own; it shares Scheme's,
                // exactly as `make_document` wires it for auto-detected files.
                self.active.highlighter = Highlighter::scheme();
                self.parse_full();
                // Re-run mode hooks so the `fundamental` `enable-lsp` hook brings
                // up the `macros-scheme` server for the now-active language
                // (idempotent: `maybe_start_lsp` skips an already-running server).
                self.active.hooks_ran = false;
                self.run_mode_hooks_once(cx);
                self.echo("Entered macros-steel mode".to_string(), cx);
                cx.notify();
            }
            SetOption(key, value) => {
                // Toggling auto-revert changes which buffer files we watch.
                let touched_auto_revert = key == "auto-revert";
                cx.update_global::<crate::settings::Settings, _>(|settings, _| {
                    settings.set(key, value);
                });
                self.script
                    .set_options(cx.global::<crate::settings::Settings>().snapshot());
                if touched_auto_revert {
                    self.reconcile_watches(cx);
                }
                cx.notify();
            }
            AddHook(mode, function) => {
                let list = self.hooks.entry(mode).or_default();
                if !list.contains(&function) {
                    list.push(function);
                }
            }
            RemoveHook(mode, function) => {
                if let Some(list) = self.hooks.get_mut(&mode) {
                    list.retain(|f| f != &function);
                }
            }
            EvalExpression => {
                self.open_minibuffer("Eval: ", MinibufferPurpose::EvalExpression, "", cx);
            }
            EvalBuffer => {
                let source = self.active.buffer.full_text();
                self.eval_interactive(&source, cx);
            }
            EvalLastSexp => {
                // With an active selection, evaluate the whole region (all forms,
                // top-to-bottom) like Emacs `eval-region`; otherwise fall back to
                // the single sexp before point (`eval-last-sexp`).
                if let Some(region) = self.region() {
                    let source = self.active.buffer.slice_to_string(region);
                    self.eval_interactive(&source, cx);
                } else {
                    let point = self.point();
                    let text = self.active.buffer.slice_to_string(0..point);
                    match last_sexp(&text) {
                        Some(range) => {
                            let source = text[range].to_string();
                            self.eval_interactive(&source, cx);
                        }
                        None => self.echo("Nothing to evaluate", cx),
                    }
                }
            }
            Message(text) => self.echo(text, cx),
            DefineKey(mode, sequence, command) => {
                self.keymap.define(mode, sequence, format!("({command})"));
            }
            SetFace(face, color) => match crate::theme::parse_color(&color) {
                Some(c) => {
                    cx.update_global::<Theme, ()>(|theme, _| theme.set(face, c));
                    cx.notify();
                }
                None => self.echo(format!("Invalid color: {color}"), cx),
            },
            SetFaceUnderline(face, on) => {
                cx.update_global::<Theme, ()>(|theme, _| theme.set_underline(face, on));
                cx.notify();
            }
            SetFaceBold(face, on) => {
                cx.update_global::<Theme, ()>(|theme, _| theme.set_bold(face, on));
                cx.notify();
            }
            LoadTheme(name) => match crate::runtime::theme_script(&name) {
                Some(source) => {
                    // Reset to an empty palette, then apply the theme's set-face
                    // calls so switching themes never leaves stale faces.
                    cx.update_global::<Theme, ()>(|theme, _| *theme = Theme::default());
                    self.run_script(source, cx);
                }
                None => self.echo(format!("No theme named {name}"), cx),
            },
            ReadString {
                prompt,
                initial,
                callback,
            } => {
                self.open_minibuffer(
                    &prompt,
                    MinibufferPurpose::SchemeCallback(callback),
                    &initial,
                    cx,
                );
            }
            CompletingRead {
                prompt,
                candidates,
                dynamic,
                initial,
                callback,
            } => {
                self.completing_candidates = candidates;
                match dynamic {
                    Some(function) => {
                        self.open_minibuffer(
                            &prompt,
                            MinibufferPurpose::SchemeCompletingDynamic(callback),
                            &initial,
                            cx,
                        );
                        // Seed the candidate list for the initial input.
                        self.helm_query_counter += 1;
                        let id = self.helm_query_counter;
                        self.mb_dynamic = Some((function.clone(), id));
                        self.script
                            .dispatch_candidates(usize::MAX, id, &function, &initial);
                    }
                    None => self.open_minibuffer(
                        &prompt,
                        MinibufferPurpose::SchemeCompleting(callback),
                        &initial,
                        cx,
                    ),
                }
            }
            YesOrNo {
                prompt,
                callback,
                sticky,
            } => {
                let prompt = format!("{prompt} (yes or no) ");
                self.open_minibuffer(&prompt, MinibufferPurpose::SchemeYesNo(callback), "", cx);
                if sticky && let Some(mb) = self.minibuffer.as_mut() {
                    mb.sticky = true;
                }
            }
            other => return Err(other),
        }
        Ok(())
    }
}
