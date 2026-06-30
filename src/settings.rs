use std::collections::HashMap;

use gpui::Global;

/// Parse a string as a boolean. The single source of truth for which spellings
/// count as "true" (mirrors common Emacs truthiness). Used when a boolean option
/// is set from a *string* value, e.g. `(set-option "completion-auto" "true")`.
fn parse_bool(value: &str) -> bool {
    matches!(value, "true" | "t" | "yes" | "1" | "on")
}

/// A typed option value. Options are set from Scheme via `(set-option "name"
/// value)` where `value` may be a boolean, number, or string; the value is
/// coerced **once, at write time** to the option's declared type (see
/// [`option_kind`]) and stored as-is. Reads are then a direct typed lookup — no
/// per-read parsing.
#[derive(Debug, Clone)]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// Compare an option value against a string literal — true only for the `Str`
/// variant. Keeps the command-queue tests able to write `v == "false"`.
impl PartialEq<&str> for OptionValue {
    fn eq(&self, other: &&str) -> bool {
        matches!(self, OptionValue::Str(s) if s.as_str() == *other)
    }
}

impl OptionValue {
    /// Interpret the value as a boolean, loosely: native booleans pass through,
    /// numbers are truthy when non-zero, and strings use [`parse_bool`]. Used by
    /// other stringly-or-natively-typed setters (e.g. `set-search-option`).
    pub fn truthy(&self) -> bool {
        match self {
            OptionValue::Bool(b) => *b,
            OptionValue::Int(i) => *i != 0,
            OptionValue::Float(f) => *f != 0.0,
            OptionValue::Str(s) => parse_bool(s),
        }
    }

    /// Interpret the value as a non-negative integer, accepting a native number
    /// or a numeric string. `None` if it isn't one.
    pub fn as_usize(&self) -> Option<usize> {
        match self {
            OptionValue::Int(i) if *i >= 0 => Some(*i as usize),
            OptionValue::Float(f) if *f >= 0.0 => Some(*f as usize),
            OptionValue::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

/// The declared type of an option (the family it belongs to). Per-mode options
/// like `line-numbers:dired-sidebar-mode` take their type from the base name.
#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Int,
    Float,
    Str,
}

/// Every option key Rust knows about — exposed to Scheme as `(option-list)` so
/// `helm-apropos` can surface options (e.g. searching "font" finds `font-size`)
/// alongside functions and variables. Keep in sync with [`option_kind`]; the
/// `known_options_have_a_kind` test guards against drift. Scheme-only options
/// (stored verbatim, no `option_kind` arm) aren't listed — Rust doesn't know
/// their names ahead of time.
pub const KNOWN_OPTIONS: &[&str] = &[
    "font-size",
    "line-height",
    "helm-height",
    "cursor-width",
    "divider-width",
    "cursor-blink-interval",
    "helm-bevel-top",
    "helm-bevel-right",
    "helm-bevel-bottom",
    "helm-bevel-left",
    "tab-width",
    "completion-auto-prefix",
    "completion-max-rows",
    "completion-style",
    "cursor-style",
    "font-family",
    "completion-auto",
    "completion-show-detail",
    "completion-show-kind",
    "completion-capf",
    "cursor-blink",
    "scroll-bar",
    "diagnostics-gutter",
    "lsp-log-on-start",
    "auto-update",
    "helm-fuzzy",
    "line-numbers",
    "hl-line",
    "word-wrap",
    "auto-revert",
    "electric-indent",
    "show-paren",
];

/// Type + numeric bounds for a known option, looked up by its base name (the
/// part before any `:mode` suffix). `None` means the option is unknown to Rust —
/// it is stored verbatim so Scheme-only options still round-trip.
fn option_kind(key: &str) -> Option<(Kind, Option<f64>, Option<f64>)> {
    use Kind::*;
    // Per-mode options (`base:mode`) carry their type in the base name.
    let base = key.split(':').next().unwrap_or(key);
    let spec = match base {
        "font-size" | "line-height" => (Float, None, None),
        "helm-height" => (Float, Some(0.1), Some(0.95)),
        "cursor-width" | "divider-width" => (Float, Some(1.0), None),
        "cursor-blink-interval" => (Float, Some(60.0), None),
        "helm-bevel-top" | "helm-bevel-right" | "helm-bevel-bottom" | "helm-bevel-left" => {
            (Float, Some(0.0), None)
        }
        "tab-width" | "completion-auto-prefix" => (Int, Some(0.0), None),
        "completion-max-rows" => (Int, Some(1.0), None),
        "completion-style" | "cursor-style" | "font-family" => (Str, None, None),
        "completion-auto"
        | "completion-show-detail"
        | "completion-show-kind"
        | "completion-capf"
        | "cursor-blink"
        | "scroll-bar"
        | "diagnostics-gutter"
        | "lsp-log-on-start"
        | "auto-update"
        | "helm-fuzzy"
        | "line-numbers"
        | "hl-line"
        | "word-wrap"
        | "auto-revert"
        | "electric-indent"
        | "show-paren" => (Bool, None, None),
        _ => return None,
    };
    Some(spec)
}

/// Coerce a raw value (as supplied to `set-option`) to `kind`, applying the
/// `min`/`max` bounds to numbers. Returns `None` if the raw value can't sensibly
/// become that type (e.g. `"abc"` for an integer) — the caller then leaves the
/// option unset so its default applies.
fn coerce(
    kind: Kind,
    min: Option<f64>,
    max: Option<f64>,
    raw: &OptionValue,
) -> Option<OptionValue> {
    let clamp = |mut x: f64| {
        if let Some(lo) = min {
            x = x.max(lo);
        }
        if let Some(hi) = max {
            x = x.min(hi);
        }
        x
    };
    Some(match kind {
        Kind::Bool => OptionValue::Bool(match raw {
            OptionValue::Bool(b) => *b,
            OptionValue::Int(i) => *i != 0,
            OptionValue::Str(s) => parse_bool(s),
            OptionValue::Float(_) => return None,
        }),
        Kind::Int => OptionValue::Int(clamp(match raw {
            OptionValue::Int(i) => *i as f64,
            OptionValue::Float(f) => *f,
            OptionValue::Str(s) => s.trim().parse::<f64>().ok()?,
            OptionValue::Bool(_) => return None,
        }) as i64),
        Kind::Float => OptionValue::Float(clamp(match raw {
            OptionValue::Float(f) => *f,
            OptionValue::Int(i) => *i as f64,
            OptionValue::Str(s) => s.trim().parse::<f64>().ok()?,
            OptionValue::Bool(_) => return None,
        })),
        Kind::Str => OptionValue::Str(match raw {
            OptionValue::Str(s) => s.clone(),
            OptionValue::Bool(b) => b.to_string(),
            OptionValue::Int(i) => i.to_string(),
            OptionValue::Float(f) => f.to_string(),
        }),
    })
}

/// User-tunable options, set from Scheme via `(set-option "name" value)` and read
/// by the renderer / commands. Each value is coerced to the option's declared
/// type when it is set ([`Settings::set`]) and stored typed, so the read-side
/// getters are plain lookups. Unknown option names are stored verbatim.
///
/// Held as a GPUI global (like [`crate::theme::Theme`]); defaults live in the
/// typed getters so an unset option still has a sensible value.
#[derive(Default)]
pub struct Settings {
    values: HashMap<String, OptionValue>,
}

impl Global for Settings {}

impl Settings {
    /// A clone of the currently-set option values, for publishing to the script
    /// engine so Scheme can read them via `(get-option …)`.
    pub fn snapshot(&self) -> HashMap<String, OptionValue> {
        self.values.clone()
    }

    /// Set an option, coercing the raw value to the option's declared type. A
    /// value that can't be coerced is dropped (the option keeps its default); an
    /// unknown option name is stored as-is.
    pub fn set(&mut self, key: String, value: OptionValue) {
        let stored = match option_kind(&key) {
            Some((kind, min, max)) => match coerce(kind, min, max, &value) {
                Some(v) => v,
                None => return,
            },
            None => value,
        };
        self.values.insert(key, stored);
    }

    fn f32_or(&self, key: &str, default: f32) -> f32 {
        match self.values.get(key) {
            Some(OptionValue::Float(f)) => *f as f32,
            Some(OptionValue::Int(i)) => *i as f32,
            _ => default,
        }
    }

    fn usize_or(&self, key: &str, default: usize) -> usize {
        match self.values.get(key) {
            Some(OptionValue::Int(i)) if *i >= 0 => *i as usize,
            Some(OptionValue::Float(f)) if *f >= 0.0 => *f as usize,
            _ => default,
        }
    }

    fn bool_or(&self, key: &str, default: bool) -> bool {
        match self.values.get(key) {
            Some(OptionValue::Bool(b)) => *b,
            _ => default,
        }
    }

    fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        match self.values.get(key) {
            Some(OptionValue::Str(s)) => s.as_str(),
            _ => default,
        }
    }

    /// Resolve a *per-major-mode* boolean option: a `base:<mode>` value, if set,
    /// overrides the global `base` default. Used for options like line numbers
    /// and hl-line that a specific mode can flip without changing the default.
    fn bool_per_mode(&self, base: &str, mode: Option<&str>, default: bool) -> bool {
        mode.and_then(|m| match self.values.get(&format!("{base}:{m}")) {
            Some(OptionValue::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or_else(|| self.bool_or(base, default))
    }

    /// Editor font family. A single family name (not a fallback list); the
    /// named font must be installed or GPUI falls back to its own default.
    pub fn font_family(&self) -> &str {
        self.str_or("font-family", "Lilex")
    }

    /// Editor font size in pixels.
    pub fn font_size(&self) -> f32 {
        self.f32_or("font-size", 15.0)
    }

    /// Line height in pixels.
    pub fn line_height(&self) -> f32 {
        self.f32_or("line-height", 22.0)
    }

    /// Spaces per indentation step, used by auto-indent across all languages:
    /// each tree-sitter structural level is `tab-width` columns, and a Lisp
    /// special form indents its body `tab-width` past the opener. Set from Scheme
    /// with `(set-option "tab-width" N)`; default 4.
    pub fn tab_width(&self) -> usize {
        self.usize_or("tab-width", 4)
    }

    /// Whether RET keeps the current indentation (and, in Lisp buffers, indents
    /// to the enclosing form) and TAB reindents the current Lisp line. Emacs
    /// `electric-indent-mode`. Set `electric-indent` to `#f` to disable.
    pub fn electric_indent(&self) -> bool {
        self.bool_or("electric-indent", true)
    }

    /// Whether the bracket at (or just before) point and its match are
    /// highlighted (Emacs `show-paren-mode`). A *per-major-mode* option: on
    /// everywhere by default except prose/data buffers (`markdown`, `toml`, and
    /// plain text in `fundamental`), where bracket matching is just noise. A
    /// `show-paren` / `show-paren:<mode>` option overrides — e.g.
    /// `(set-option "show-paren:markdown" #t)` to re-enable it there.
    pub fn show_paren_for(&self, mode: Option<&str>) -> bool {
        let default = !matches!(
            mode,
            None | Some("markdown" | "toml" | "fundamental" | "text")
        );
        self.bool_per_mode("show-paren", mode, default)
    }

    /// How completion candidates are presented: `"popup"` (corfu-style overlay)
    /// or `"minibuffer"`.
    pub fn completion_style(&self) -> &str {
        self.str_or("completion-style", "popup")
    }

    /// Whether the popup appears automatically as you type (corfu `corfu-auto`).
    pub fn completion_auto(&self) -> bool {
        self.bool_or("completion-auto", true)
    }

    /// Minimum prefix length before auto-completion triggers
    /// (corfu `corfu-auto-prefix`).
    pub fn completion_auto_prefix(&self) -> usize {
        self.usize_or("completion-auto-prefix", 2)
    }

    /// Maximum candidate rows shown in the popup (corfu `corfu-count`). The
    /// minimum of 1 is enforced when the option is set.
    pub fn completion_max_rows(&self) -> usize {
        self.usize_or("completion-max-rows", 10)
    }

    /// Show the signature / type detail next to each candidate.
    pub fn completion_show_detail(&self) -> bool {
        self.bool_or("completion-show-detail", true)
    }

    /// Show the kind tag (`fn`, `var`, …) for each candidate.
    pub fn completion_show_kind(&self) -> bool {
        self.bool_or("completion-show-kind", true)
    }

    /// Whether `completion-at-point` consults the registered Scheme
    /// completion-at-point-functions before falling back to the language server.
    /// On by default; set `(set-option "completion-capf" #f)` for strictly
    /// LSP-driven completion (skipping CAPFs and their engine round-trip).
    pub fn completion_capf(&self) -> bool {
        self.bool_or("completion-capf", true)
    }

    /// Height of the Helm panel as a fraction of the window (default 0.5). The
    /// 0.1–0.95 range is enforced when the option is set.
    pub fn helm_height(&self) -> f32 {
        self.f32_or("helm-height", 0.5)
    }

    /// Cursor shape: `"block"` (default, Zed-style), `"bar"`, or `"auto"`
    /// (block, but a bar in evil insert mode).
    pub fn cursor_style(&self) -> &str {
        self.str_or("cursor-style", "block")
    }

    /// Bar-cursor width in pixels (default 2; minimum 1 enforced on set).
    pub fn cursor_width(&self) -> f32 {
        self.f32_or("cursor-width", 2.0)
    }

    /// Whether the cursor blinks. On by default (Zed-style blinking block); set
    /// `(set-option "cursor-blink" #f)` for a steady cursor (no idle redraw).
    pub fn cursor_blink(&self) -> bool {
        self.bool_or("cursor-blink", true)
    }

    /// Cursor blink half-period in milliseconds (time the cursor stays shown,
    /// then hidden). Default 530, matching Emacs `blink-cursor-interval` (0.53s).
    /// The 60ms minimum is enforced when the option is set.
    pub fn cursor_blink_interval(&self) -> u64 {
        self.f32_or("cursor-blink-interval", 530.0) as u64
    }

    /// Whether to show the line-number gutter for a buffer whose major mode is
    /// `mode` (Emacs `display-line-numbers-mode`). It's a *per-major-mode*
    /// option: a `line-numbers:<mode>` value overrides the global `line-numbers`
    /// default, which is on for every mode unless a mode (e.g. dired-sidebar) turns
    /// it off.
    pub fn line_numbers_for(&self, mode: Option<&str>) -> bool {
        self.bool_per_mode("line-numbers", mode, true)
    }

    /// Highlight the line the cursor is on (Emacs `hl-line-mode`). A *per-major-
    /// mode* option: a `hl-line:<mode>` value overrides the global `hl-line`
    /// default, which is off everywhere unless a mode (e.g. magit-status) turns
    /// it on. Painted as a subtle full-width background band behind the text.
    pub fn hl_line_for(&self, mode: Option<&str>) -> bool {
        self.bool_per_mode("hl-line", mode, false)
    }

    /// Show a scroll bar on the right edge of each pane (Emacs `scroll-bar-mode`).
    pub fn scroll_bar(&self) -> bool {
        self.bool_or("scroll-bar", true)
    }

    /// Soft-wrap long lines at the window edge (Emacs `visual-line-mode` /
    /// `word-wrap`). On by default. When off, long lines are not wrapped and the
    /// buffer scrolls horizontally to reveal off-screen text. A *per-major-mode*
    /// option: a `word-wrap:<mode>` value overrides the global `word-wrap`
    /// default, so a mode (e.g. the dired-sidebar tree) can truncate long lines
    /// while ordinary buffers keep wrapping.
    pub fn word_wrap_for(&self, mode: Option<&str>) -> bool {
        self.bool_per_mode("word-wrap", mode, true)
    }

    /// Show a colored dot in the line-number gutter on lines that carry an LSP
    /// diagnostic (red for an error, orange for a warning, blue for info/hint) —
    /// the worst severity on the line wins. On by default; set
    /// `(set-option "diagnostics-gutter" #f)` to hide the dots. Only drawn when
    /// the line-number gutter itself is shown.
    pub fn diagnostics_gutter(&self) -> bool {
        self.bool_or("diagnostics-gutter", true)
    }

    /// Pop open `*lsp-log*` in a split when an external language server starts,
    /// streaming its stderr (handy for diagnosing startup). Off by default — the
    /// log is always captured; open it with `C-h l` (`M-x list-lsp-log`). Only
    /// fires on an actual server spawn (once per language/session).
    pub fn lsp_log_on_start(&self) -> bool {
        self.bool_or("lsp-log-on-start", false)
    }

    /// Whether Macros checks macros.dev for a newer release at launch (once a
    /// day) and, on macOS, stages it in the background so a "restart to update"
    /// notice can offer a one-click relaunch. On by default; the Scheme
    /// `(set-auto-update #f)` flips this off (no network check, no notice).
    pub fn auto_update(&self) -> bool {
        self.bool_or("auto-update", true)
    }

    /// Width (px) of the line dividing split windows. Also its drag-to-resize
    /// hit area, so bump it if the divider is hard to grab with the mouse. The
    /// 1px minimum is enforced when the option is set.
    pub fn divider_width(&self) -> f32 {
        self.f32_or("divider-width", 1.0)
    }

    /// Helm matching: fuzzy (nucleo, fzf-style ranking) vs plain substring.
    pub fn helm_fuzzy(&self) -> bool {
        self.bool_or("helm-fuzzy", true)
    }

    /// Auto-revert (Emacs `global-auto-revert-mode`): when a file-visiting buffer
    /// with no unsaved edits changes on disk, re-read it automatically. Off by
    /// default — enabled with `(set-option "auto-revert" #t)`. Buffers with unsaved
    /// changes are never clobbered (a warning is echoed instead). Independent of the
    /// always-on dired-sidebar/dired directory refresh, which watches dirs, not buffers.
    pub fn auto_revert(&self) -> bool {
        self.bool_or("auto-revert", false)
    }

    /// Bevel edge width (px) for one side, default 1 (minimum 0 enforced on set).
    fn helm_bevel(&self, side: &str) -> f32 {
        self.f32_or(&format!("helm-bevel-{side}"), 1.0)
    }
    pub fn helm_bevel_top(&self) -> f32 {
        self.helm_bevel("top")
    }
    pub fn helm_bevel_right(&self) -> f32 {
        self.helm_bevel("right")
    }
    pub fn helm_bevel_bottom(&self) -> f32 {
        self.helm_bevel("bottom")
    }
    pub fn helm_bevel_left(&self) -> f32 {
        self.helm_bevel("left")
    }
}

#[cfg(test)]
mod tests {
    use super::{KNOWN_OPTIONS, OptionValue, Settings, option_kind};

    /// Every advertised option must actually be a known Rust option — guards the
    /// `KNOWN_OPTIONS` list from drifting out of sync with `option_kind`.
    #[test]
    fn known_options_have_a_kind() {
        for key in KNOWN_OPTIONS {
            assert!(
                option_kind(key).is_some(),
                "{key} is in KNOWN_OPTIONS but has no option_kind arm"
            );
        }
        // No accidental duplicates.
        let mut seen = std::collections::HashSet::new();
        for key in KNOWN_OPTIONS {
            assert!(seen.insert(*key), "{key} listed twice in KNOWN_OPTIONS");
        }
    }

    #[test]
    fn show_paren_defaults_per_language_and_overrides() {
        let mut s = Settings::default();
        // On for code, off for prose/data buffers and plain text.
        assert!(s.show_paren_for(Some("rust")));
        assert!(s.show_paren_for(Some("python")));
        assert!(!s.show_paren_for(Some("markdown")));
        assert!(!s.show_paren_for(Some("toml")));
        assert!(!s.show_paren_for(Some("fundamental")));
        assert!(!s.show_paren_for(None));
        // A per-mode override wins over the per-language default…
        s.set("show-paren:markdown".into(), OptionValue::Bool(true));
        assert!(s.show_paren_for(Some("markdown")));
        // …and a global override applies where no per-mode value is set.
        s.set("show-paren".into(), OptionValue::Bool(false));
        assert!(!s.show_paren_for(Some("rust")));
        assert!(s.show_paren_for(Some("markdown"))); // per-mode still wins
    }

    #[test]
    fn coerces_native_and_string_values() {
        let mut s = Settings::default();
        // String form (back-compat) and native numbers both land as the float.
        s.set("font-size".into(), OptionValue::Str("13".into()));
        assert_eq!(s.font_size(), 13.0);
        s.set("font-size".into(), OptionValue::Int(16));
        assert_eq!(s.font_size(), 16.0);
        // Booleans from a native #t/#f and from a "true"/"false" string.
        s.set("completion-auto".into(), OptionValue::Bool(false));
        assert!(!s.completion_auto());
        s.set("completion-auto".into(), OptionValue::Str("true".into()));
        assert!(s.completion_auto());
        // String-typed option.
        s.set("cursor-style".into(), OptionValue::Str("bar".into()));
        assert_eq!(s.cursor_style(), "bar");
    }

    #[test]
    fn clamps_at_write_time() {
        let mut s = Settings::default();
        s.set("helm-height".into(), OptionValue::Float(2.0)); // above the 0.95 max
        assert_eq!(s.helm_height(), 0.95);
        s.set("completion-max-rows".into(), OptionValue::Int(0)); // below the 1 min
        assert_eq!(s.completion_max_rows(), 1);
    }

    #[test]
    fn per_mode_override_and_uncoercible_is_ignored() {
        let mut s = Settings::default();
        // A `base:mode` value overrides only that mode; others keep the default.
        s.set(
            "line-numbers:dired-sidebar-mode".into(),
            OptionValue::Bool(false),
        );
        assert!(!s.line_numbers_for(Some("dired-sidebar-mode")));
        assert!(s.line_numbers_for(Some("rust")));
        // An un-coercible value leaves the previous value intact.
        s.set("font-size".into(), OptionValue::Int(16));
        s.set("font-size".into(), OptionValue::Str("abc".into()));
        assert_eq!(s.font_size(), 16.0);
    }
}
