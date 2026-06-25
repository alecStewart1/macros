//! Completion popup (corfu-style) methods for [`Editor`].

use super::*;

impl Editor {
    /// The candidate set filtered by the live prefix `buffer[anchor..point]`.
    pub(super) fn popup_filtered(&self) -> Vec<CompletionItem> {
        let Some(popup) = self.completion_popup.as_ref() else {
            return Vec::new();
        };
        let point = self.point();
        let prefix = if point >= popup.anchor {
            self.buffer.slice_to_string(popup.anchor..point)
        } else {
            String::new()
        };
        popup
            .items
            .iter()
            .filter(|c| c.label.starts_with(&prefix))
            .cloned()
            .collect()
    }

    /// Render snapshot for the element: visible rows (label + detail + kind) and
    /// the selected one.
    pub fn completion_view(&self) -> Option<CompletionView> {
        let popup = self.completion_popup.as_ref()?;
        let items = self.popup_filtered();
        if items.is_empty() {
            return None;
        }
        let selected = popup.selected.min(items.len() - 1);
        let doc = items.get(selected).and_then(|i| i.documentation.clone());
        let rows = items
            .into_iter()
            .map(|item| CompletionRow {
                label: item.label,
                detail: item.detail,
                kind: item.kind,
            })
            .collect();
        Some(CompletionView {
            items: rows,
            selected,
            doc,
        })
    }

    pub(super) fn completion_next(&mut self, cx: &mut Context<Self>) {
        self.completion_cycle(true, cx);
    }

    pub(super) fn completion_previous(&mut self, cx: &mut Context<Self>) {
        self.completion_cycle(false, cx);
    }

    fn completion_cycle(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.popup_filtered().len();
        if let Some(popup) = self.completion_popup.as_mut() {
            popup.selected = wrap_index(popup.selected, count, forward);
        }
        self.completion_resolve_selected();
        cx.notify();
    }

    pub(super) fn completion_accept(&mut self, cx: &mut Context<Self>) {
        let filtered = self.popup_filtered();
        let Some(popup) = self.completion_popup.take() else {
            return;
        };
        if let Some(choice) = filtered
            .get(popup.selected.min(filtered.len().saturating_sub(1)))
            .cloned()
        {
            let point = self.point();
            if choice.additional_edits.is_empty() {
                let cursor = self.edit(popup.anchor..point, &choice.insert_text);
                self.collapse_to(cursor);
            } else {
                self.accept_completion_with_edits(popup.anchor, point, &choice, cx);
            }
        }
        cx.notify();
    }

    /// A mouse click on filtered-candidate row `index`: select it and accept.
    pub(super) fn completion_click(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.popup_filtered().len();
        if index >= count {
            return;
        }
        if let Some(popup) = self.completion_popup.as_mut() {
            popup.selected = index;
        }
        self.completion_accept(cx);
    }

    pub(super) fn completion_abort(&mut self, cx: &mut Context<Self>) {
        self.completion_popup = None;
        cx.notify();
    }

    /// After an edit while the popup is open: close it if the prefix no longer
    /// matches anything or point moved before the anchor; otherwise reset the
    /// selection to the top.
    pub(super) fn popup_refilter(&mut self, cx: &mut Context<Self>) {
        let Some(anchor) = self.completion_popup.as_ref().map(|p| p.anchor) else {
            return;
        };
        if self.point() < anchor || self.popup_filtered().is_empty() {
            self.completion_popup = None;
        } else if let Some(popup) = self.completion_popup.as_mut() {
            popup.selected = 0;
        }
        cx.notify();
    }

    /// If the popup is open, run the `completion`-keymap binding for `key`, or
    /// the given default command if unbound. Returns whether it was handled.
    pub(super) fn popup_special_key(
        &mut self,
        key: &str,
        default: EditorCommand,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.completion_popup.is_none() {
            return false;
        }
        match self
            .keymap
            .resolve_mode("completion", key)
            .map(str::to_string)
        {
            Some(expr) => self.run_script(&expr, cx),
            None => self.apply_command(default, cx),
        }
        true
    }

    /// Whether a language server is ready and tracking the active buffer.
    pub(super) fn has_ready_lsp(&self) -> bool {
        let (Some(language), Some(uri)) = (self.active_language(), self.active_uri()) else {
            return false;
        };
        self.servers
            .get(&language)
            .is_some_and(|s| s.ready && s.open.contains(&uri))
    }

    /// After a self-insert, open the popup automatically if `completion-auto` is
    /// on, a word prefix of sufficient length precedes point, and a server is
    /// ready (corfu's auto behavior). One request per session: once the popup is
    /// open, further typing filters client-side via `popup_refilter`.
    pub(super) fn maybe_auto_complete(&mut self, typed: &str, cx: &mut Context<Self>) {
        if self.completion_auto_pending {
            return;
        }
        let settings = cx.global::<crate::settings::Settings>();
        if !settings.completion_auto() {
            return;
        }
        let min_prefix = settings.completion_auto_prefix();
        // Only trigger right after typing a word character.
        if !typed
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return;
        }
        let point = self.point();
        let word_start = self.buffer.backward_word(point);
        if point.saturating_sub(word_start) < min_prefix.max(1) {
            return;
        }
        if !self.has_ready_lsp() {
            return;
        }
        self.completion_auto_pending = true;
        // Auto-trigger is LSP-driven (it only fires when a server is ready);
        // Scheme CAPFs participate in the explicit `completion-at-point` command.
        self.completion_at_point_lsp(cx);
    }
}
