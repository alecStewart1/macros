//! gix-backed git reads: pure-Rust queries (no `git` subprocess), blocking
//! on the engine thread.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, _ctx: &EngineShared) {
    reg.section(Section::Git);
    // gix bridge: pure-Rust git queries (no `git` subprocess), exposed the same
    // blocking-on-the-engine-thread way as `walk-files`/`grep-files`. Errors
    // (not a repo, unborn HEAD, untracked file) collapse to an empty list.
    {
        // `(gix-toplevel dir)` → absolute work-tree root for the repo enclosing
        // `dir`, or "" if `dir` isn't in a git work tree. The native form of
        // `git rev-parse --show-toplevel` behind magit's `magit-toplevel`.
        reg.register_fn("gix-toplevel", |dir: String| -> String {
            ::gix::discover(&dir)
                .ok()
                .and_then(|repo| repo.workdir().map(|p| p.to_string_lossy().into_owned()))
                .unwrap_or_default()
        });
        // `(gix-branch dir)` → short name of the checked-out branch ("main"), or
        // "" outside a repo / with a detached HEAD. `git rev-parse --abbrev-ref HEAD`.
        reg.register_fn("gix-branch", |dir: String| -> String { gix_branch(&dir) });
        // `(gix-log dir n)` → up to `n` recent HEAD commits, newest first, each
        // "shorthash\tauthor\tYYYY-MM-DD\tsummary".
        reg.register_fn(
            "gix-log",
            |dir: String, NonNeg(max): NonNeg| -> Vec<String> {
                gix_log(&dir, max).unwrap_or_default()
            },
        );
        // `(gix-blame dir file)` → one "line\tsha\tauthor\tepoch\tsummary" per
        // line of `file` (absolute or relative to `dir`) as committed at HEAD —
        // or an empty list when the file differs from HEAD (caller falls back to
        // `git blame`; see gix_blame's alignment guard).
        reg.register_fn("gix-blame", |dir: String, file: String| -> Vec<String> {
            gix_blame(&dir, &file).unwrap_or_default()
        });
        // `(gix-blame-text dir file)` → `git blame --date=short`-format text for
        // `file` at HEAD, or "" when it differs from HEAD (caller falls back to git).
        reg.register_fn("gix-blame-text", |dir: String, file: String| -> String {
            gix_blame_text(&dir, &file).unwrap_or_default()
        });
        // `(gix-git-dir dir)` → absolute git dir (`git rev-parse --absolute-git-dir`),
        // or "" outside a repo. Linked worktrees get their private git dir.
        reg.register_fn("gix-git-dir", |dir: String| -> String { gix_git_dir(&dir) });
        // `(gix-branches dir)` / `(gix-remote-branches dir)` / `(gix-tags dir)`
        // → sorted short ref names, the native form of `git branch [-r]` / `git tag`.
        reg.register_fn("gix-branches", |dir: String| -> Vec<String> {
            gix_ref_names(&dir, RefKind::Local).unwrap_or_default()
        });
        reg.register_fn("gix-remote-branches", |dir: String| -> Vec<String> {
            gix_ref_names(&dir, RefKind::Remote).unwrap_or_default()
        });
        reg.register_fn("gix-tags", |dir: String| -> Vec<String> {
            gix_ref_names(&dir, RefKind::Tag).unwrap_or_default()
        });
        // `(gix-remotes dir)` → configured remote names.
        reg.register_fn("gix-remotes", |dir: String| -> Vec<String> {
            gix_remotes(&dir).unwrap_or_default()
        });
        // `(gix-state dir)` → "merge" / "rebase" / "" (in-progress operation).
        reg.register_fn("gix-state", |dir: String| -> String { gix_state(&dir) });
        // `(gix-head-subject dir)` / `(gix-head-message dir)` → HEAD commit %s / %B.
        reg.register_fn("gix-head-subject", |dir: String| -> String {
            gix_head_message(&dir, false)
        });
        reg.register_fn("gix-head-message", |dir: String| -> String {
            gix_head_message(&dir, true)
        });
        // `(gix-log-oneline dir n)` → up to n "<shorthash> <summary>" lines.
        reg.register_fn(
            "gix-log-oneline",
            |dir: String, NonNeg(max): NonNeg| -> Vec<String> {
                gix_log_oneline(&dir, max).unwrap_or_default()
            },
        );
        // `(gix-diff dir)` / `(gix-diff-cached dir)` → `git diff` / `git diff
        // --cached`-format text (unstaged: index↔worktree; staged: HEAD↔index).
        reg.register_fn("gix-diff", |dir: String| -> String {
            gix_diff_text(&dir, false).unwrap_or_default()
        });
        reg.register_fn("gix-diff-cached", |dir: String| -> String {
            gix_diff_text(&dir, true).unwrap_or_default()
        });
        // `(gix-show dir rev)` → `git show <rev>`-format commit view (header + diff).
        reg.register_fn("gix-show", |dir: String, rev: String| -> String {
            gix_show(&dir, &rev).unwrap_or_default()
        });
        // `(gix-log-decorated dir all? n)` → `git log --oneline --decorate [--all]`.
        reg.register_fn(
            "gix-log-decorated",
            |dir: String, all: bool, NonNeg(max): NonNeg| -> Vec<String> {
                gix_log_decorated(&dir, all, max).unwrap_or_default()
            },
        );
        // `(gix-log-range dir tip hidden n)` → `git log --oneline hidden..tip`.
        reg.register_fn(
            "gix-log-range",
            |dir: String, tip: String, hidden: String, NonNeg(max): NonNeg| -> Vec<String> {
                gix_log_range(&dir, &tip, &hidden, max).unwrap_or_default()
            },
        );
        // `(gix-stash-list dir)` → `git stash list`-format lines (refs/stash reflog).
        reg.register_fn("gix-stash-list", |dir: String| -> Vec<String> {
            gix_stash_list(&dir).unwrap_or_default()
        });
        // `(gix-log-file dir file n)` → `git log --oneline --decorate -n N -- file`.
        reg.register_fn(
            "gix-log-file",
            |dir: String, file: String, NonNeg(max): NonNeg| -> Vec<String> {
                gix_log_file(&dir, &file, max).unwrap_or_default()
            },
        );
        // `(gix-status dir)` → `git status --porcelain --branch --untracked-files=all`
        // lines (header first). Renames read as delete+add (`--no-renames`).
        reg.register_fn("gix-status", |dir: String| -> Vec<String> {
            gix_status(&dir).unwrap_or_default()
        });
    }
}
