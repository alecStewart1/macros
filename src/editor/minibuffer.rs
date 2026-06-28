//! Minibuffer prompts, completion, and history for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::path::Path;

use gpui::Context;

use super::*;

impl Editor {
    /// A render snapshot of the active minibuffer, split at the cursor.
    pub fn minibuffer_view(&self) -> Option<MinibufferView> {
        self.minibuffer.as_ref().map(|mb| {
            let (before, after) = mb.input.split_at(mb.cursor);
            MinibufferView {
                prompt: mb.prompt.clone(),
                before: before.to_string(),
                after: after.to_string(),
                message: mb.message.clone(),
            }
        })
    }

    pub(super) fn open_minibuffer(
        &mut self,
        prompt: &str,
        purpose: MinibufferPurpose,
        initial: &str,
        cx: &mut Context<Self>,
    ) {
        self.minibuffer = Some(Minibuffer {
            prompt: prompt.to_string(),
            input: initial.to_string(),
            cursor: initial.len(),
            message: String::new(),
            purpose,
        });
        self.minibuffer_history_pos = None;
        self.minibuffer_stash = String::new();
        self.mb_dynamic = None; // set afterward by a dynamic completing-read
        cx.notify();
    }

    /// For a dynamic `completing-read`, re-dispatch the candidate fn with the
    /// current input on the engine thread (results arrive via `helm_set_candidates`'
    /// sibling routing in the `Candidates` outcome). No-op otherwise.
    pub(super) fn mb_dynamic_requery(&mut self) {
        let Some((function, _)) = self.mb_dynamic.clone() else {
            return;
        };
        let Some(input) = self.minibuffer.as_ref().map(|mb| mb.input.clone()) else {
            return;
        };
        self.helm_query_counter += 1;
        let id = self.helm_query_counter;
        self.mb_dynamic = Some((function.clone(), id));
        // Sentinel source id `usize::MAX` distinguishes minibuffer queries from
        // Helm's (which use real source indices) in the `Candidates` outcome.
        self.script
            .dispatch_candidates(usize::MAX, id, &function, &input);
    }

    /// Install dynamic `completing-read` candidates from the engine, if they're
    /// still current (the query id matches the latest dispatch).
    pub(super) fn mb_set_dynamic_candidates(
        &mut self,
        query_id: u64,
        candidates: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.mb_dynamic.as_ref().map(|(_, id)| *id) == Some(query_id) {
            self.completing_candidates = candidates;
            cx.notify();
        }
    }

    /// Record a submitted minibuffer input in the shared history (newest last,
    /// skipping blanks and consecutive duplicates).
    pub(super) fn push_minibuffer_history(&mut self, input: &str) {
        if input.is_empty() || self.minibuffer_history.last().map(String::as_str) == Some(input) {
            return;
        }
        self.minibuffer_history.push(input.to_string());
    }

    /// Browse minibuffer history: `delta < 0` recalls older entries, `delta > 0`
    /// newer ones; stepping past the newest restores the in-progress input.
    pub(super) fn mb_history(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.minibuffer.is_none() || self.minibuffer_history.is_empty() {
            return;
        }
        let len = self.minibuffer_history.len();
        let pos = self.minibuffer_history_pos;
        let next = if delta < 0 {
            // Older. Entering history stashes the current (fresh) input.
            match pos {
                None => {
                    if let Some(mb) = self.minibuffer.as_ref() {
                        self.minibuffer_stash = mb.input.clone();
                    }
                    Some(0)
                }
                Some(i) => Some((i + 1).min(len - 1)),
            }
        } else {
            // Newer.
            match pos {
                None => return,
                Some(0) => None,
                Some(i) => Some(i - 1),
            }
        };
        let text = match next {
            Some(i) => self.minibuffer_history[len - 1 - i].clone(),
            None => std::mem::take(&mut self.minibuffer_stash),
        };
        self.minibuffer_history_pos = next;
        if let Some(mb) = self.minibuffer.as_mut() {
            mb.cursor = text.len();
            mb.input = text;
        }
        cx.notify();
    }

    // --- Buffers / files --------------------------------------------------

    /// The active buffer's directory as a minibuffer-ready prefix (trailing `/`),
    /// for file-name prompts. Falls back to the process cwd.
    pub(super) fn default_directory_input(&self) -> String {
        let dir = self
            .active_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let mut initial = dir.to_string_lossy().into_owned();
        if !initial.ends_with('/') {
            initial.push('/');
        }
        initial
    }

    pub(super) fn find_file_prompt(&mut self, cx: &mut Context<Self>) {
        let initial = self.default_directory_input();
        self.open_minibuffer("Find file: ", MinibufferPurpose::FindFile, &initial, cx);
    }

    pub(super) fn mb_insert(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_mut() {
            mb.input.insert_str(mb.cursor, text);
            mb.cursor += text.len();
            mb.message.clear();
            cx.notify();
        }
        if self.is_isearch() {
            self.isearch_update(cx);
        }
        self.mb_dynamic_requery();
    }

    pub(super) fn mb_backspace(&mut self, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_mut()
            && mb.cursor > 0
        {
            let prev = prev_grapheme(&mb.input, mb.cursor);
            mb.input.replace_range(prev..mb.cursor, "");
            mb.cursor = prev;
            mb.message.clear();
            cx.notify();
        }
        if self.is_isearch() {
            // Shorten the string and re-match from the original start so deleting
            // a char backs up to the earlier (shorter) match.
            self.isearch_from = self.isearch_origin;
            self.isearch_update(cx);
        }
        self.mb_dynamic_requery();
    }

    /// Whether an incremental-search minibuffer is currently open.
    pub(super) fn is_isearch(&self) -> bool {
        matches!(
            self.minibuffer.as_ref().map(|mb| &mb.purpose),
            Some(MinibufferPurpose::Isearch)
        )
    }

    pub(super) fn mb_delete(&mut self, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_mut() {
            let next = next_grapheme(&mb.input, mb.cursor);
            if next > mb.cursor {
                mb.input.replace_range(mb.cursor..next, "");
                mb.message.clear();
                cx.notify();
            }
        }
        self.mb_dynamic_requery();
    }

    pub(super) fn mb_move(&mut self, to: usize, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_mut() {
            mb.cursor = to.min(mb.input.len());
            cx.notify();
        }
    }

    pub(super) fn mb_left(&mut self, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_ref() {
            let to = prev_grapheme(&mb.input, mb.cursor);
            self.mb_move(to, cx);
        }
    }

    pub(super) fn mb_right(&mut self, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_ref() {
            let to = next_grapheme(&mb.input, mb.cursor);
            self.mb_move(to, cx);
        }
    }

    pub(super) fn mb_abort(&mut self, cx: &mut Context<Self>) {
        // Aborting an incremental search returns point to where it began and
        // drops the match highlight (Emacs C-g out of isearch).
        let isearch = self.is_isearch();
        self.minibuffer = None;
        if isearch {
            self.search_query = None;
            self.isearch_match = None;
            self.move_to(self.isearch_origin, cx);
        }
        self.echo("Quit", cx);
    }

    /// Completion candidates for the active minibuffer's purpose and input.
    pub(super) fn mb_candidates(&self, input: &str) -> Vec<String> {
        match self.minibuffer.as_ref().map(|mb| mb.purpose.clone()) {
            Some(MinibufferPurpose::ExecuteCommand) => self
                .script
                .command_names()
                .iter()
                .filter(|c| c.starts_with(input))
                .cloned()
                .collect(),
            Some(MinibufferPurpose::SwitchBuffer) => self
                .buffer_names()
                .into_iter()
                .filter(|n| n.starts_with(input))
                .collect(),
            Some(MinibufferPurpose::FindFile) => file_completions(input),
            Some(MinibufferPurpose::Confirm(_)) | Some(MinibufferPurpose::QuitSave) => {
                ["yes", "no"]
                    .iter()
                    .map(|s| s.to_string())
                    .filter(|c| c.starts_with(input))
                    .collect()
            }
            Some(MinibufferPurpose::CompleteAtPoint) => self
                .completion_items
                .iter()
                .filter(|c| c.starts_with(input))
                .cloned()
                .collect(),
            Some(MinibufferPurpose::SchemeCompleting(_)) => self
                .completing_candidates
                .iter()
                .filter(|c| c.starts_with(input))
                .cloned()
                .collect(),
            // Dynamic: the Scheme fn already filtered for the pattern — show as-is.
            Some(MinibufferPurpose::SchemeCompletingDynamic(_)) => {
                self.completing_candidates.clone()
            }
            Some(MinibufferPurpose::SchemeReadFile(_)) => file_completions(input),
            Some(MinibufferPurpose::SchemeYesNo(_)) => ["yes", "no"]
                .iter()
                .map(|s| s.to_string())
                .filter(|c| c.starts_with(input))
                .collect(),
            // Free-form reads (regex search, ex command, read-string) have no
            // completions.
            Some(
                MinibufferPurpose::SearchForward
                | MinibufferPurpose::SearchBackward
                | MinibufferPurpose::Isearch
                | MinibufferPurpose::ExCommand
                | MinibufferPurpose::EvalExpression
                | MinibufferPurpose::SchemeCallback(_),
            ) => Vec::new(),
            None => Vec::new(),
        }
    }

    /// `TAB`: complete the input against the candidate set for this read.
    pub(super) fn mb_complete(&mut self, cx: &mut Context<Self>) {
        let Some((input, purpose)) = self
            .minibuffer
            .as_ref()
            .map(|mb| (mb.input.clone(), mb.purpose.clone()))
        else {
            return;
        };
        // The eval read holds a whole expression, so TAB completes the trailing
        // Scheme symbol token (e.g. `(set-o` → `(set-option`) rather than the
        // whole string — a dedicated, splice-in-place path.
        if matches!(purpose, MinibufferPurpose::EvalExpression) {
            self.mb_complete_symbol(&input, cx);
            return;
        }
        let matches = self.mb_candidates(&input);

        let mb = self.minibuffer.as_mut().unwrap();
        if matches.is_empty() {
            mb.message = "[No match]".into();
        } else if matches.len() == 1 {
            mb.input = matches[0].clone();
            mb.cursor = mb.input.len();
            mb.message = "[Sole completion]".into();
        } else {
            let prefix = longest_common_prefix(&matches);
            if prefix.len() > mb.input.len() {
                mb.input = prefix;
                mb.cursor = mb.input.len();
            }
            // Show the candidates so the user can see what to disambiguate.
            mb.message = format_candidates(&matches);
        }
        cx.notify();
    }

    /// All Scheme symbols worth completing in the eval minibuffer: the
    /// Rust-registered builtins and interactive commands, plus every top-level
    /// define in the bundled Scheme library. Sorted and de-duplicated.
    pub(super) fn scheme_symbol_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.script.global_names().to_vec();
        names.extend(crate::runtime::defined_names());
        names.sort();
        names.dedup();
        names
    }

    /// `TAB` in the eval minibuffer: complete the trailing symbol token against
    /// [`Self::scheme_symbol_names`], splicing the result in place of just that
    /// token so the rest of the expression (open parens, earlier args) is kept.
    pub(super) fn mb_complete_symbol(&mut self, input: &str, cx: &mut Context<Self>) {
        let (start, token) = scheme_symbol_token(input);
        let candidates: Vec<String> = if token.is_empty() {
            Vec::new()
        } else {
            self.scheme_symbol_names()
                .into_iter()
                .filter(|s| s.starts_with(token))
                .collect()
        };
        let head = input[..start].to_string();
        let token_len = token.len();
        let mb = self.minibuffer.as_mut().unwrap();
        if candidates.is_empty() {
            mb.message = "[No match]".into();
        } else if candidates.len() == 1 {
            mb.input = format!("{head}{}", candidates[0]);
            mb.cursor = mb.input.len();
            mb.message = "[Sole completion]".into();
        } else {
            let lcp = longest_common_prefix(&candidates);
            if lcp.len() > token_len {
                mb.input = format!("{head}{lcp}");
                mb.cursor = mb.input.len();
            }
            // Show bare symbol names so the user can see what to disambiguate.
            mb.message = format_candidates(&candidates);
        }
        cx.notify();
    }

    /// `RET`: finish the read and act on the input.
    pub(super) fn mb_submit(&mut self, cx: &mut Context<Self>) {
        let Some(mb) = self.minibuffer.take() else {
            return;
        };
        cx.notify();
        let input = mb.input.trim().to_string();
        // Record text-entry reads in the shared history (not yes/no prompts).
        if !matches!(
            mb.purpose,
            MinibufferPurpose::Confirm(_)
                | MinibufferPurpose::QuitSave
                | MinibufferPurpose::SchemeYesNo(_)
        ) {
            self.push_minibuffer_history(&input);
        }
        match mb.purpose {
            MinibufferPurpose::ExecuteCommand => {
                if input.is_empty() {
                    self.echo("", cx);
                } else if self.script.command_names().iter().any(|c| c == &input) {
                    self.run_script(&format!("({input})"), cx);
                } else {
                    self.echo(format!("[No match: {input}]"), cx);
                }
            }
            MinibufferPurpose::FindFile => {
                if !input.is_empty() {
                    self.find_file(&input, cx);
                }
            }
            MinibufferPurpose::SwitchBuffer => {
                if !input.is_empty() {
                    self.switch_to_buffer(&input, cx);
                }
            }
            MinibufferPurpose::Confirm(action) => {
                if matches!(input.as_str(), "yes" | "y") {
                    self.perform(action, cx);
                } else {
                    self.echo("Cancelled", cx);
                }
            }
            MinibufferPurpose::QuitSave => match input.as_str() {
                "yes" | "y" => {
                    self.save_all_modified(cx);
                    cx.quit();
                }
                "no" | "n" => cx.quit(),
                _ => self.echo("Cancelled", cx),
            },
            MinibufferPurpose::CompleteAtPoint => {
                if let Some(range) = self.completion_range.take()
                    && !input.is_empty()
                {
                    let cursor = self.edit(range, &input);
                    self.collapse_to(cursor);
                    cx.notify();
                }
            }
            MinibufferPurpose::SearchForward => {
                if !input.is_empty() {
                    self.search_query = Some(input);
                }
                self.evil_search_forward = true;
                self.do_search(true, cx);
            }
            MinibufferPurpose::SearchBackward => {
                if !input.is_empty() {
                    self.search_query = Some(input);
                }
                self.evil_search_forward = false;
                self.do_search(false, cx);
            }
            MinibufferPurpose::Isearch => {
                // RET ends the search, leaving point at the current match. Remember
                // the string so a later empty C-s recalls it; keep `search_query`
                // set so the matches stay highlighted (Emacs lazy-highlight).
                if !input.is_empty() {
                    self.last_isearch = Some(input);
                } else if let Some(prev) = self.last_isearch.clone() {
                    // RET on an empty isearch with a prior string: adopt it.
                    self.search_query = Some(prev);
                }
                self.isearch_match = None;
            }
            MinibufferPurpose::ExCommand => self.run_ex_command(&input, cx),
            MinibufferPurpose::EvalExpression => {
                if !input.is_empty() {
                    self.eval_interactive(&input, cx); // echoes `=> value`
                }
            }
            MinibufferPurpose::SchemeCallback(callback)
            | MinibufferPurpose::SchemeCompleting(callback)
            | MinibufferPurpose::SchemeCompletingDynamic(callback) => {
                // Pass the entered text to the Scheme callback.
                self.publish_point();
                self.script.dispatch_action(&callback, Some(&input));
            }
            MinibufferPurpose::SchemeReadFile(callback) => {
                self.publish_point();
                self.script
                    .dispatch_action(&callback, Some(&expand_path(&input)));
            }
            MinibufferPurpose::SchemeYesNo(callback) => {
                let answer = if matches!(input.as_str(), "yes" | "y") {
                    "yes"
                } else {
                    "no"
                };
                self.publish_point();
                self.script.dispatch_action(&callback, Some(answer));
            }
        }
    }

    pub(super) fn open_confirm(
        &mut self,
        prompt: &str,
        action: PendingAction,
        cx: &mut Context<Self>,
    ) {
        let prompt = format!("{prompt} (yes or no) ");
        self.open_minibuffer(&prompt, MinibufferPurpose::Confirm(action), "", cx);
    }

    pub(super) fn perform(&mut self, action: PendingAction, cx: &mut Context<Self>) {
        match action {
            PendingAction::KillActiveBuffer => self.kill_active_buffer(cx),
            PendingAction::KillBufferAndWindow => {
                self.kill_active_buffer(cx);
                self.delete_window(cx);
            }
        }
    }

    // --- kill-buffer / quit ----------------------------------------------
}
