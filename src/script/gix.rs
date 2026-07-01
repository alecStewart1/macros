use std::collections::HashMap;

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

/// One commit's metadata row, shared by `gix-log` and `gix-blame`:
/// `"<shorthash>\t<author>\t<YYYY-MM-DD>\t<summary>"`. Tab-separated because
/// author names and commit summaries contain spaces and colons, so the
/// `path:line:text` colon convention `grep-files` uses wouldn't round-trip. The
/// date comes from `chrono` (already a dep) so we don't depend on gix-date's
/// formatter module being public.
pub(crate) fn gix_commit_row(
    repo: &gix::Repository,
    id: &gix::ObjectId,
) -> Result<String, Box<dyn std::error::Error>> {
    let commit = repo.find_commit(*id)?;
    let short = id.to_hex_with_len(8).to_string();
    let author = commit.author()?;
    let name = author.name.to_string();
    let date = chrono::DateTime::from_timestamp(author.seconds(), 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let summary = commit.message()?.summary().to_string();
    Ok(format!("{short}\t{name}\t{date}\t{summary}"))
}

/// One `git log --oneline`-style row: `"<short7>[ (<decorations>)] <summary>"`
/// — 7-char abbreviation matching git's default. Shared by the oneline /
/// decorated / range / file log backends; `decos` is the [`ref_decorations`]
/// map for the decorated variants (`None` = undecorated).
pub(crate) fn gix_oneline_row(
    repo: &gix::Repository,
    id: gix::ObjectId,
    decos: Option<&HashMap<gix::ObjectId, Vec<String>>>,
) -> Result<String, Box<dyn std::error::Error>> {
    let summary = repo.find_commit(id)?.message()?.summary().to_string();
    let deco = decos
        .and_then(|d| d.get(&id))
        .map(|v| format!(" ({})", v.join(", ")))
        .unwrap_or_default();
    Ok(format!("{}{} {}", id.to_hex_with_len(7), deco, summary))
}

/// `(gix-log dir n)` backend: up to `n` commits reachable from HEAD, newest
/// first, each a commit row. Errors (not a repo, unborn HEAD) surface to Scheme
/// as an empty list, the same way `walk-files` yields nothing for a missing dir.
pub(crate) fn gix_log(dir: &str, max: usize) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repo = gix::discover(dir)?;
    let head = repo.head_id()?;
    let mut rows = Vec::new();
    for info in repo.rev_walk(Some(head.detach())).all()?.take(max) {
        rows.push(gix_commit_row(&repo, &info?.id)?);
    }
    Ok(rows)
}

/// `(gix-blame dir file)` backend: one row per line of `file` (absolute or
/// relative to `dir`) as committed at HEAD, in file order:
/// `"<1-based-line>\t<sha>\t<author>\t<author-epoch>\t<summary>"` (full 40-hex
/// SHA, author time as Unix seconds — the fields magit's blame UI needs).
///
/// Alignment guard: gix blames HEAD's committed blob, but the buffer shows the
/// *working-tree* file. The two only line up row-for-row when they're
/// byte-identical, so when the on-disk file differs from HEAD (uncommitted
/// edits) — or isn't tracked at HEAD — we return an empty list. magit treats
/// that as "gix can't" and falls back to `git blame`, which blames the working
/// tree (and marks uncommitted lines). This is the "gix where possible, git
/// otherwise" boundary.
pub(crate) fn gix_blame(dir: &str, file: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use gix::bstr::BStr;
    let repo = gix::discover(dir)?;
    // Resolve `file` (absolute, or relative to `dir`) to a repo-root-relative
    // path, the way `git blame -C dir -- file` does — callers (e.g. magit) pass
    // the buffer's path, not a path already relative to the repo root.
    let abs = {
        let p = std::path::Path::new(file);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::path::Path::new(dir).join(p)
        }
    };
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
    let rel = match repo.workdir().and_then(|wd| std::fs::canonicalize(wd).ok()) {
        Some(wd) => abs
            .strip_prefix(&wd)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| abs.clone()),
        None => abs.clone(),
    };

    // Bail to git unless the working-tree file matches HEAD's blob byte-for-byte.
    let head_tree = repo.head_tree()?;
    let Some(entry) = head_tree.lookup_entry_by_path(&rel)? else {
        return Ok(Vec::new()); // not tracked at HEAD → let git handle it
    };
    if entry.object()?.data != std::fs::read(&abs)? {
        return Ok(Vec::new()); // working tree differs from HEAD → fall back to git
    }

    let rel = rel.to_string_lossy();
    let head = repo.head_id()?.detach();
    let opts = gix::repository::blame_file::Options::default();
    let outcome = repo.blame_file(BStr::new(rel.as_bytes()), head, opts)?;

    // Resolve each commit's "sha\tauthor\tepoch\tsummary" once — adjacent blame
    // hunks often share one.
    let mut cache: HashMap<gix::ObjectId, String> = HashMap::new();
    let mut entries = outcome.entries;
    entries.sort_by_key(|e| e.start_in_blamed_file);
    let mut rows = Vec::new();
    for entry in entries {
        let meta = match cache.get(&entry.commit_id) {
            Some(m) => m.clone(),
            None => {
                let commit = repo.find_commit(entry.commit_id)?;
                let author = commit.author()?;
                let m = format!(
                    "{}\t{}\t{}\t{}",
                    entry.commit_id.to_hex(),
                    author.name,
                    author.seconds(),
                    commit.message()?.summary()
                );
                cache.insert(entry.commit_id, m.clone());
                m
            }
        };
        for i in 0..entry.len.get() {
            // `start_in_blamed_file` is 0-based; editor lines are 1-based.
            rows.push(format!("{}\t{}", entry.start_in_blamed_file + i + 1, meta));
        }
    }
    Ok(rows)
}

/// Which ref category [`gix_ref_names`] lists — the `git branch` /
/// `git branch -r` / `git tag` split.
#[derive(Clone, Copy)]
pub(crate) enum RefKind {
    Local,
    Remote,
    Tag,
}

/// Sorted short ref names for one category — backs `gix-branches` (local
/// branches), `gix-remote-branches` (`origin/main`…), and `gix-tags`. `kind`
/// picks the iterator the way `git branch` / `git branch -r` / `git tag` do.
pub(crate) fn gix_ref_names(
    dir: &str,
    kind: RefKind,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repo = gix::discover(dir)?;
    let platform = repo.references()?;
    let iter = match kind {
        RefKind::Local => platform.local_branches()?,
        RefKind::Remote => platform.remote_branches()?,
        RefKind::Tag => platform.tags()?,
    };
    let mut names: Vec<String> = iter
        .filter_map(|r| r.ok())
        .map(|r| r.name().shorten().to_string())
        .collect();
    names.sort();
    Ok(names)
}

/// `(gix-remotes dir)` backend: configured remote names (`origin`…), sorted.
pub(crate) fn gix_remotes(dir: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repo = gix::discover(dir)?;
    Ok(repo.remote_names().iter().map(|n| n.to_string()).collect())
}

/// `(gix-state dir)` backend: `"merge"` while a merge is in progress (MERGE_HEAD
/// present), `"rebase"` during a rebase (the rebase-merge/rebase-apply state
/// dirs), else `""`. Replaces magit's `rev-parse --verify MERGE_HEAD/REBASE_HEAD`
/// probes — git records this state on disk in the git dir, so we read it there.
pub(crate) fn gix_state(dir: &str) -> String {
    let Ok(repo) = gix::discover(dir) else {
        return String::new();
    };
    let git_dir = repo.git_dir();
    if git_dir.join("MERGE_HEAD").exists() {
        "merge".into()
    } else if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        "rebase".into()
    } else {
        String::new()
    }
}

/// `(gix-head-message dir full?)` backend: the HEAD commit's summary (`full` =
/// false → `git log -1 --format=%s`) or full message (`full` = true → `%B`).
/// Empty string when there are no commits yet.
pub(crate) fn gix_head_message(dir: &str, full: bool) -> String {
    let inner = || -> Result<String, Box<dyn std::error::Error>> {
        let repo = gix::discover(dir)?;
        let id = repo.head_id()?.detach();
        let commit = repo.find_commit(id)?;
        if full {
            Ok(commit.message_raw()?.to_string())
        } else {
            Ok(commit.message()?.summary().to_string())
        }
    };
    inner().unwrap_or_default()
}

/// `(gix-log-oneline dir n)` backend: up to `n` commits from HEAD, newest first,
/// each `"<shorthash> <summary>"` — the native form of `git log --oneline -n N`.
/// 7-char abbreviation matches git's default; magit accents the first token as
/// the hash and uses it as a `git show` ref.
pub(crate) fn gix_log_oneline(
    dir: &str,
    max: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repo = gix::discover(dir)?;
    let head = repo.head_id()?;
    let mut rows = Vec::new();
    for info in repo.rev_walk(Some(head.detach())).all()?.take(max) {
        rows.push(gix_oneline_row(&repo, info?.id, None)?);
    }
    Ok(rows)
}

// --- gix diff generation (git-format unified diffs without a subprocess) -----
// Backs `gix-diff` (index↔worktree) and `gix-diff-cached` (HEAD↔index). The
// output is `git diff`-format text so magit's `build-diff-map`/`parse-diff`
// parse it unchanged AND it round-trips through `git apply --cached` (hunk
// staging reuses the same text) — see the `gix_diff` round-trip test.

/// Split `data` into lines that retain their trailing newline (the final line
/// may lack one) — the exact tokenization gix-imara-diff uses, so our line
/// indices line up with the hunk ranges it returns.
pub(crate) fn diff_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let n = rest
            .iter()
            .position(|&b| b == b'\n')
            .map_or(rest.len(), |i| i + 1);
        let (line, r) = rest.split_at(n);
        out.push(line);
        rest = r;
    }
    out
}

/// Emit one hunk line: `prefix` + content, adding git's
/// `\ No newline at end of file` marker when the source line has no terminator
/// (imara's printer omits it — the round-trip footgun).
pub(crate) fn diff_emit_line(out: &mut String, prefix: char, line: &[u8]) {
    out.push(prefix);
    out.push_str(&String::from_utf8_lossy(line));
    if !line.ends_with(b"\n") {
        out.push('\n');
        out.push_str("\\ No newline at end of file\n");
    }
}

/// A unified `@@` range field: git omits `,1` and writes `start,0` for an empty
/// side (pure insertion/deletion).
pub(crate) fn diff_hunk_range(start0: u32, len: u32) -> String {
    if len == 1 {
        format!("{}", start0 + 1)
    } else if len == 0 {
        format!("{},0", start0)
    } else {
        format!("{},{}", start0 + 1, len)
    }
}

/// The unified hunks (everything after the file header) for `old` → `new`, or
/// `None` when identical. Context = 3, hunks within 2*context merged — matching
/// `git diff` closely enough for both display and `git apply`.
pub(crate) fn diff_hunks(old: &[u8], new: &[u8]) -> Option<String> {
    use gix::diff::blob::{Algorithm, Diff, InternedInput};
    let input = InternedInput::new(old, new);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let hunks: Vec<gix::diff::blob::Hunk> = diff.hunks().collect();
    if hunks.is_empty() {
        return None;
    }
    let old_lines = diff_lines(old);
    let new_lines = diff_lines(new);
    let ctx = 3u32;
    let mut out = String::new();
    let mut i = 0;
    while i < hunks.len() {
        // Merge hunks whose gap is within 2*context into one displayed hunk.
        let mut j = i;
        while j + 1 < hunks.len()
            && hunks[j + 1]
                .before
                .start
                .saturating_sub(hunks[j].before.end)
                <= 2 * ctx
        {
            j += 1;
        }
        let bs = hunks[i].before.start.saturating_sub(ctx);
        let be = (hunks[j].before.end + ctx).min(old_lines.len() as u32);
        let a0 = hunks[i].after.start.saturating_sub(ctx);
        let ae = (hunks[j].after.end + ctx).min(new_lines.len() as u32);
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            diff_hunk_range(bs, be - bs),
            diff_hunk_range(a0, ae - a0)
        ));
        let mut bpos = bs;
        for h in &hunks[i..=j] {
            for k in bpos..h.before.start {
                diff_emit_line(&mut out, ' ', old_lines[k as usize]);
            }
            for k in h.before.start..h.before.end {
                diff_emit_line(&mut out, '-', old_lines[k as usize]);
            }
            for k in h.after.start..h.after.end {
                diff_emit_line(&mut out, '+', new_lines[k as usize]);
            }
            bpos = h.before.end;
        }
        for k in bpos..be {
            diff_emit_line(&mut out, ' ', old_lines[k as usize]);
        }
        i = j + 1;
    }
    Some(out)
}

/// git's octal mode string for a committed-tree entry.
pub(crate) fn tree_mode_str(mode: gix::objs::tree::EntryMode) -> &'static str {
    use gix::objs::tree::EntryKind;
    match mode.kind() {
        EntryKind::BlobExecutable => "100755",
        EntryKind::Link => "120000",
        EntryKind::Commit => "160000",
        _ => "100644",
    }
}

/// git's octal mode string for an index entry.
pub(crate) fn index_mode_str(mode: gix::index::entry::Mode) -> &'static str {
    use gix::index::entry::Mode;
    if mode.contains(Mode::SYMLINK) {
        "120000"
    } else if mode.contains(Mode::FILE_EXECUTABLE) {
        "100755"
    } else if mode.contains(Mode::COMMIT) {
        "160000"
    } else {
        "100644"
    }
}

/// git's binary heuristic: a NUL byte in the first 8000 bytes. Running the line
/// differ over binary content would dump raw bytes into the diff (garbling the
/// magit buffer and breaking its file/hunk parsing), so we detect it first.
pub(crate) fn diff_is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

/// `diff --git`-format text for one changed file. `old`/`new` are `None` for an
/// addition/deletion respectively; the `&str` is that side's git mode.
pub(crate) fn diff_one_file(
    path: &str,
    old: Option<(&[u8], &str)>,
    new: Option<(&[u8], &str)>,
) -> String {
    let empty: &[u8] = &[];
    let oc = old.map_or(empty, |(c, _)| c);
    let nc = new.map_or(empty, |(c, _)| c);

    // Binary files: emit `Binary files … differ` with no body, exactly like git —
    // no content, so nothing to garble or to fail to parse.
    if diff_is_binary(oc) || diff_is_binary(nc) {
        let mut out = format!("diff --git a/{path} b/{path}\n");
        match (old, new) {
            (None, Some((_, mode))) => out.push_str(&format!(
                "new file mode {mode}\nBinary files /dev/null and b/{path} differ\n"
            )),
            (Some((_, mode)), None) => out.push_str(&format!(
                "deleted file mode {mode}\nBinary files a/{path} and /dev/null differ\n"
            )),
            _ => out.push_str(&format!("Binary files a/{path} and b/{path} differ\n")),
        }
        return out;
    }

    let Some(hunks) = diff_hunks(oc, nc) else {
        return String::new(); // identical → nothing (callers pre-filter, but be safe)
    };
    let mut out = format!("diff --git a/{path} b/{path}\n");
    match (old, new) {
        (None, Some((_, mode))) => {
            out.push_str(&format!(
                "new file mode {mode}\n--- /dev/null\n+++ b/{path}\n"
            ));
        }
        (Some((_, mode)), None) => {
            out.push_str(&format!(
                "deleted file mode {mode}\n--- a/{path}\n+++ /dev/null\n"
            ));
        }
        _ => out.push_str(&format!("--- a/{path}\n+++ b/{path}\n")),
    }
    out.push_str(&hunks);
    out
}

/// A path's blob id + git mode on one side of a diff (a tree or the index).
pub(crate) type GixSide = (gix::ObjectId, &'static str);

/// Flatten a tree to `path → (blob id, mode)` for every file it contains.
/// `.files()` records directories (trees) and submodules (commits) too; we keep
/// only blobs/symlinks, else a "deleted directory" would diff the raw tree
/// object — bytes that garble the buffer and break magit's parser.
pub(crate) fn tree_side_map(
    tree: &gix::Tree<'_>,
) -> Result<std::collections::BTreeMap<Vec<u8>, GixSide>, Box<dyn std::error::Error>> {
    Ok(tree
        .traverse()
        .breadthfirst
        .files()?
        .into_iter()
        .filter(|e| e.mode.is_blob_or_symlink())
        .map(|e| (Vec::from(e.filepath), (e.oid, tree_mode_str(e.mode))))
        .collect())
}

/// `git diff`-format text between two `path → (id, mode)` sides (tree↔tree or
/// tree↔index). Shared by `gix-diff-cached` and `gix-show`.
pub(crate) fn diff_side_maps(
    repo: &gix::Repository,
    old: &std::collections::BTreeMap<Vec<u8>, GixSide>,
    new: &std::collections::BTreeMap<Vec<u8>, GixSide>,
) -> Result<String, Box<dyn std::error::Error>> {
    let blob = |id: gix::ObjectId| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        Ok(repo.find_object(id)?.data.clone())
    };
    let mut keys: Vec<&Vec<u8>> = old.keys().chain(new.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = String::new();
    for k in keys {
        let path = String::from_utf8_lossy(k).into_owned();
        match (old.get(k), new.get(k)) {
            (Some((oo, om)), Some((no, nm))) if oo != no => {
                let (o, n) = (blob(*oo)?, blob(*no)?);
                out.push_str(&diff_one_file(&path, Some((&o, om)), Some((&n, nm))));
            }
            (None, Some((no, nm))) => {
                let n = blob(*no)?;
                out.push_str(&diff_one_file(&path, None, Some((&n, nm))));
            }
            (Some((oo, om)), None) => {
                let o = blob(*oo)?;
                out.push_str(&diff_one_file(&path, Some((&o, om)), None));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// `(gix-diff dir)` / `(gix-diff-cached dir)` backend. `cached` = false diffs the
/// index against the working tree (`git diff`); `cached` = true diffs HEAD
/// against the index (`git diff --cached`). Untracked files are excluded, as git
/// does. Errors (bare repo, etc.) surface as "" → an empty section in magit.
pub(crate) fn gix_diff_text(dir: &str, cached: bool) -> Result<String, Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;
    let repo = gix::discover(dir)?;
    let index = repo.open_index()?;
    let blob = |id: gix::ObjectId| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        Ok(repo.find_object(id)?.data.clone())
    };
    let mut out = String::new();

    if cached {
        // HEAD tree vs index. No HEAD yet (unborn) → empty tree, so every staged
        // path reads as an addition (initial commit).
        let head = match repo.head_tree() {
            Ok(t) => tree_side_map(&t)?,
            Err(_) => BTreeMap::new(),
        };
        let mut idx: BTreeMap<Vec<u8>, GixSide> = BTreeMap::new();
        for e in index.entries() {
            if e.stage() != gix::index::entry::Stage::Unconflicted {
                continue;
            }
            idx.insert(e.path(&index).to_vec(), (e.id, index_mode_str(e.mode)));
        }
        out.push_str(&diff_side_maps(&repo, &head, &idx)?);
    } else {
        // index vs working tree.
        let workdir = repo.workdir().ok_or("bare repository")?.to_path_buf();
        for e in index.entries() {
            if e.stage() != gix::index::entry::Stage::Unconflicted {
                continue;
            }
            let path = String::from_utf8_lossy(e.path(&index)).into_owned();
            let mode = index_mode_str(e.mode);
            let file = workdir.join(&path);
            if !file.exists() {
                let o = blob(e.id)?;
                out.push_str(&diff_one_file(&path, Some((&o, mode)), None));
            } else {
                let disk = std::fs::read(&file)?;
                let o = blob(e.id)?;
                if disk != o {
                    out.push_str(&diff_one_file(&path, Some((&o, mode)), Some((&disk, mode))));
                }
            }
        }
    }
    Ok(out)
}

/// `(gix-show dir rev)` backend: `git show <rev>`-format text — the commit
/// header (`commit`/`Merge`/`Author`/`Date`, message indented 4 spaces) followed
/// by the diff against its first parent (or the empty tree for a root commit).
/// Display-only; magit colorizes it by line prefix.
pub(crate) fn gix_show(dir: &str, rev: &str) -> Result<String, Box<dyn std::error::Error>> {
    use gix::bstr::BStr;
    use std::collections::BTreeMap;
    let repo = gix::discover(dir)?;
    let id = repo.rev_parse_single(BStr::new(rev))?.detach();
    let commit = repo.find_commit(id)?;
    let parents: Vec<gix::ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();

    let mut out = format!("commit {}\n", id.to_hex());
    if parents.len() > 1 {
        let shorts: Vec<String> = parents
            .iter()
            .map(|p| p.to_hex_with_len(7).to_string())
            .collect();
        out.push_str(&format!("Merge: {}\n", shorts.join(" ")));
    }
    let author = commit.author()?;
    out.push_str(&format!("Author: {} <{}>\n", author.name, author.email));
    let t = author.time()?;
    let date = chrono::DateTime::from_timestamp(t.seconds, 0)
        .and_then(|dt| {
            chrono::FixedOffset::east_opt(t.offset).map(|off| {
                dt.with_timezone(&off)
                    .format("%a %b %e %H:%M:%S %Y %z")
                    .to_string()
            })
        })
        .unwrap_or_default();
    out.push_str(&format!("Date:   {date}\n\n"));
    for line in commit.message_raw()?.to_string().lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');

    // Diff against the first parent (empty tree for a root commit).
    let new_map = tree_side_map(&commit.tree()?)?;
    let old_map = match parents.first() {
        Some(p) => tree_side_map(&repo.find_commit(*p)?.tree()?)?,
        None => BTreeMap::new(),
    };
    out.push_str(&diff_side_maps(&repo, &old_map, &new_map)?);
    Ok(out)
}

/// `oid → ["HEAD -> main", "tag: v1", "origin/main"]` for `git log --decorate`.
/// Tags get a `tag:` prefix; the checked-out branch gets `HEAD ->`. Built from
/// the ref-name prefix so no Category import is needed.
pub(crate) fn ref_decorations(
    repo: &gix::Repository,
) -> std::collections::HashMap<gix::ObjectId, Vec<String>> {
    let mut map: std::collections::HashMap<gix::ObjectId, Vec<String>> =
        std::collections::HashMap::new();
    let head_branch = repo
        .head_name()
        .ok()
        .flatten()
        .map(|n| n.shorten().to_string());
    if let Ok(platform) = repo.references()
        && let Ok(iter) = platform.all()
    {
        for r in iter.filter_map(Result::ok) {
            let full = r.name().as_bstr().to_string();
            let short = r.name().shorten().to_string();
            let deco = if let Some(t) = full.strip_prefix("refs/tags/") {
                format!("tag: {t}")
            } else if full.starts_with("refs/heads/") {
                if head_branch.as_deref() == Some(short.as_str()) {
                    format!("HEAD -> {short}")
                } else {
                    short
                }
            } else if full.starts_with("refs/remotes/") {
                short
            } else {
                continue; // HEAD, stash, notes, …
            };
            let mut r = r;
            if let Ok(id) = r.peel_to_id() {
                map.entry(id.detach()).or_default().push(deco);
            }
        }
    }
    map
}

/// `(gix-log-decorated dir all? n)`: up to `n` commits newest-first, each
/// `"<short> [(<decorations>)] <summary>"` — `git log --oneline --decorate
/// [--all]`. magit keeps git for the `--graph` variant.
pub(crate) fn gix_log_decorated(
    dir: &str,
    all: bool,
    max: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use gix::revision::walk::Sorting;
    use gix::traverse::commit::simple::CommitTimeOrder;
    let repo = gix::discover(dir)?;
    let decos = ref_decorations(&repo);
    let tips: Vec<gix::ObjectId> = if all {
        repo.references()?
            .all()?
            .filter_map(Result::ok)
            .filter_map(|mut r| r.peel_to_id().ok().map(|id| id.detach()))
            .collect()
    } else {
        vec![repo.head_id()?.detach()]
    };
    if tips.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    for info in repo
        .rev_walk(tips)
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()?
        .take(max)
    {
        rows.push(gix_oneline_row(&repo, info?.id, Some(&decos))?);
    }
    Ok(rows)
}

/// `(gix-log-range dir tip hidden n)`: up to `n` commits reachable from `tip`
/// but not `hidden`, newest-first, as `"<short> <summary>"` — `git log
/// --oneline hidden..tip`, used for the unpushed/unpulled sections.
pub(crate) fn gix_log_range(
    dir: &str,
    tip: &str,
    hidden: &str,
    max: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use gix::bstr::BStr;
    use gix::revision::walk::Sorting;
    use gix::traverse::commit::simple::CommitTimeOrder;
    let repo = gix::discover(dir)?;
    let tip_id = repo.rev_parse_single(BStr::new(tip))?.detach();
    let hidden_id = repo.rev_parse_single(BStr::new(hidden))?.detach();
    let mut rows = Vec::new();
    for info in repo
        .rev_walk(Some(tip_id))
        .with_hidden(Some(hidden_id))
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()?
        .take(max)
    {
        rows.push(gix_oneline_row(&repo, info?.id, None)?);
    }
    Ok(rows)
}

/// `(gix-log-file dir file n)`: up to `n` commits that changed `file` (path
/// relative to the repo root), newest-first, decorated — `git log --oneline
/// --decorate -n N -- <file>`. A commit "changed" the file when its blob differs
/// from the first parent's (including add/remove).
pub(crate) fn gix_log_file(
    dir: &str,
    file: &str,
    max: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use gix::revision::walk::Sorting;
    use gix::traverse::commit::simple::CommitTimeOrder;
    let repo = gix::discover(dir)?;
    let decos = ref_decorations(&repo);
    let path = std::path::Path::new(file);
    let blob_at = |commit: &gix::Commit<'_>| -> Option<gix::ObjectId> {
        commit
            .tree()
            .ok()?
            .lookup_entry_by_path(path)
            .ok()?
            .map(|e| e.id().detach())
    };
    let head = repo.head_id()?.detach();
    let mut rows = Vec::new();
    for info in repo
        .rev_walk(Some(head))
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()?
    {
        if rows.len() >= max {
            break;
        }
        let id = info?.id;
        let commit = repo.find_commit(id)?;
        let here = blob_at(&commit);
        let parent = match commit.parent_ids().next() {
            Some(p) => blob_at(&repo.find_commit(p.detach())?),
            None => None,
        };
        if here != parent {
            rows.push(gix_oneline_row(&repo, id, Some(&decos))?);
        }
    }
    Ok(rows)
}

/// `(gix-stash-list dir)`: `git stash list`-format lines, newest first
/// (`"stash@{0}: WIP on main: …"`). Stashes are entries in the `refs/stash`
/// reflog, whose message is exactly what git prints after the `stash@{N}:` tag.
pub(crate) fn gix_stash_list(dir: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repo = gix::discover(dir)?;
    let reference = match repo.find_reference("refs/stash") {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()), // no stashes
    };
    let mut platform = reference.log_iter();
    let mut msgs: Vec<String> = Vec::new();
    if let Some(iter) = platform.all()? {
        for line in iter {
            msgs.push(line?.message.to_string());
        }
    }
    // The reflog is oldest-first; `git stash list` is newest-first (stash@{0} =
    // most recent).
    Ok(msgs
        .into_iter()
        .rev()
        .enumerate()
        .map(|(i, msg)| format!("stash@{{{i}}}: {msg}"))
        .collect())
}

// --- gix status (porcelain v1 without a subprocess) --------------------------
// Backs `gix-status`, the last hot-path read magit had on `git`. Produces
// `git status --porcelain --branch --untracked-files=all`-format lines so
// magit's `magit-porcelain-full` parser is unchanged. Renames are NOT detected
// (a rename reads as a delete + add, i.e. matches `git status --no-renames`).

/// Count commits reachable from `tip` but not `base` — git's ahead/behind number.
pub(crate) fn gix_count_excl(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    base: gix::ObjectId,
) -> usize {
    repo.rev_walk(Some(tip))
        .with_hidden(Some(base))
        .all()
        .map(|w| w.filter_map(Result::ok).count())
        .unwrap_or(0)
}

/// The short name of the checked-out branch for the repo enclosing `dir`, e.g.
/// "main". Empty when `dir` isn't in a git work tree or HEAD is detached (no
/// branch). The native form of `git rev-parse --abbrev-ref HEAD`.
pub(crate) fn gix_branch(dir: &str) -> String {
    gix::discover(dir)
        .ok()
        .and_then(|repo| repo.head_name().ok().flatten())
        .map(|n| n.shorten().to_string())
        .unwrap_or_default()
}

/// The `## …` porcelain branch header: `## branch...upstream [ahead N, behind M]`,
/// or the `No commits yet` / `HEAD (no branch)` forms magit-parse-branch-line knows.
pub(crate) fn gix_status_branch_line(repo: &gix::Repository) -> String {
    use gix::bstr::BStr;
    let Some(head_name) = repo.head_name().ok().flatten() else {
        return "## HEAD (no branch)".to_string();
    };
    let branch = head_name.shorten().to_string();
    let head_id = match repo.head_id() {
        Ok(id) => id.detach(),
        Err(_) => return format!("## No commits yet on {branch}"),
    };
    let upstream = repo.head_ref().ok().flatten().and_then(|r| {
        r.remote_tracking_ref_name(gix::remote::Direction::Fetch)
            .and_then(|x| x.ok())
            .map(|n| n.shorten().to_string())
    });
    let Some(up) = upstream else {
        return format!("## {branch}");
    };
    let Some(up_id) = repo
        .rev_parse_single(BStr::new(up.as_bytes()))
        .ok()
        .map(|id| id.detach())
    else {
        return format!("## {branch}...{up} [gone]");
    };
    let (ahead, behind) = (
        gix_count_excl(repo, head_id, up_id),
        gix_count_excl(repo, up_id, head_id),
    );
    if ahead == 0 && behind == 0 {
        return format!("## {branch}...{up}");
    }
    let mut ab = Vec::new();
    if ahead > 0 {
        ab.push(format!("ahead {ahead}"));
    }
    if behind > 0 {
        ab.push(format!("behind {behind}"));
    }
    format!("## {branch}...{up} [{}]", ab.join(", "))
}

/// One porcelain status row: a `path` with its staged (`x`) and unstaged (`y`)
/// codes. `x`/`y` are `' '` (unchanged), `M`/`A`/`D`, or `?` (untracked).
pub(crate) struct StatusEntry {
    path: String,
    x: char,
    y: char,
}

/// `XY`-classified change rows for `gix-status`, sorted by path. X = index-vs-HEAD
/// (staged), Y = worktree-vs-index (unstaged), `??` = untracked.
pub(crate) fn gix_status_entries(
    repo: &gix::Repository,
) -> Result<Vec<StatusEntry>, Box<dyn std::error::Error>> {
    use std::collections::{BTreeMap, HashSet};
    let index = repo.open_index()?;
    let workdir = repo.workdir().ok_or("bare repository")?.to_path_buf();

    // index (stage 0) path→oid, plus every index path for the untracked test.
    let mut idx: BTreeMap<Vec<u8>, gix::ObjectId> = BTreeMap::new();
    let mut idx_paths: HashSet<Vec<u8>> = HashSet::new();
    for e in index.entries() {
        idx_paths.insert(e.path(&index).to_vec());
        if e.stage() == gix::index::entry::Stage::Unconflicted {
            idx.insert(e.path(&index).to_vec(), e.id);
        }
    }
    let head: BTreeMap<Vec<u8>, gix::ObjectId> = match repo.head_tree() {
        Ok(t) => t
            .traverse()
            .breadthfirst
            .files()?
            .into_iter()
            .filter(|e| e.mode.is_blob_or_symlink()) // drop tree/submodule entries
            .map(|e| (Vec::from(e.filepath), e.oid))
            .collect(),
        Err(_) => BTreeMap::new(),
    };

    let mut codes: BTreeMap<Vec<u8>, [char; 2]> = BTreeMap::new();

    // X — staged (HEAD tree vs index).
    let mut staged: Vec<&Vec<u8>> = head.keys().chain(idx.keys()).collect();
    staged.sort();
    staged.dedup();
    for k in staged {
        let x = match (head.get(k), idx.get(k)) {
            (None, Some(_)) => 'A',
            (Some(_), None) => 'D',
            (Some(h), Some(i)) if h != i => 'M',
            _ => continue,
        };
        codes.entry(k.clone()).or_insert([' ', ' '])[0] = x;
    }

    // Y — unstaged (index vs working tree). Skip reading content when size+mtime
    // match the index's stat cache, the way git does (so this stays cheap).
    for e in index.entries() {
        if e.stage() != gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let rel = e.path(&index).to_vec();
        let file = workdir.join(String::from_utf8_lossy(&rel).as_ref());
        let y = match std::fs::metadata(&file) {
            Err(_) => 'D', // gone from the worktree
            Ok(meta) => {
                let mtime_matches = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .is_some_and(|d| {
                        d.as_secs() as u32 == e.stat.mtime.secs
                            && d.subsec_nanos() == e.stat.mtime.nsecs
                    });
                if meta.len() == e.stat.size as u64 && mtime_matches {
                    continue; // unchanged per the stat cache
                }
                let disk = std::fs::read(&file).unwrap_or_default();
                if disk == repo.find_object(e.id)?.data.clone() {
                    continue; // touched but identical content
                }
                'M'
            }
        };
        codes.entry(rel).or_insert([' ', ' '])[1] = y;
    }

    // Untracked — gitignore-aware worktree walk; files absent from the index.
    let walker = ignore::WalkBuilder::new(&workdir)
        .hidden(false) // dotfiles are tracked by git
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .build();
    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.components().any(|c| c.as_os_str() == ".git") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(&workdir) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/").into_bytes();
        if !idx_paths.contains(&rel) && !head.contains_key(&rel) {
            codes.insert(rel, ['?', '?']);
        }
    }

    Ok(codes
        .into_iter()
        .map(|(p, [x, y])| StatusEntry {
            path: String::from_utf8_lossy(&p).into_owned(),
            x,
            y,
        })
        .collect())
}

/// `(gix-status dir)` backend: porcelain v1 lines, header first.
pub(crate) fn gix_status(dir: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repo = gix::discover(dir)?;
    let mut out = vec![gix_status_branch_line(&repo)];
    for e in gix_status_entries(&repo)? {
        out.push(format!("{}{} {}", e.x, e.y, e.path));
    }
    Ok(out)
}

/// Working-tree-vs-HEAD gutter marks (git-gutter / diff-hl) for `file`, diffing
/// the supplied `text` — the live buffer rope, so marks update before a save —
/// against the file's blob as committed at HEAD. Returns `(0-based line, mark)`
/// for changed lines only; an empty vec when `file` is outside a repo or its
/// parent can't be resolved.
///
/// A file not tracked at HEAD (brand new, or never committed) diffs against an
/// empty HEAD, so every line reads as `Added` — matching how diff-hl shows a new
/// file. Binary / non-UTF-8 HEAD blobs likewise fall back to "all added", which
/// is harmless for the text buffers this runs on.
pub(crate) fn git_diff_marks(
    file: &std::path::Path,
    text: &str,
) -> Vec<(usize, crate::editor::DiffMark)> {
    let Some(dir) = file.parent() else {
        return Vec::new();
    };
    let Ok(repo) = gix::discover(dir) else {
        return Vec::new();
    };
    // Resolve `file` to a repo-root-relative path the way `gix_blame` does.
    let abs = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let Some(wd) = repo.workdir().and_then(|wd| std::fs::canonicalize(wd).ok()) else {
        return Vec::new();
    };
    let Ok(rel) = abs.strip_prefix(&wd) else {
        return Vec::new();
    };
    // HEAD's blob for this path, or "" when it isn't tracked there (→ all added).
    let head_text = (|| -> Option<String> {
        let entry = repo.head_tree().ok()?.lookup_entry_by_path(rel).ok()??;
        String::from_utf8(entry.object().ok()?.data.clone()).ok()
    })()
    .unwrap_or_default();

    diff_line_marks(&head_text, text)
}

/// The pure core of [`git_diff_marks`]: a line-level diff of `head` → `text`
/// mapped to gutter marks. Insert → `Added`, Replace → `Modified`, Delete →
/// a single `Removed` on the line that now sits where the text was removed
/// (clamped to the last line for an end-of-file deletion). Split out so the
/// mapping is testable without a git repo.
pub(crate) fn diff_line_marks(head: &str, text: &str) -> Vec<(usize, crate::editor::DiffMark)> {
    use crate::editor::DiffMark;
    let new_total = text.lines().count();
    let diff = similar::TextDiff::from_lines(head, text);
    let mut marks = Vec::new();
    for op in diff.ops() {
        let (tag, _old, new) = op.as_tag_tuple();
        match tag {
            similar::DiffTag::Equal => {}
            similar::DiffTag::Insert => {
                marks.extend((new.start..new.end).map(|l| (l, DiffMark::Added)));
            }
            similar::DiffTag::Replace => {
                marks.extend((new.start..new.end).map(|l| (l, DiffMark::Modified)));
            }
            similar::DiffTag::Delete => {
                let line = new.start.min(new_total.saturating_sub(1));
                marks.push((line, DiffMark::Removed));
            }
        }
    }
    marks
}
