//! ripgrep-as-a-library file walking behind the `walk-files` / `grep-files`
//! primitives: the Scheme-tunable walk options and the walker built from them.

/// Scheme-configurable options for the ripgrep-library search primitives
/// (`walk-files` / `grep-files`). Lives on the engine thread, shared among the
/// search register_fns. Defaults match ripgrep's sensible behaviour
/// (gitignore-aware, skip hidden).
#[derive(Clone)]
pub(crate) struct SearchConfig {
    /// Extra exclude globs, e.g. `"target"`, `"*.lock"` (added as `!glob`).
    pub(crate) excludes: Vec<String>,
    /// Whitelist globs — if non-empty, only matching files are walked.
    pub(crate) includes: Vec<String>,
    pub(crate) hidden: bool,
    pub(crate) follow_links: bool,
    pub(crate) respect_gitignore: bool,
    pub(crate) max_depth: Option<usize>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            excludes: Vec::new(),
            includes: Vec::new(),
            hidden: false,
            follow_links: false,
            respect_gitignore: true,
            max_depth: None,
        }
    }
}

/// Build ripgrep's gitignore-aware walker for `dir` under the current config.
pub(crate) fn build_walk(dir: &str, config: &SearchConfig) -> ignore::Walk {
    let mut wb = ignore::WalkBuilder::new(dir);
    wb.hidden(!config.hidden)
        .git_ignore(config.respect_gitignore)
        .git_global(config.respect_gitignore)
        .git_exclude(config.respect_gitignore)
        .ignore(config.respect_gitignore)
        .follow_links(config.follow_links)
        .max_depth(config.max_depth);
    if !config.includes.is_empty() || !config.excludes.is_empty() {
        let mut ob = ignore::overrides::OverrideBuilder::new(dir);
        for inc in &config.includes {
            let _ = ob.add(inc);
        }
        for exc in &config.excludes {
            let _ = ob.add(&format!("!{exc}"));
        }
        if let Ok(ov) = ob.build() {
            wb.overrides(ov);
        }
    }
    wb.build()
}
