//! Incremental search for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::ops::Range;

use gpui::Context;
use regex::Regex;

use super::*;

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
        let end = end.min(self.buffer.len());
        let start = start.min(end);
        // `start`/`end` are line boundaries (char-safe); slice only the visible
        // bytes, then map match offsets back to absolute positions.
        let slice = self.buffer.slice_to_string(start..end);
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
        self.isearch_forward = forward;
        self.isearch_origin = self.point();
        self.isearch_from = self.point();
        self.isearch_match = None;
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
        let (forward, from) = (self.isearch_forward, self.isearch_from);
        self.isearch_run(&query, forward, from, cx);
    }

    /// `C-s` / `C-r` while searching: repeat in `forward`'s direction. An empty
    /// search string first recalls the previous one (Emacs `C-s C-s`); otherwise
    /// the search advances past the current match.
    pub(super) fn isearch_repeat(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.isearch_forward = forward;
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
        } else if let Some(m) = self.isearch_match.clone() {
            // Step off the current match so the next hit is a different one.
            self.isearch_from = if forward {
                m.start + 1
            } else {
                m.start.saturating_sub(1)
            };
        }
        let query = self
            .minibuffer
            .as_ref()
            .map(|mb| mb.input.clone())
            .unwrap_or_default();
        let from = self.isearch_from;
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
            self.isearch_match = None;
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
        let text = self.buffer.full_text();
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
                self.isearch_match = Some(m);
                if let Some(mb) = self.minibuffer.as_mut() {
                    mb.message.clear();
                }
                self.move_to(target, cx);
            }
            None => {
                self.isearch_match = None;
                if let Some(mb) = self.minibuffer.as_mut() {
                    mb.message = " [failing]".into();
                }
                cx.notify();
            }
        }
    }
}
