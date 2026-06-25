//! Helm (generic selection framework) methods for [`Editor`].

use super::*;

/// A Helm preview computed in Rust (rather than dispatched to a Scheme fn), for
/// the LSP location / code-action pickers.
enum NativePreview {
    Location(crate::lsp::Location),
    CodeAction(crate::lsp::CodeAction),
}

impl Editor {
    pub(super) fn helm_open(
        &mut self,
        prompt: String,
        sources: Vec<HelmSource>,
        initial: String,
        cx: &mut Context<Self>,
    ) {
        self.completion_popup = None;
        self.helm_query_counter += 1;
        let query_id = self.helm_query_counter;
        let fuzzy = cx.global::<crate::settings::Settings>().helm_fuzzy();
        self.helm = Some(HelmSession {
            prompt,
            pattern: initial,
            sources,
            selected: 0,
            query_id,
            fuzzy,
            marked: Vec::new(),
            preview: String::new(),
            preview_query: 0,
        });
        self.helm_query_dynamic();
        self.helm_update_preview();
        cx.notify();
    }

    /// Re-request the preview for the current selection, if its source defines a
    /// preview fn. The fn receives the candidate and returns the preview as a
    /// list of lines. Clears the pane when the source has no preview.
    pub(super) fn helm_update_preview(&mut self) {
        let matches = self.helm_matches();
        // Native (Rust-computed) preview for the LSP location / code-action
        // pickers. Clone the target out so the `self.helm` borrow is released
        // before reading files.
        let Some(helm) = self.helm.as_ref() else {
            return;
        };
        let native = match matches.get(helm.selected) {
            Some((src, candidate)) => {
                let source = &helm.sources[*src];
                let idx = source.candidates.iter().position(|c| c == candidate);
                match (&source.action, idx) {
                    (HelmAction::GotoLocation(locs), Some(i)) => {
                        locs.get(i).cloned().map(NativePreview::Location)
                    }
                    (HelmAction::ApplyCodeAction(acts), Some(i)) => {
                        acts.get(i).cloned().map(NativePreview::CodeAction)
                    }
                    _ => None,
                }
            }
            None => None,
        };
        if let Some(native) = native {
            let text = match native {
                NativePreview::Location(loc) => self.location_preview(&loc),
                NativePreview::CodeAction(action) => Self::code_action_preview(&action),
            };
            if let Some(helm) = self.helm.as_mut() {
                helm.preview = text;
            }
            return;
        }
        let Some(helm) = self.helm.as_ref() else {
            return;
        };
        let preview_fn = matches
            .get(helm.selected)
            .and_then(|(src, _)| helm.sources.get(*src))
            .and_then(|src| src.preview.clone());
        let candidate = matches.get(helm.selected).map(|(_, c)| c.clone());
        match (preview_fn, candidate) {
            (Some(function), Some(candidate)) => {
                self.helm_query_counter += 1;
                let id = self.helm_query_counter;
                if let Some(helm) = self.helm.as_mut() {
                    helm.preview_query = id;
                }
                // `usize::MAX - 1` tags preview results (distinct from dynamic
                // candidates' `usize::MAX` and real Helm source indices).
                self.script
                    .dispatch_candidates(usize::MAX - 1, id, &function, &candidate);
            }
            _ => {
                if let Some(helm) = self.helm.as_mut() {
                    helm.preview.clear();
                }
            }
        }
    }

    /// Install a preview result if it's still current.
    pub(super) fn helm_set_preview(
        &mut self,
        query_id: u64,
        lines: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(helm) = self.helm.as_mut()
            && helm.preview_query == query_id
        {
            helm.preview = lines.join("\n");
            cx.notify();
        }
    }

    /// Built-in `helm-M-x`: every command name, running the chosen one.
    pub(super) fn helm_mx(&mut self, cx: &mut Context<Self>) {
        let source = HelmSource {
            name: "Commands".to_string(),
            candidates: self.script.command_names().to_vec(),
            annotations: HashMap::new(),
            dynamic: None,
            raw: false,
            action: HelmAction::ExecuteCommand,
            preview: None,
        };
        self.helm_open("M-x ".to_string(), vec![source], String::new(), cx);
    }

    /// The flattened `(source_index, candidate)` list matching the live pattern
    /// (case-insensitive substring; everything matches an empty pattern). Dynamic
    /// sources are also substring-filtered — their fn supplies the pool.
    pub(super) fn helm_matches(&self) -> Vec<(usize, String)> {
        match self.helm.as_ref() {
            Some(helm) => helm_filter(&helm.sources, &helm.pattern, helm.fuzzy),
            None => Vec::new(),
        }
    }

    /// (Re)compute every dynamic source's candidates for the current pattern, on
    /// the engine thread. Results return via `helm_set_candidates`.
    pub(super) fn helm_query_dynamic(&self) {
        let Some(helm) = self.helm.as_ref() else {
            return;
        };
        for (i, source) in helm.sources.iter().enumerate() {
            if let Some(function) = &source.dynamic {
                self.script
                    .dispatch_candidates(i, helm.query_id, function, &helm.pattern);
            }
        }
    }

    /// On a pattern change, bump the query id and re-query dynamic sources.
    pub(super) fn helm_requery(&mut self) {
        let has_dynamic = self
            .helm
            .as_ref()
            .is_some_and(|h| h.sources.iter().any(|s| s.dynamic.is_some()));
        if has_dynamic {
            self.helm_query_counter += 1;
            let id = self.helm_query_counter;
            if let Some(helm) = self.helm.as_mut() {
                helm.query_id = id;
            }
            self.helm_query_dynamic();
        }
    }

    /// Install fresh dynamic candidates for `source` if they're still current.
    pub(super) fn helm_set_candidates(
        &mut self,
        source: usize,
        query_id: u64,
        candidates: Vec<(String, Option<String>)>,
        cx: &mut Context<Self>,
    ) {
        let current = {
            let Some(helm) = self.helm.as_mut() else {
                return;
            };
            if helm.query_id != query_id {
                return; // stale
            }
            if let Some(src) = helm.sources.get_mut(source) {
                src.annotations.clear();
                src.candidates = candidates
                    .into_iter()
                    .map(|(c, ann)| {
                        if let Some(ann) = ann {
                            src.annotations.insert(c.clone(), ann);
                        }
                        c
                    })
                    .collect();
            }
            true
        };
        if current {
            let count = self.helm_matches().len();
            if let Some(helm) = self.helm.as_mut()
                && helm.selected >= count
            {
                helm.selected = count.saturating_sub(1);
            }
            self.helm_update_preview();
            cx.notify();
        }
    }

    pub(super) fn helm_input(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(helm) = self.helm.as_mut() {
            helm.pattern.push_str(text);
            if let Some(pattern) = helm_file_shadow_pattern(&helm.sources, &helm.pattern) {
                helm.pattern = pattern;
            }
            helm.selected = 0;
        }
        self.helm_requery();
        self.helm_update_preview();
        cx.notify();
    }

    pub(super) fn helm_backspace(&mut self, cx: &mut Context<Self>) {
        if let Some(helm) = self.helm.as_mut() {
            helm.pattern.pop();
            helm.selected = 0;
        }
        self.helm_requery();
        self.helm_update_preview();
        cx.notify();
    }

    pub(super) fn helm_next(&mut self, cx: &mut Context<Self>) {
        self.helm_cycle(true, cx);
    }

    pub(super) fn helm_previous(&mut self, cx: &mut Context<Self>) {
        self.helm_cycle(false, cx);
    }

    fn helm_cycle(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.helm_matches().len();
        if let Some(helm) = self.helm.as_mut() {
            helm.selected = wrap_index(helm.selected, count, forward);
        }
        self.helm_update_preview();
        cx.notify();
    }

    /// The `(source_index, candidate)` currently under the Helm cursor, if any.
    fn helm_selected(&self) -> Option<(usize, String)> {
        let helm = self.helm.as_ref()?;
        self.helm_matches().get(helm.selected).cloned()
    }

    /// A mouse click on candidate row `index`: select it and run its default
    /// action (a directory descends, mirroring RET).
    pub fn helm_click(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.helm_matches().len();
        if index >= count {
            return;
        }
        if let Some(helm) = self.helm.as_mut() {
            helm.selected = index;
        }
        self.helm_accept(cx);
    }

    pub(super) fn helm_abort(&mut self, cx: &mut Context<Self>) {
        self.helm = None;
        cx.notify();
    }

    /// `C-SPC`: toggle the selected candidate's mark, then advance — so repeated
    /// presses mark a run. Accepting then runs the action for every marked item.
    pub(super) fn helm_toggle_mark(&mut self, cx: &mut Context<Self>) {
        let matches = self.helm_matches();
        let Some(helm) = self.helm.as_ref() else {
            return;
        };
        let Some((_, candidate)) = matches.get(helm.selected).cloned() else {
            return;
        };
        if candidate.ends_with('/') {
            return; // directories descend, not mark
        }
        let count = matches.len();
        if let Some(helm) = self.helm.as_mut() {
            if let Some(pos) = helm.marked.iter().position(|m| m == &candidate) {
                helm.marked.remove(pos);
            } else {
                helm.marked.push(candidate);
            }
            helm.selected = wrap_index(helm.selected, count, true);
        }
        cx.notify();
    }

    /// `TAB`: complete the live input to the selected candidate. A directory
    /// candidate ends with `/`, so a dynamic file source then lists its contents
    /// — i.e. TAB descends into the matched folder.
    pub(super) fn helm_complete(&mut self, cx: &mut Context<Self>) {
        let Some((_, candidate)) = self.helm_selected() else {
            return;
        };
        if let Some(helm) = self.helm.as_mut() {
            if helm.pattern == candidate {
                return; // nothing to expand
            }
            helm.pattern = candidate;
            helm.selected = 0;
        }
        self.helm_requery(); // dynamic sources relist for the new path
        cx.notify();
    }

    /// Run the selected candidate's default action, then close the session.
    ///
    /// RET acts on the highlighted candidate (the top match), mirroring Emacs
    /// Helm — so a partial pattern still opens the selected file. Only when the
    /// match list is empty does a file picker fall back to visiting the typed
    /// path (find-file's create-a-new-file behaviour).
    pub(super) fn helm_accept(&mut self, cx: &mut Context<Self>) {
        let Some((source_index, candidate)) = self.helm_selected() else {
            // Nothing matches the pattern: for a file picker, visit the typed
            // path so RET can create a new (not-yet-existing) file.
            if let Some((action, target)) = self
                .helm
                .as_ref()
                .and_then(|helm| helm_file_accept_target(&helm.sources, &helm.pattern))
            {
                self.helm = None;
                match action {
                    HelmAction::Scheme(actions) => {
                        if let Some((_, function)) = actions.first() {
                            self.script.dispatch_action(function, Some(&target));
                        }
                    }
                    HelmAction::ExecuteCommand => self.script.dispatch_action(&target, None),
                    _ => {}
                }
                cx.notify();
            }
            return;
        };
        // A directory (trailing slash): RET descends into it rather than acting.
        if candidate.ends_with('/') {
            self.helm_complete(cx);
            return;
        }
        let Some(helm) = self.helm.as_ref() else {
            return;
        };
        let action = helm.sources[source_index].action.clone();
        // The accepted candidate's index within the (unfiltered) source — native
        // actions use it to pick from their parallel target vec, since Helm
        // filters and reorders the visible matches.
        let candidate_index = helm.sources[source_index]
            .candidates
            .iter()
            .position(|c| c == &candidate);
        // Multi-select: act on every marked candidate (in mark order). With none
        // marked, fall back to the single candidate under the cursor.
        let targets = if helm.marked.is_empty() {
            vec![candidate.clone()]
        } else {
            helm.marked.clone()
        };
        self.helm = None;
        // Actions are Steel functions, dispatched by name to the engine thread;
        // their commands arrive via `apply_script_outcome`.
        match action {
            HelmAction::Scheme(actions) => {
                if let Some((_, function)) = actions.first() {
                    for target in &targets {
                        self.script.dispatch_action(function, Some(target));
                    }
                }
            }
            HelmAction::ExecuteCommand => self.script.dispatch_action(&candidate, None),
            HelmAction::ActionMenu => {
                if let Some((original, actions)) = self.helm_action_context.take()
                    && let Some((_, function)) =
                        actions.iter().find(|(label, _)| label == &candidate)
                {
                    self.script.dispatch_action(function, Some(&original));
                }
            }
            HelmAction::GotoLocation(locations) => {
                if let Some(loc) = candidate_index.and_then(|i| locations.get(i)).cloned() {
                    self.goto_file_position(loc.uri, loc.start.0, loc.start.1, cx);
                }
            }
            HelmAction::ApplyCodeAction(actions) => {
                if let Some(action) = candidate_index.and_then(|i| actions.get(i)).cloned() {
                    self.apply_code_action(action, cx);
                }
            }
        }
        cx.notify();
    }

    /// `TAB`: with multiple actions, open an action-menu sub-Helm; otherwise run
    /// the default action.
    pub(super) fn helm_action_menu(&mut self, cx: &mut Context<Self>) {
        let Some((source_index, candidate)) = self.helm_selected() else {
            return;
        };
        let actions = self
            .helm
            .as_ref()
            .and_then(|h| match &h.sources[source_index].action {
                HelmAction::Scheme(a) if a.len() > 1 => Some(a.clone()),
                _ => None,
            });
        let Some(actions) = actions else {
            self.helm_accept(cx); // single action / M-x: just run it
            return;
        };
        let labels: Vec<String> = actions.iter().map(|(label, _)| label.clone()).collect();
        self.helm_action_context = Some((candidate, actions));
        self.helm_open(
            "Action: ".to_string(),
            vec![HelmSource {
                name: "Actions".to_string(),
                candidates: labels,
                annotations: HashMap::new(),
                dynamic: None,
                raw: false,
                action: HelmAction::ActionMenu,
                preview: None,
            }],
            String::new(),
            cx,
        );
    }

    /// A render snapshot of the Helm panel (windowed around the selection, with
    /// a header whenever the source changes).
    pub fn helm_view(&self) -> Option<HelmView> {
        let helm = self.helm.as_ref()?;
        let matches = self.helm_matches();
        const MAX: usize = 12;
        let total = matches.len();
        let start = if helm.selected >= MAX {
            helm.selected + 1 - MAX
        } else {
            0
        };
        let end = (start + MAX).min(total);

        let mut rows = Vec::new();
        let mut last_source = None;
        for (i, (source_index, candidate)) in matches.iter().enumerate().take(end).skip(start) {
            if last_source != Some(*source_index) {
                rows.push(HelmRow {
                    text: helm.sources[*source_index].name.clone(),
                    annotation: None,
                    header: true,
                    selected: false,
                    marked: false,
                    index: None,
                });
                last_source = Some(*source_index);
            }
            rows.push(HelmRow {
                text: candidate.clone(),
                annotation: helm.sources[*source_index]
                    .annotations
                    .get(candidate)
                    .cloned(),
                header: false,
                selected: i == helm.selected,
                marked: helm.marked.iter().any(|m| m == candidate),
                index: Some(i),
            });
        }
        Some(HelmView {
            prompt: helm.prompt.clone(),
            pattern: helm.pattern.clone(),
            rows,
            preview: (!helm.preview.is_empty()).then(|| helm.preview.clone()),
        })
    }
}
