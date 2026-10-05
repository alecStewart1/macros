//! Helm (generic selection framework) methods for [`Editor`].

use super::*;

/// Convert parsed [`HelmSourceSpec`]s into runtime [`HelmSource`]s.
pub(super) fn helm_sources_from_specs(
    specs: Vec<crate::script::HelmSourceSpec>,
) -> Vec<HelmSource> {
    specs
        .into_iter()
        .map(|s| {
            let (candidates, annotations, dynamic, raw) = match s.candidates {
                crate::script::HelmCandidates::Static(pairs) => {
                    let mut annotations = HashMap::new();
                    let candidates = pairs
                        .into_iter()
                        .map(|(c, ann)| {
                            if let Some(ann) = ann {
                                annotations.insert(c.clone(), ann);
                            }
                            c
                        })
                        .collect();
                    (candidates, annotations, None, false)
                }
                crate::script::HelmCandidates::Dynamic(f) => {
                    (Vec::new(), HashMap::new(), Some(f), false)
                }
                crate::script::HelmCandidates::DynamicRaw(f) => {
                    (Vec::new(), HashMap::new(), Some(f), true)
                }
            };
            HelmSource {
                name: s.name,
                candidates,
                annotations,
                dynamic,
                raw,
                action: HelmAction::Scheme(s.actions),
                preview: s.preview,
            }
        })
        .collect()
}

/// Flatten Helm sources to `(source_index, candidate)` matching `pattern`:
/// nucleo fuzzy ranking (best-first within each source) when `fuzzy`, else
/// case-insensitive substring; an empty pattern keeps everything in order.
pub(super) fn helm_filter(
    sources: &[HelmSource],
    pattern: &str,
    fuzzy: bool,
) -> Vec<(usize, String)> {
    let empty = pattern.is_empty();
    // Fuzzy machinery, built once and only when a non-raw source will need it.
    let mut fuzzy_ctx = (!empty && fuzzy).then(|| {
        (
            Matcher::new(Config::DEFAULT),
            Pattern::parse(pattern, CaseMatching::Smart, Normalization::Smart),
        )
    });
    let needle = (!empty && !fuzzy).then(|| pattern.to_lowercase());
    let mut out = Vec::new();
    for (i, source) in sources.iter().enumerate() {
        // An empty pattern keeps everything; a `raw` source self-filtered in
        // Scheme, so it too is shown verbatim, in the order the fn returned.
        if empty || source.raw {
            out.extend(source.candidates.iter().map(|c| (i, c.clone())));
        } else if let Some((matcher, parsed)) = fuzzy_ctx.as_mut() {
            for (candidate, _score) in parsed.match_list(source.candidates.iter(), matcher) {
                out.push((i, candidate.clone()));
            }
        } else if let Some(needle) = &needle {
            for candidate in &source.candidates {
                if candidate.to_lowercase().contains(needle) {
                    out.push((i, candidate.clone()));
                }
            }
        }
    }
    out
}

/// Emacs-style file prompt shortcuts for Helm file sources. When a file picker
/// is pre-filled with the current directory, typing `/` after that trailing `/`
/// produces `//`; the second slash shadows everything before it and jumps to
/// the filesystem root.
pub(super) fn helm_file_shadow_pattern(sources: &[HelmSource], pattern: &str) -> Option<String> {
    let is_file_source = sources
        .iter()
        .any(|s| s.dynamic.as_deref() == Some("find-file-candidates"));
    if !is_file_source {
        return None;
    }
    let (_, tail) = pattern.rsplit_once("//")?;
    Some(format!("/{tail}"))
}

/// For Helm file pickers, `RET` on text that is not an exact listed candidate
/// should visit that typed path. This is how `find-file` creates a file-visiting
/// empty buffer for a new file; directories still use candidate completion.
pub(super) fn helm_file_accept_target(
    sources: &[HelmSource],
    pattern: &str,
) -> Option<(HelmAction, String)> {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.ends_with('/') {
        return None;
    }
    let source = sources
        .iter()
        .find(|s| s.dynamic.as_deref() == Some("find-file-candidates"))?;
    if source.candidates.iter().any(|c| c == pattern) {
        return None;
    }
    if Path::new(&expand_path(pattern)).is_dir() {
        return None;
    }
    Some((source.action.clone(), pattern.to_string()))
}

/// The index within its source's (unfiltered) `candidates` of match
/// `selected` in `matches` (as from [`helm_filter`]). Identical candidate
/// strings (two LSP locations rendering the same `file:line:col: text` row)
/// are told apart by occurrence: the k-th matching copy maps to the k-th copy
/// in the source — filtering keeps equal strings in source order (substring
/// filtering preserves it; equal strings score equally under a stable sort).
pub(super) fn helm_candidate_index(
    sources: &[HelmSource],
    matches: &[(usize, String)],
    selected: usize,
) -> Option<usize> {
    let (src, candidate) = matches.get(selected)?;
    let nth = matches[..selected]
        .iter()
        .filter(|(s, c)| s == src && c == candidate)
        .count();
    sources
        .get(*src)?
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| *c == candidate)
        .nth(nth)
        .map(|(i, _)| i)
}

/// Cycle a selection index by one step with wraparound over `count` items.
/// `forward` advances, otherwise it steps back. Returns 0 if `count` is 0.
pub(super) fn wrap_index(selected: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    let step = if forward { 1 } else { count - 1 };
    (selected + step) % count
}

/// What running a Helm candidate does.
#[derive(Clone)]
pub(super) enum HelmAction {
    /// `(label, function-name)` pairs — the first is the default (RET); the rest
    /// are reachable via the TAB action menu. The function is called with the
    /// chosen candidate string.
    Scheme(Vec<(String, String)>),
    /// Treat the candidate itself as a command name and run it (helm-M-x).
    ExecuteCommand,
    /// An action-menu session: the candidate is an action label, resolved
    /// against `helm_action_context`.
    ActionMenu,
    /// Jump to one of a set of LSP locations. The vec is parallel to the
    /// source's `candidates`; the accepted candidate's index selects the target.
    GotoLocation(Vec<lsp::Location>),
    /// Apply one of a set of LSP code actions. The vec is parallel to the
    /// source's `candidates`; the accepted candidate's index selects the action.
    ApplyCodeAction(Vec<lsp::CodeAction>),
}

/// One group of candidates in a Helm session.
pub(super) struct HelmSource {
    pub(super) name: String,
    /// Current candidates — for a dynamic source, the latest query result.
    pub(super) candidates: Vec<String>,
    /// Optional marginalia-style annotation per candidate (candidate → text),
    /// shown dimmed to the right of the row. Candidates absent from the map have
    /// no annotation.
    pub(super) annotations: HashMap<String, String>,
    /// Name of a Scheme fn that (re)computes candidates from the pattern; `None`
    /// for static sources.
    pub(super) dynamic: Option<String>,
    /// When set, the source's candidates are shown verbatim — Rust does no
    /// pattern filtering (the dynamic fn already did, e.g. via `fuzzy-match`).
    pub(super) raw: bool,
    pub(super) action: HelmAction,
    /// Name of a Scheme fn that returns the preview (as lines) for a candidate;
    /// `None` = this source has no preview pane.
    pub(super) preview: Option<String>,
}

/// An active Helm selection session (a generic completing-read UI): a prompt, a
/// live filter `pattern`, the sources, the selected match index, and the
/// `query_id` that tags in-flight dynamic-candidate requests.
pub(super) struct HelmSession {
    prompt: String,
    pattern: String,
    sources: Vec<HelmSource>,
    selected: usize,
    query_id: u64,
    /// Fuzzy (nucleo) vs substring matching, captured from settings at open.
    fuzzy: bool,
    /// Candidates marked for multi-select (`C-SPC`). When non-empty, accepting
    /// runs the action once per marked candidate instead of the selected one.
    marked: Vec<String>,
    /// Preview text for the current selection (from the source's preview fn),
    /// shown in a side pane. Empty when the selected source has no preview.
    preview: String,
    /// Query id of the in-flight preview request, so a stale result is dropped.
    preview_query: u64,
}

/// One rendered line of the Helm panel: a source header or a candidate.
pub struct HelmRow {
    pub text: String,
    /// Marginalia-style annotation shown dimmed to the right (candidates only).
    pub annotation: Option<String>,
    pub header: bool,
    pub selected: bool,
    pub marked: bool,
    /// Index into the flattened match list, for click-to-select (candidate rows
    /// only; headers carry `None`).
    pub index: Option<usize>,
}

/// A render snapshot of the Helm panel.
pub struct HelmView {
    pub prompt: String,
    pub pattern: String,
    pub rows: Vec<HelmRow>,
    /// Preview text for the current selection, shown in a side pane (`None` when
    /// the selected source has no preview fn).
    pub preview: Option<String>,
}

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
            Some((src, _)) => {
                let source = &helm.sources[*src];
                let idx = helm_candidate_index(&helm.sources, &matches, helm.selected);
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
            // Recently run commands first (savehist's command history).
            candidates: self.commands_by_recency(self.script.command_names()),
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
        let candidate_index =
            helm_candidate_index(&helm.sources, &self.helm_matches(), helm.selected);
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
            HelmAction::ExecuteCommand => {
                self.note_command(&candidate);
                self.script.dispatch_action(&candidate, None);
            }
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

#[cfg(test)]
mod tests {
    #[test]
    fn duplicate_candidates_map_to_their_own_index() {
        use super::{HelmAction, HelmSource, helm_candidate_index, helm_filter};
        // Two LSP locations rendering the same row (same basename/line/col in
        // different directories) must still select distinct targets.
        let sources = vec![HelmSource {
            name: "References".into(),
            candidates: vec![
                "mod.rs:3:1: x".into(),
                "lib.rs:9:2: y".into(),
                "mod.rs:3:1: x".into(),
            ],
            annotations: std::collections::HashMap::new(),
            dynamic: None,
            raw: false,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];
        let all = helm_filter(&sources, "", true);
        let idx: Vec<_> = (0..3)
            .map(|i| helm_candidate_index(&sources, &all, i))
            .collect();
        assert_eq!(idx, vec![Some(0), Some(1), Some(2)]);
        // Filtered (fuzzy + substring): copies keep their source order.
        for fuzzy in [true, false] {
            let m = helm_filter(&sources, "mod", fuzzy);
            assert_eq!(m.len(), 2);
            assert_eq!(helm_candidate_index(&sources, &m, 0), Some(0));
            assert_eq!(helm_candidate_index(&sources, &m, 1), Some(2));
            assert_eq!(helm_candidate_index(&sources, &m, 2), None);
        }
    }

    #[test]
    fn helm_fuzzy_ranks_and_filters() {
        use super::{HelmAction, HelmSource, helm_filter};
        let sources = vec![HelmSource {
            name: "Files".into(),
            candidates: vec![
                "foo/bar.rs".into(),
                "fxxxbxxxr.rs".into(),
                "unrelated.txt".into(),
            ],
            annotations: std::collections::HashMap::new(),
            dynamic: None,
            raw: false,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];

        // Empty pattern: everything, in original order.
        let all = helm_filter(&sources, "", true);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].1, "foo/bar.rs");

        // Fuzzy "fbr": subsequence matches only; tighter match ranks first, and
        // the non-matching candidate is dropped.
        let fuzzy = helm_filter(&sources, "fbr", true);
        assert!(
            fuzzy.iter().all(|(_, c)| c != "unrelated.txt"),
            "non-match filtered"
        );
        assert_eq!(fuzzy[0].1, "foo/bar.rs", "the tighter match ranks first");

        // Substring mode: "bar" is a literal substring of only one candidate.
        let subs = helm_filter(&sources, "bar", false);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].1, "foo/bar.rs");
    }

    #[test]
    fn helm_filter_shows_raw_sources_verbatim() {
        use super::{HelmAction, HelmSource, helm_filter};
        // A `raw` (self-filtered) source ignores the pattern entirely: its
        // candidates pass through unchanged and in order — the Scheme fn that
        // produced them is the sole filter authority.
        let sources = vec![HelmSource {
            name: "Scheme-filtered".into(),
            candidates: vec!["zebra".into(), "apple".into()],
            annotations: std::collections::HashMap::new(),
            dynamic: Some("my-filter".into()),
            raw: true,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];
        let out = helm_filter(&sources, "nomatch", true);
        assert_eq!(
            out.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>(),
            ["zebra", "apple"],
        );
    }

    #[test]
    fn helm_file_shadow_pattern_jumps_to_root_for_file_sources() {
        use super::{HelmAction, HelmSource, helm_file_shadow_pattern};
        let sources = vec![HelmSource {
            name: "Files".into(),
            candidates: Vec::new(),
            annotations: std::collections::HashMap::new(),
            dynamic: Some("find-file-candidates".into()),
            raw: false,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];

        assert_eq!(
            helm_file_shadow_pattern(&sources, "/Users/me/project//"),
            Some("/".into())
        );
        assert_eq!(
            helm_file_shadow_pattern(&sources, "/Users/me/project//etc"),
            Some("/etc".into())
        );
        assert_eq!(
            helm_file_shadow_pattern(&sources, "/Users//me//project"),
            Some("/project".into())
        );
        assert_eq!(
            helm_file_shadow_pattern(&sources, "/Users/me/project/"),
            None
        );
    }

    #[test]
    fn helm_file_shadow_pattern_ignores_generic_helm_sources() {
        use super::{HelmAction, HelmSource, helm_file_shadow_pattern};
        let sources = vec![HelmSource {
            name: "Commands".into(),
            candidates: Vec::new(),
            annotations: std::collections::HashMap::new(),
            dynamic: None,
            raw: false,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];

        assert_eq!(helm_file_shadow_pattern(&sources, "foo//bar"), None);
    }

    #[test]
    fn helm_file_accept_target_uses_typed_new_file_path() {
        use super::{HelmAction, HelmSource, helm_file_accept_target};
        let sources = vec![HelmSource {
            name: "Files".into(),
            candidates: vec!["/tmp/existing.rs".into()],
            annotations: std::collections::HashMap::new(),
            dynamic: Some("find-file-candidates".into()),
            raw: false,
            action: HelmAction::Scheme(vec![("open".into(), "open-file".into())]),
            preview: None,
        }];

        let Some((action, target)) = helm_file_accept_target(&sources, "/tmp/new-file.rs") else {
            panic!("new file path should be accepted");
        };
        assert_eq!(target, "/tmp/new-file.rs");
        assert!(matches!(action, HelmAction::Scheme(actions) if actions[0].1 == "open-file"));
    }

    #[test]
    fn helm_file_accept_target_keeps_existing_candidates_and_dirs_normal() {
        use super::{HelmAction, HelmSource, helm_file_accept_target};
        let sources = vec![HelmSource {
            name: "Files".into(),
            candidates: vec!["/tmp/existing.rs".into()],
            annotations: std::collections::HashMap::new(),
            dynamic: Some("find-file-candidates".into()),
            raw: false,
            action: HelmAction::Scheme(vec![("open".into(), "open-file".into())]),
            preview: None,
        }];

        assert_eq!(
            helm_file_accept_target(&sources, "/tmp/existing.rs").map(|(_, t)| t),
            None
        );
        assert_eq!(
            helm_file_accept_target(&sources, "/tmp/").map(|(_, t)| t),
            None
        );
    }
}
