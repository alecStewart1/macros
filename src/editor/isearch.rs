//! Incremental search for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::ops::Range;

use gpui::Context;
use regex::Regex;

use super::*;

/// Incremental-search (`C-s`) transient state, held in [`Editor::isearch`]
/// only while an `Isearch` minibuffer is open. `forward` is the current
/// direction (toggled by C-r/C-s), `origin` is where point was when the search
/// began (restored on C-g), `from` is the offset the current attempt searches
/// from, and `matched` is the range of the current hit (the base for the next
/// repeat).
pub(super) struct IsearchState {
    pub(super) forward: bool,
    pub(super) origin: usize,
    pub(super) from: usize,
    pub(super) matched: Option<Range<usize>>,
}

impl Editor {
    /// Byte ranges of the active search query's matches *within `[start, end)`*
    /// (the visible region), in absolute buffer offsets. Empty when no search is
    /// active or the regex doesn't compile. Scoped to the viewport so the cost
    /// is O(visible text), not O(document), and stays correct-by-construction
    /// (recomputed from the live buffer each frame — no cache to go stale).
    /// Navigation (`n`/`N`) searches the whole buffer separately in `do_search`.
    pub fn search_match_ranges_in(&self, start: usize, end: usize) -> Vec<Range<usize>> {
        let Some(query) = self.search_query.as_ref().filter(|q| !q.is_empty()) else {
            return Vec::new();
        };
        let Ok(regex) = Regex::new(query) else {
            return Vec::new();
        };
        let end = end.min(self.active.buffer.len());
        let start = start.min(end);
        // `start`/`end` are line boundaries (char-safe); slice only the visible
        // bytes, then map match offsets back to absolute positions.
        let slice = self.active.buffer.slice_to_string(start..end);
        regex
            .find_iter(&slice)
            .filter(|m| m.end() > m.start()) // skip zero-width matches
            .map(|m| (start + m.start())..(start + m.end()))
            .collect()
    }

    // --- Incremental search (Emacs `C-s` / `C-r`) -------------------------

    /// Start an incremental search. Reads a query in the minibuffer and matches
    /// it live (see `isearch_update`); `C-s`/`C-r` repeat, `RET` finishes leaving
    /// point on the match, and `C-g` aborts back to where the search began.
    pub(super) fn isearch_start(&mut self, forward: bool, cx: &mut Context<Self>) {
        let point = self.point();
        self.isearch = Some(IsearchState {
            forward,
            origin: point,
            from: point,
            matched: None,
        });
        self.search_query = None;
        let prompt = if forward {
            "I-search: "
        } else {
            "I-search backward: "
        };
        self.open_minibuffer(prompt, MinibufferPurpose::Isearch, "", cx);
    }

    /// Re-run the search for the current minibuffer string from `isearch_from`.
    /// Called on every edit of the search string, so matching is incremental.
    pub(super) fn isearch_update(&mut self, cx: &mut Context<Self>) {
        let Some(query) = self.minibuffer.as_ref().map(|mb| mb.input.clone()) else {
            return;
        };
        let Some((forward, from)) = self.isearch.as_ref().map(|st| (st.forward, st.from)) else {
            return;
        };
        self.isearch_run(&query, forward, from, cx);
    }

    /// `C-s` / `C-r` while searching: repeat in `forward`'s direction. An empty
    /// search string first recalls the previous one (Emacs `C-s C-s`); otherwise
    /// the search advances past the current match.
    pub(super) fn isearch_repeat(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(st) = self.isearch.as_mut() else {
            return;
        };
        st.forward = forward;
        let empty = self
            .minibuffer
            .as_ref()
            .is_some_and(|mb| mb.input.is_empty());
        if empty {
            if let Some(prev) = self.last_isearch.clone()
                && let Some(mb) = self.minibuffer.as_mut()
            {
                mb.input = prev;
                mb.cursor = mb.input.len();
            }
        } else if let Some(m) = st.matched.clone() {
            // Step off the current match so the next hit is a different one.
            st.from = if forward {
                m.start + 1
            } else {
                m.start.saturating_sub(1)
            };
        }
        let from = self.isearch.as_ref().map_or(0, |st| st.from);
        let query = self
            .minibuffer
            .as_ref()
            .map(|mb| mb.input.clone())
            .unwrap_or_default();
        self.isearch_run(&query, forward, from, cx);
    }

    /// Find the next match of `query` from `from` in the given direction, wrapping
    /// around the buffer; move point onto it (its end going forward, its start
    /// going backward) and record the match. Updates the minibuffer hint on
    /// failure or an invalid regex. An empty query just clears the highlight.
    pub(super) fn isearch_run(
        &mut self,
        query: &str,
        forward: bool,
        from: usize,
        cx: &mut Context<Self>,
    ) {
        if query.is_empty() {
            self.search_query = None;
            if let Some(st) = self.isearch.as_mut() {
                st.matched = None;
            }
            if let Some(mb) = self.minibuffer.as_mut() {
                mb.message.clear();
            }
            cx.notify();
            return;
        }
        self.search_query = Some(query.to_string());
        let regex = match Regex::new(query) {
            Ok(r) => r,
            Err(_) => {
                if let Some(mb) = self.minibuffer.as_mut() {
                    mb.message = " [invalid regex]".into();
                }
                cx.notify();
                return;
            }
        };
        let text = self.active.buffer.full_text();
        let matches: Vec<Range<usize>> = regex
            .find_iter(&text)
            .filter(|m| m.end() > m.start())
            .map(|m| m.start()..m.end())
            .collect();
        let found = if forward {
            matches
                .iter()
                .find(|m| m.start >= from)
                .or_else(|| matches.first())
                .cloned()
        } else {
            matches
                .iter()
                .rev()
                .find(|m| m.start <= from)
                .or_else(|| matches.last())
                .cloned()
        };
        match found {
            Some(m) => {
                let target = if forward { m.end } else { m.start };
                if let Some(st) = self.isearch.as_mut() {
                    st.matched = Some(m);
                }
                if let Some(mb) = self.minibuffer.as_mut() {
                    mb.message.clear();
                }
                self.move_to(target, cx);
            }
            None => {
                if let Some(st) = self.isearch.as_mut() {
                    st.matched = None;
                }
                if let Some(mb) = self.minibuffer.as_mut() {
                    mb.message = " [failing]".into();
                }
                cx.notify();
            }
        }
    }
}
