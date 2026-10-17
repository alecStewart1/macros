use std::collections::HashMap;

/// A mode active in a buffer. Modes contribute a highlighting language and, via
/// the global [`Keymap`], key bindings. Keymaps live globally (keyed by mode
/// name) rather than on the mode instance, so every buffer using a mode shares
/// its bindings — the Emacs model.
#[derive(Debug, Clone)]
pub struct Mode {
    pub name: String,
    /// The tree-sitter language this mode highlights with, if any.
    pub language: Option<String>,
}

impl Mode {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            language: None,
        }
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }
}

/// The ordered set of modes active in a buffer (top of stack = last element).
#[derive(Debug, Default, Clone)]
pub struct ModeStack {
    modes: Vec<Mode>,
}

impl ModeStack {
    /// Just the always-present `fundamental` base mode.
    pub fn fundamental() -> Self {
        Self {
            modes: vec![Mode::new("fundamental")],
        }
    }

    /// `major` stacked on the `fundamental` base — a buffer's usual shape.
    pub fn with_major(major: Mode) -> Self {
        let mut stack = Self::fundamental();
        stack.push(major);
        stack
    }

    pub fn push(&mut self, mode: Mode) {
        self.modes.push(mode);
    }

    /// Mode names bottom-to-top, for the mode line.
    pub fn names(&self) -> Vec<&str> {
        self.modes.iter().map(|m| m.name.as_str()).collect()
    }

    /// Mode names top-to-bottom, for keymap resolution (newer shadows older).
    pub fn resolution_order(&self) -> impl Iterator<Item = &str> {
        self.modes.iter().rev().map(|m| m.name.as_str())
    }

    /// The highlighting language of the topmost mode that declares one.
    pub fn active_language(&self) -> Option<&str> {
        self.modes.iter().rev().find_map(|m| m.language.as_deref())
    }
}

/// The keymap holding modal leader chords (`evil-leader-set`). Normal state in
/// file buffers resolves them from `evil-normal` like any other key; this copy
/// is what the places `evil-normal` never reaches consult — special buffers
/// (magit, dashboards, terminals) and Insert state (via the alt leader).
pub const LEADER_KEYMAP: &str = "evil-leader";

/// Where a key sequence lands in a special buffer's modal overlay plus the
/// leader keymap (see [`Keymap::resolve_leader`]).
#[derive(Debug, PartialEq, Eq)]
pub enum LeaderMatch<'a> {
    /// A complete binding: run this expression.
    Bound(&'a str),
    /// A strict prefix of a longer binding: wait for the next key.
    Prefix,
    /// Neither keymap knows the sequence.
    Miss,
}

/// Global key bindings, keyed by mode name then by chord sequence (e.g.
/// `"ctrl-x ctrl-f"`) → Steel expression. Shared across all buffers.
#[derive(Debug, Default)]
pub struct Keymap {
    maps: HashMap<String, HashMap<String, String>>,
}

impl Keymap {
    /// Bind `sequence` to `expr` in `mode`'s keymap — what `define-key` drives.
    pub fn define(
        &mut self,
        mode: impl Into<String>,
        sequence: impl Into<String>,
        expr: impl Into<String>,
    ) {
        self.maps
            .entry(mode.into())
            .or_default()
            .insert(sequence.into(), expr.into());
    }

    /// Resolve a sequence against a buffer's active modes (top-down); the first
    /// mode that binds it wins.
    pub fn resolve(&self, modes: &ModeStack, sequence: &str) -> Option<&str> {
        modes
            .resolution_order()
            .find_map(|name| self.maps.get(name).and_then(|m| m.get(sequence)))
            .map(String::as_str)
    }

    /// Resolve a sequence against a single named mode's keymap, ignoring the
    /// mode stack. Used for transient overlays (e.g. the completion popup) that
    /// borrow their bindings from one mode regardless of the active buffer.
    pub fn resolve_mode(&self, mode: &str, sequence: &str) -> Option<&str> {
        self.maps
            .get(mode)
            .and_then(|m| m.get(sequence))
            .map(String::as_str)
    }

    /// Whether `sequence` is a strict prefix of a longer binding in `mode` — for
    /// multi-key sequences (e.g. `g g`, leader `space …`) in a single mode.
    pub fn is_prefix_mode(&self, mode: &str, sequence: &str) -> bool {
        let with_sep = format!("{sequence} ");
        self.maps
            .get(mode)
            .is_some_and(|m| m.keys().any(|k| k.starts_with(&with_sep)))
    }

    /// Whether `sequence` is a strict prefix of some longer binding in any of
    /// the active modes (a pending prefix awaiting more keys).
    pub fn is_prefix(&self, modes: &ModeStack, sequence: &str) -> bool {
        modes
            .resolution_order()
            .any(|name| self.is_prefix_mode(name, sequence))
    }

    /// Resolve an evil `state` keymap (`"evil-normal"` / `"evil-visual"` / …),
    /// letting a major-mode-scoped keymap `"<state>:<major>"` shadow the global
    /// one — so `(define-key "evil-normal:rust" …)` overrides `evil-normal` in
    /// Rust buffers only. Falls back to the global `state` keymap.
    pub fn resolve_evil(&self, state: &str, major: Option<&str>, sequence: &str) -> Option<&str> {
        major
            .and_then(|m| self.resolve_mode(&format!("{state}:{m}"), sequence))
            .or_else(|| self.resolve_mode(state, sequence))
    }

    /// Like [`is_prefix_mode`](Self::is_prefix_mode) but across the major-mode
    /// scoped keymap and the global `state` keymap (so a mode-specific multi-key
    /// sequence still pends).
    pub fn is_prefix_evil(&self, state: &str, major: Option<&str>, sequence: &str) -> bool {
        major.is_some_and(|m| self.is_prefix_mode(&format!("{state}:{m}"), sequence))
            || self.is_prefix_mode(state, sequence)
    }

    /// Resolve `sequence` against the leader keymap, letting `overlay` (a
    /// special buffer's `<style>-normal:<major>` map, when there is one) shadow
    /// it — so `evil-leader-set-for-modes` beats the global leader in that mode.
    /// Same precedence as the special-buffer layer: an exact overlay binding,
    /// then a LONGER overlay binding (pend), then the leader map's.
    pub fn resolve_leader(&self, overlay: Option<&str>, sequence: &str) -> LeaderMatch<'_> {
        if let Some(o) = overlay {
            if let Some(expr) = self.resolve_mode(o, sequence) {
                return LeaderMatch::Bound(expr);
            }
            if self.is_prefix_mode(o, sequence) {
                return LeaderMatch::Prefix;
            }
        }
        if let Some(expr) = self.resolve_mode(LEADER_KEYMAP, sequence) {
            LeaderMatch::Bound(expr)
        } else if self.is_prefix_mode(LEADER_KEYMAP, sequence) {
            LeaderMatch::Prefix
        } else {
            LeaderMatch::Miss
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Keymap, LEADER_KEYMAP, LeaderMatch};

    #[test]
    fn leader_keymap_resolves_under_mode_overlay() {
        let mut km = Keymap::default();
        km.define(LEADER_KEYMAP, "space n o", "delete-other-windows");
        km.define(LEADER_KEYMAP, "alt-space n o", "delete-other-windows");
        km.define("evil-normal:k8s-mode", "j", "next-line");
        km.define("evil-normal:k8s-mode", "space n d", "k8s-describe");

        let ov = Some("evil-normal:k8s-mode");
        assert_eq!(km.resolve_leader(ov, "space"), LeaderMatch::Prefix);
        assert_eq!(km.resolve_leader(ov, "space n"), LeaderMatch::Prefix);
        assert_eq!(
            km.resolve_leader(ov, "space n o"),
            LeaderMatch::Bound("delete-other-windows")
        );
        // A per-mode binding shadows the global leader in that mode only.
        assert_eq!(
            km.resolve_leader(ov, "space n d"),
            LeaderMatch::Bound("k8s-describe")
        );
        assert_eq!(km.resolve_leader(None, "space n d"), LeaderMatch::Miss);
        // The overlay's own plain keys still resolve (it runs first).
        assert_eq!(km.resolve_leader(ov, "j"), LeaderMatch::Bound("next-line"));
        // The alt leader works without any overlay (Insert state, terminals).
        assert_eq!(km.resolve_leader(None, "alt-space"), LeaderMatch::Prefix);
        assert_eq!(
            km.resolve_leader(None, "alt-space n o"),
            LeaderMatch::Bound("delete-other-windows")
        );
        assert_eq!(km.resolve_leader(ov, "space x"), LeaderMatch::Miss);
    }

    #[test]
    fn evil_mode_scoped_keymaps_shadow_global() {
        let mut km = Keymap::default();
        km.define("evil-normal", "space g g", "global-def");
        km.define("evil-normal:rust", "space g g", "lsp-find-definition");
        km.define("evil-normal:rust", "space t", "lsp-hover");

        // In a rust buffer the scoped binding wins; the rust-only key resolves.
        assert_eq!(
            km.resolve_evil("evil-normal", Some("rust"), "space g g"),
            Some("lsp-find-definition")
        );
        assert_eq!(
            km.resolve_evil("evil-normal", Some("rust"), "space t"),
            Some("lsp-hover")
        );
        // In another major mode, only the global binding applies.
        assert_eq!(
            km.resolve_evil("evil-normal", Some("go"), "space g g"),
            Some("global-def")
        );
        assert_eq!(km.resolve_evil("evil-normal", Some("go"), "space t"), None);
        // No major mode → global only.
        assert_eq!(
            km.resolve_evil("evil-normal", None, "space g g"),
            Some("global-def")
        );

        // Prefixes pend across both scoped and global keymaps.
        assert!(km.is_prefix_evil("evil-normal", Some("rust"), "space"));
        assert!(km.is_prefix_evil("evil-normal", Some("rust"), "space g"));
        assert!(km.is_prefix_evil("evil-normal", Some("go"), "space")); // global "space g g"
        assert!(!km.is_prefix_evil("evil-normal", Some("go"), "space t"));
    }
}
