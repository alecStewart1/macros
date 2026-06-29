//! Runtime tree-sitter grammars: download a grammar repo, compile its generated
//! C parser to a shared library, `dlopen` it, and register it so the highlighter
//! can color a language macros was never built with.
//!
//! The model is Helix's, not Neovim's: we never run `tree-sitter generate` (which
//! would need Node). A published grammar commits its generated `src/parser.c`, so
//! [`compile`] just feeds that C — plus an optional hand-written `src/scanner.c`
//! / `scanner.cc` — to the host compiler and links a shared library. The user
//! only needs a C/C++ toolchain, which their build already requires (build.rs
//! compiles the vendored Mermaid parser the same way).
//!
//! The pipeline is three steps, each a small function so they're testable and the
//! errors are specific:
//!   1. [`fetch`]   — `git clone` the repo at a pinned rev into a cache dir.
//!   2. [`compile`] — `cc`/`c++` the committed parser into `<id>.{so,dylib}`.
//!   3. [`load`]    — `dlopen`, resolve `tree_sitter_<symbol>`, check the ABI.
//!
//! [`install`] runs all three and registers the result. It blocks (git + a C
//! compile take seconds), so the editor drives it on the background executor and
//! only touches the registry, which is process-global and thread-safe.
//!
//! ## Safety / trust
//! Loading a grammar runs arbitrary native code in-process — exactly like every
//! tree-sitter editor (nvim/helix/emacs). A buggy scanner can crash the editor; a
//! hostile repo is code execution. We pin the rev (so an upstream repo can't
//! silently change what we compile) and check the parser's ABI version before
//! use, but there's no sandbox. The dlopen'd library is intentionally leaked: a
//! [`LanguageFn`] points into its code pages, so it must outlive every parse —
//! and grammars stay loaded for the whole session anyway.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{OnceLock, RwLock};

use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

/// A grammar loaded at runtime: its parser entry point and highlights query, plus
/// the file extensions that should select it.
struct Installed {
    language_fn: LanguageFn,
    highlights: String,
    /// The grammar's `queries/indents.scm`, if it ships one — enables structural
    /// auto-indent. Optional: absent for grammars that provide only highlights.
    indents: Option<String>,
    extensions: Vec<String>,
}

/// What to install. The Scheme `install-language` wrapper fills the optional
/// fields with conventional defaults (symbol = id, queries/highlights.scm), so a
/// minimal call is just id + repo + rev + extensions.
pub struct InstallSpec {
    /// The language id (the mode name, the registry key — e.g. "nim").
    pub id: String,
    /// Git URL of the grammar repo.
    pub repo: String,
    /// A pinned commit / tag / branch. Recorded so a rebuild is reproducible.
    pub rev: String,
    /// File extensions (no dot) that select this language.
    pub extensions: Vec<String>,
    /// Subdirectory holding the grammar, for repos that nest it (e.g.
    /// tree-sitter-typescript's `typescript/`). `None` = repo root.
    pub subdir: Option<String>,
    /// The exported parser symbol minus the `tree_sitter_` prefix. Defaults to
    /// the id with hyphens turned to underscores (`my-lang` → `tree_sitter_my_lang`).
    pub symbol: Option<String>,
    /// Repo-relative path to the highlights query. Defaults to
    /// `queries/highlights.scm`.
    pub highlights_path: Option<String>,
}

/// Process-global registry of runtime-installed grammars, keyed by language id.
/// The install pipeline writes it from the background executor while the UI
/// thread reads it to build highlighters, so it's behind an `RwLock`. A
/// [`LanguageFn`] is just a function pointer (`Send + Sync`), and the library it
/// points into is leaked, so handing one out from here can never dangle.
fn registry() -> &'static RwLock<HashMap<String, Installed>> {
    static REG: OnceLock<RwLock<HashMap<String, Installed>>> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Intern a language id as `&'static str` so [`language_for_extension`] can hand
/// one back to `language_for_path`, whose callers expect a static id (the mode
/// name lives for the process). Installs are rare, so the small leak is fine;
/// re-installing the same id reuses the first leak.
fn intern(id: &str) -> &'static str {
    static IDS: OnceLock<RwLock<HashMap<String, &'static str>>> = OnceLock::new();
    let ids = IDS.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(s) = ids.read().unwrap().get(id) {
        return s;
    }
    let mut w = ids.write().unwrap();
    // Re-check: another thread may have interned it between the read and write.
    if let Some(s) = w.get(id) {
        return s;
    }
    let leaked: &'static str = Box::leak(id.to_string().into_boxed_str());
    w.insert(id.to_string(), leaked);
    leaked
}

/// The `(LanguageFn, highlights-query)` for a runtime-installed language, or
/// `None`. [`crate::highlight::Highlighter::for_language`] falls back to this
/// when no grammar is bundled for the id.
pub fn installed(id: &str) -> Option<(LanguageFn, String)> {
    let reg = registry().read().unwrap();
    let entry = reg.get(id)?;
    Some((entry.language_fn, entry.highlights.clone()))
}

/// The structural-indent query (`queries/indents.scm`) for a runtime-installed
/// language, or `None` when the grammar shipped none.
/// [`crate::highlight::Highlighter::for_language`] uses it to enable auto-indent.
pub fn installed_indent(id: &str) -> Option<String> {
    registry().read().unwrap().get(id)?.indents.clone()
}

/// The installed language id claiming file extension `ext` (no dot), or `None`.
/// `language_for_path` consults this after its bundled match misses, so opening a
/// `.foo` file highlights once a `.foo` grammar is installed — no restart.
pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    let reg = registry().read().unwrap();
    for (id, entry) in reg.iter() {
        if entry.extensions.iter().any(|e| e == ext) {
            return Some(intern(id));
        }
    }
    None
}

/// Every runtime-installed language id, sorted. Scheme reads this (alongside
/// `(bundled-languages)`) to offer `<id>-mode` commands for installed grammars.
pub fn installed_ids() -> Vec<String> {
    let mut ids: Vec<String> = registry().read().unwrap().keys().cloned().collect();
    ids.sort();
    ids
}

/// Insert a loaded grammar into the registry. Shared by [`install`] and the
/// tests (which register a bundled `LanguageFn` directly, skipping dlopen).
fn register(
    id: &str,
    language_fn: LanguageFn,
    highlights: String,
    indents: Option<String>,
    extensions: Vec<String>,
) {
    intern(id); // so a later language_for_extension hand-back is cheap
    registry().write().unwrap().insert(
        id.to_string(),
        Installed {
            language_fn,
            highlights,
            indents,
            extensions,
        },
    );
}

/// Run the full pipeline — fetch, compile, load, register — and return the
/// installed language id. Blocking; the editor calls it on the background
/// executor. Errors are human-readable (they surface in the echo area).
pub fn install(spec: InstallSpec) -> Result<String, String> {
    let root = cache_root().ok_or("could not locate a cache directory")?;
    install_in(spec, &root)
}

/// [`install`] against an explicit cache root (so tests don't touch `~/`).
fn install_in(spec: InstallSpec, root: &Path) -> Result<String, String> {
    let symbol = spec
        .symbol
        .clone()
        .unwrap_or_else(|| spec.id.replace('-', "_"));
    let slug = cache_slug(&spec);

    // 1. Fetch the repo at the pinned rev (cached by id+repo+rev+subdir).
    let checkout = root.join("sources").join(&slug);
    fetch(&spec.repo, &spec.rev, &checkout)?;
    let grammar_dir = match &spec.subdir {
        Some(sub) => checkout.join(sub),
        None => checkout.clone(),
    };

    // 2. Compile the committed parser (+ scanner) to a shared library, cached.
    //    Build into a unique temp path and rename into place, so a failed compile
    //    leaves no half-written `.dll` that the `lib.exists()` check would later
    //    treat as a good cache, and two concurrent installs of the same slug can't
    //    write the same file at once.
    let lib = root
        .join("lib")
        .join(format!("{slug}.{}", std::env::consts::DLL_EXTENSION));
    if !lib.exists() {
        if let Some(parent) = lib.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cache dir: {e}"))?;
        }
        let tmp_lib = unique_temp(&lib);
        let objs = tmp_lib.with_extension("objs");
        let cleanup = || {
            let _ = std::fs::remove_file(&tmp_lib);
            let _ = std::fs::remove_dir_all(&objs);
        };
        compile(&grammar_dir.join("src"), &tmp_lib).inspect_err(|_| cleanup())?;
        if lib.exists() {
            cleanup(); // a concurrent install published it first — keep theirs
        } else if let Err(e) = std::fs::rename(&tmp_lib, &lib) {
            cleanup();
            return Err(format!("publish library: {e}"));
        }
        let _ = std::fs::remove_dir_all(&objs); // drop intermediate object files
    }

    // 3. Read the highlights query — without it the parser produces a tree but no
    //    color, so a missing query is a hard error, not a silent no-op.
    let hl_rel = spec
        .highlights_path
        .as_deref()
        .unwrap_or("queries/highlights.scm");
    let hl_path = grammar_dir.join(hl_rel);
    let highlights = std::fs::read_to_string(&hl_path)
        .map_err(|e| format!("highlights query {}: {e}", hl_path.display()))?;

    // 3b. The structural-indent query is optional — read it if present (a
    //     missing one just leaves the language on copy-previous indentation),
    //     but if it exists it must compile, else it would silently no-op.
    let indents = std::fs::read_to_string(grammar_dir.join("queries/indents.scm")).ok();

    // 4. dlopen + ABI check, then register.
    let language_fn = load(&lib, &symbol)?;
    // Validate the query compiles against this grammar now, so a broken query
    // fails the install instead of silently disabling highlighting later.
    let language: Language = language_fn.into();
    tree_sitter::Query::new(&language, &highlights)
        .map_err(|e| format!("highlights query does not compile against {}: {e}", spec.id))?;
    if let Some(src) = &indents {
        tree_sitter::Query::new(&language, src)
            .map_err(|e| format!("indents query does not compile against {}: {e}", spec.id))?;
    }

    register(&spec.id, language_fn, highlights, indents, spec.extensions);
    Ok(spec.id)
}

/// Clone `repo` at `rev` into `dest`. A no-op if `dest` already holds a complete
/// checkout. We shell out to `git`: it's a one-time install step, not a hot path,
/// and the CLI handles arbitrary remotes and auth far more simply than a library.
///
/// Clone + checkout happen in a unique sibling temp dir that's only renamed into
/// `dest` *after* checkout succeeds. So `dest` existing always means a clean tree
/// at the right rev — a failed/interrupted clone never leaves a half-checkout (or
/// the default branch when the pinned rev's checkout failed, which a cache keyed
/// by rev would then silently reuse). It also lets two concurrent installs of the
/// same slug clone independently; the loser's temp dir is discarded.
fn fetch(repo: &str, rev: &str, dest: &Path) -> Result<(), String> {
    if dest.join(".git").exists() {
        return Ok(());
    }
    let parent = dest
        .parent()
        .ok_or_else(|| "cache dir has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("cache dir: {e}"))?;

    let tmp = unique_temp(dest);
    let _ = std::fs::remove_dir_all(&tmp); // clear any stale leftover
    let clean = |e: String| {
        let _ = std::fs::remove_dir_all(&tmp);
        e
    };

    // Full clone + checkout: works for a commit SHA, tag, or branch on any
    // server (a shallow `--branch` clone can't take an arbitrary SHA).
    run(
        Command::new("git")
            .arg("clone")
            .arg(repo)
            .arg(&tmp)
            .arg("--quiet"),
        "git clone",
    )
    .map_err(clean)?;
    run(
        Command::new("git")
            .current_dir(&tmp)
            .arg("checkout")
            .arg("--quiet")
            .arg(rev),
        "git checkout",
    )
    .map_err(clean)?;

    // Publish atomically. If another install won the race, keep theirs.
    if dest.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Ok(());
    }
    match std::fs::rename(&tmp, dest) {
        Ok(()) => Ok(()),
        // A concurrent install populated `dest` between our check and rename.
        Err(_) if dest.join(".git").exists() => {
            let _ = std::fs::remove_dir_all(&tmp);
            Ok(())
        }
        Err(e) => Err(clean(format!("publish checkout: {e}"))),
    }
}

/// Compile the grammar's C (and optional scanner) in `src_dir` into the shared
/// library `out`. Compiles each translation unit to an object, then links — so a
/// C++ scanner (`scanner.cc`) is built with the C++ driver and linked against
/// libc++, while the C `parser.c` stays C. The parser references no external
/// tree-sitter symbols (the runtime calls *into* it), so the library is
/// self-contained — no link against macros itself.
fn compile(src_dir: &Path, out: &Path) -> Result<(), String> {
    if !src_dir.join("parser.c").exists() {
        return Err(format!(
            "no {} — the grammar repo must commit its generated parser.c",
            src_dir.join("parser.c").display()
        ));
    }
    // The grammar exports its own entry point (`TS_PUBLIC` → `dllexport` on
    // Windows, `visibility("default")` elsewhere), so neither path needs explicit
    // export wiring — they just build a loadable library.
    #[cfg(windows)]
    {
        compile_windows(src_dir, out)
    }
    #[cfg(not(windows))]
    {
        compile_unix(src_dir, out)
    }
}

/// Compile on Unix by spawning the host `cc`/`c++` directly: each translation
/// unit to an object, then link into a shared library (so a C++ `scanner.cc` is
/// built and linked with the C++ driver while the C `parser.c` stays C).
#[cfg(not(windows))]
fn compile_unix(src_dir: &Path, out: &Path) -> Result<(), String> {
    let parser = src_dir.join("parser.c");
    let obj_dir = out.with_extension("objs");
    std::fs::create_dir_all(&obj_dir).map_err(|e| format!("obj dir: {e}"))?;

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".into());
    let mut objects = Vec::new();
    let mut cxx_link = false;

    // parser.c is always C.
    objects.push(compile_one(&cc, &parser, src_dir, &obj_dir, false)?);

    // An external scanner is optional and may be C or C++.
    if src_dir.join("scanner.c").exists() {
        objects.push(compile_one(
            &cc,
            &src_dir.join("scanner.c"),
            src_dir,
            &obj_dir,
            false,
        )?);
    } else if let Some(cpp) = ["scanner.cc", "scanner.cpp", "scanner.cxx"]
        .iter()
        .map(|f| src_dir.join(f))
        .find(|p| p.exists())
    {
        objects.push(compile_one(&cxx, &cpp, src_dir, &obj_dir, true)?);
        cxx_link = true;
    }

    // Link the objects into a shared library, using the C++ driver if any TU was
    // C++ so its runtime is pulled in.
    let linker = if cxx_link { &cxx } else { &cc };
    let mut cmd = Command::new(linker);
    cmd.args(shared_lib_flags());
    cmd.args(&objects);
    cmd.arg("-o").arg(out);
    run(&mut cmd, "link grammar")?;
    Ok(())
}

/// Compile one source file to an object in `obj_dir`, returning its path.
/// `-I src_dir` lets the parser find its vendored `tree_sitter/parser.h`.
#[cfg(not(windows))]
fn compile_one(
    compiler: &str,
    source: &Path,
    src_dir: &Path,
    obj_dir: &Path,
    cpp: bool,
) -> Result<PathBuf, String> {
    let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("tu");
    let obj = obj_dir.join(format!("{stem}.o"));
    let mut cmd = Command::new(compiler);
    cmd.arg("-c")
        .arg("-fPIC")
        .arg("-O2")
        .arg(if cpp { "-std=c++14" } else { "-std=c11" })
        .arg("-I")
        .arg(src_dir)
        .arg(source)
        .arg("-o")
        .arg(&obj);
    run(&mut cmd, &format!("compile {}", source.display()))?;
    Ok(obj)
}

/// Linker flags that produce a loadable shared library for this platform.
#[cfg(not(windows))]
fn shared_lib_flags() -> Vec<&'static str> {
    if cfg!(target_os = "macos") {
        vec!["-dynamiclib", "-fPIC"]
    } else {
        vec!["-shared", "-fPIC"]
    }
}

/// An external scanner source and whether it's C++ (so the build picks the right
/// compiler driver / language mode).
#[cfg(windows)]
struct Scanner {
    path: PathBuf,
    cpp: bool,
}

/// Locate the optional `scanner.{c,cc,cpp,cxx}` next to `parser.c`.
#[cfg(windows)]
fn scanner_source(src_dir: &Path) -> Option<Scanner> {
    if src_dir.join("scanner.c").exists() {
        return Some(Scanner {
            path: src_dir.join("scanner.c"),
            cpp: false,
        });
    }
    ["scanner.cc", "scanner.cpp", "scanner.cxx"]
        .iter()
        .map(|f| src_dir.join(f))
        .find(|p| p.exists())
        .map(|path| Scanner { path, cpp: true })
}

/// Compile on Windows into a `.dll`, mirroring Helix's approach. We don't spawn a
/// compiler by name (`cl.exe` isn't on `PATH` and needs `INCLUDE`/`LIB` set);
/// instead the `cc` crate locates the toolchain for the baked-in `BUILD_TARGET`
/// and hands back a `Command` with the right environment. MSVC builds the DLL in
/// one shot (`cl /LD`), dispatching C vs C++ by file extension; a `*-windows-gnu`
/// target falls back to the GNU `-shared` form. No `/EXPORT`: the grammar's
/// `__declspec(dllexport)` on the entry point already exposes the symbol.
///
/// Untested in this tree — there is no Windows host or MSVC toolchain in the dev
/// environment. It follows Helix's working recipe but needs a real Windows run.
#[cfg(windows)]
fn compile_windows(src_dir: &Path, out: &Path) -> Result<(), String> {
    let parser = src_dir.join("parser.c");
    let scanner = scanner_source(src_dir);

    // `BUILD_TARGET` is baked in by build.rs; the cc crate needs a target to find
    // the toolchain but reads `$TARGET` only during the build, not at runtime.
    let target = env!("BUILD_TARGET");
    let compiler = cc::Build::new()
        .cpp(scanner.as_ref().is_some_and(|s| s.cpp))
        .opt_level(2)
        .cargo_metadata(false)
        .host(target)
        .target(target)
        .warnings(false)
        .get_compiler();

    let obj_dir = out.with_extension("objs");
    std::fs::create_dir_all(&obj_dir).map_err(|e| format!("obj dir: {e}"))?;

    let mut cmd = compiler.to_command();
    if compiler.is_like_msvc() {
        cmd.args(["/nologo", "/LD", "/utf-8", "/std:c11"])
            .arg(format!("/I{}", src_dir.display()))
            .arg(&parser);
        if let Some(s) = &scanner {
            cmd.arg(&s.path);
        }
        cmd.arg(format!("/Fo{}\\", obj_dir.display()))
            .arg("/link")
            .arg(format!("/out:{}", out.display()));
    } else {
        // MinGW / clang on a *-windows-gnu target behaves like the Unix path.
        cmd.arg("-shared")
            .arg("-fPIC")
            .arg("-I")
            .arg(src_dir)
            .arg(&parser);
        if let Some(s) = &scanner {
            cmd.arg(&s.path);
        }
        cmd.arg("-o").arg(out);
    }
    run(&mut cmd, "compile grammar (windows)")
}

/// `dlopen` the compiled library, resolve `tree_sitter_<symbol>`, and wrap it as
/// a [`LanguageFn`] after checking the parser's ABI is one this tree-sitter can
/// drive. The library is leaked (`Box::leak`) so the function pointer stays valid
/// for the process — see the module safety note.
fn load(lib_path: &Path, symbol: &str) -> Result<LanguageFn, String> {
    // SAFETY: dlopen runs the library's initializers — inherent to loading any
    // native grammar. The path comes from a pinned, just-compiled artifact.
    let library = unsafe { libloading::Library::new(lib_path) }
        .map_err(|e| format!("dlopen {}: {e}", lib_path.display()))?;

    // Resolve the entry point and read the ABI *before* committing the library to
    // the process. The `func` borrow is scoped so `library` stays owned (and is
    // therefore unloaded if any check below fails — no leak on the error path);
    // only a fully validated grammar is leaked to keep its code mapped for good.
    let entry = format!("tree_sitter_{symbol}");
    let language_fn = {
        // SAFETY: the symbol's type is the tree-sitter parser-entry ABI, the same
        // signature `LanguageFn::from_raw` expects. A wrong `symbol` fails the
        // lookup rather than mistyping a real one.
        let func: libloading::Symbol<unsafe extern "C" fn() -> *const ()> =
            unsafe { library.get(entry.as_bytes()) }
                .map_err(|e| format!("symbol {entry} not found: {e}"))?;
        // SAFETY: the resolved pointer stays valid as long as `library` is loaded,
        // which it is for every use below and (after the leak) for the process.
        unsafe { LanguageFn::from_raw(*func) }
    };

    let language: Language = language_fn.into();
    let abi = language.abi_version();
    let (min, max) = (
        tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION,
        tree_sitter::LANGUAGE_VERSION,
    );
    if !(min..=max).contains(&abi) {
        // `library` drops here → dlclose → unmapped. Nothing leaked.
        return Err(format!(
            "grammar ABI version {abi} is unsupported (macros drives {min}..={max}); \
             rebuild the grammar against a compatible tree-sitter"
        ));
    }

    // Validated: keep the library mapped for the process so `language_fn` (a
    // pointer into its code) never dangles. See the module safety note.
    Box::leak(Box::new(library));
    Ok(language_fn)
}

/// The cache directory for grammar checkouts and compiled libraries
/// (`<cache>/macros/grammars`), matching where the self-updater stages downloads.
fn cache_root() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("macros").join("grammars"))
}

/// The trust list — repos the user has approved for download+compile+load. Lives
/// in the config dir (not the cache): trusting a source is a security decision
/// that should survive a cache wipe. One repo URL per line.
fn trust_file() -> PathBuf {
    crate::runtime::config_directory().join("trusted-grammars")
}

/// Whether `repo` has been approved before. Installing a grammar runs arbitrary
/// native code, so the editor confirms the first install from a given repo and
/// records the approval; subsequent installs (e.g. an init.scm re-run on every
/// launch) don't re-prompt.
pub fn is_trusted(repo: &str) -> bool {
    is_trusted_in(&trust_file(), repo)
}

/// Record `repo` as approved (idempotent). Called after the user confirms the
/// first install from it.
pub fn trust(repo: &str) {
    trust_in(&trust_file(), repo);
}

/// [`is_trusted`] against an explicit trust file (so tests don't depend on the
/// process-global config dir, which other tests mutate via `XDG_CONFIG_HOME`).
fn is_trusted_in(path: &Path, repo: &str) -> bool {
    let repo = repo.trim();
    std::fs::read_to_string(path)
        .map(|c| c.lines().any(|l| l.trim() == repo))
        .unwrap_or(false)
}

/// [`trust`] against an explicit trust file.
fn trust_in(path: &Path, repo: &str) {
    if is_trusted_in(path, repo) {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{}", repo.trim());
    }
}

/// Replace path-hostile characters in a string so it's safe as a directory name
/// (an id with a slash, a branch like `feature/x`, a tag with dots).
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The cache slug for a grammar: a readable `id` prefix plus a hash of everything
/// that determines what gets compiled — id, repo, rev, and subdir. Keying the
/// cache (sources + compiled lib) on all four means two repos sharing an id+rev,
/// or a mutable rev like `main` repointed at a different repo, never collide on a
/// stale library. (`sanitize` alone collapsed distinct revs like `v1.2`/`v1-2`.)
fn cache_slug(spec: &InstallSpec) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    spec.id.hash(&mut h);
    spec.repo.hash(&mut h);
    spec.rev.hash(&mut h);
    spec.subdir.hash(&mut h);
    format!("{}-{:016x}", sanitize(&spec.id), h.finish())
}

/// A unique sibling temp path for `base` (hidden, dotted), distinct across both
/// processes and threads so concurrent installs don't collide. Used to stage a
/// git checkout before atomically renaming it into place.
fn unique_temp(base: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = base
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("grammar");
    base.with_file_name(format!(".{name}.tmp.{}.{}", std::process::id(), n))
}

/// Run a command, mapping a non-zero exit or spawn failure to its stderr.
fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("{what}: failed to run ({e})"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let tail = stderr.trim().lines().next_back().unwrap_or("").trim();
    Err(format!("{what} failed: {tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registering a real `LanguageFn` makes it resolvable everywhere the editor
    /// looks: by id (`installed`), by extension (`language_for_extension`), and —
    /// the real integration — through `Highlighter::for_language`, which colors.
    /// Uses the bundled Rust grammar under a fake id so no dlopen/network is
    /// needed; the registry path is identical to a freshly loaded grammar.
    #[test]
    fn registry_drives_extension_lookup_and_highlighting() {
        register(
            "rustish",
            tree_sitter_rust::LANGUAGE,
            tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
            None,
            vec!["rsh".to_string()],
        );

        assert!(installed("rustish").is_some());
        assert_eq!(language_for_extension("rsh"), Some("rustish"));
        assert_eq!(language_for_extension("nope"), None);
        assert!(installed_ids().contains(&"rustish".to_string()));

        // The whole point: a non-bundled id now produces a working highlighter.
        let mut h = crate::highlight::Highlighter::for_language("rustish")
            .expect("registered grammar should build a highlighter");
        let rope = ropey::Rope::from_str("fn main() { let x = 1; }");
        h.set_text(&rope);
        let spans = h.spans_in(&rope, 0..rope.len_bytes());
        let has_keyword = spans
            .iter()
            .filter_map(|s| s.face.map(crate::highlight::face_name))
            .any(|f| f == "keyword");
        assert!(has_keyword, "registered grammar should color keywords");
    }

    /// Exercise the compile + dlopen toolchain end to end on a trivial C unit
    /// (not a real grammar): `compile` must produce a loadable library, and
    /// `load` must report a *clean error* — not crash — when the grammar symbol
    /// is absent. Proves the cc invocation and libloading path work without
    /// needing a network fetch or a real parser.c.
    #[test]
    fn compile_then_load_reports_missing_symbol_cleanly() {
        let tmp = std::env::temp_dir().join(format!("macros-grammar-test-{}", std::process::id()));
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        // A stand-in for parser.c: valid C, but exports no tree_sitter_* symbol.
        std::fs::write(
            src.join("parser.c"),
            "int macros_grammar_probe(void){return 7;}\n",
        )
        .unwrap();

        let lib = tmp.join(format!("probe.{}", std::env::consts::DLL_EXTENSION));
        compile(&src, &lib).expect("trivial C unit should compile to a shared library");
        assert!(lib.exists(), "compile should produce the library file");

        // Loading a non-existent grammar symbol is an Err, never UB. (`LanguageFn`
        // isn't `Debug`, so match the result rather than `unwrap_err`.)
        match load(&lib, "definitely_absent") {
            Ok(_) => panic!("missing symbol should not load"),
            Err(err) => assert!(err.contains("not found"), "got: {err}"),
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The full pipeline against a real grammar: clone tree-sitter-json at a
    /// pinned commit, compile its committed `parser.c`, dlopen it, read its
    /// highlights query, and confirm the resulting highlighter actually colors
    /// JSON. Ignored because it hits the network and the host toolchain — run
    /// manually with `cargo test --ignored real_grammar`.
    #[test]
    #[ignore = "network + C toolchain: real grammar install"]
    fn real_grammar_installs_and_highlights() {
        let root = std::env::temp_dir().join(format!("macros-grammar-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let spec = InstallSpec {
            id: "jsonish".to_string(),
            repo: "https://github.com/tree-sitter/tree-sitter-json".to_string(),
            rev: "001c28d7a29832b06b0e831ec77845553c89b56d".to_string(),
            extensions: vec!["jsonish".to_string()],
            subdir: None,
            // The repo exports `tree_sitter_json`, not `tree_sitter_jsonish` —
            // exercise the symbol override (id need not match the grammar name).
            symbol: Some("json".to_string()),
            highlights_path: None,
        };
        let id = install_in(spec, &root).expect("real grammar should install");
        assert_eq!(id, "jsonish");

        let mut h = crate::highlight::Highlighter::for_language("jsonish")
            .expect("installed grammar should build a highlighter");
        let rope = ropey::Rope::from_str(r#"{"key": 123, "ok": true}"#);
        h.set_text(&rope);
        let spans = h.spans_in(&rope, 0..rope.len_bytes());
        let faces: Vec<&str> = spans
            .iter()
            .filter_map(|s| s.face.map(crate::highlight::face_name))
            .collect();
        assert!(
            !faces.is_empty(),
            "real grammar should color JSON, got none"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rev_is_sanitized_for_paths() {
        assert_eq!(sanitize("v1.2.3"), "v1_2_3");
        assert_eq!(sanitize("feature/foo"), "feature_foo");
        assert_eq!(sanitize("abc123"), "abc123");
    }

    /// The cache slug must distinguish repo, rev, and subdir so two grammars that
    /// share an id can't collide on a stale checkout or compiled library — the
    /// bug a plain `id-rev` key had (and that `sanitize` alone made worse by
    /// collapsing distinct revs).
    #[test]
    fn cache_slug_distinguishes_repo_rev_and_subdir() {
        let mk = |repo: &str, rev: &str, subdir: Option<&str>| InstallSpec {
            id: "foo".into(),
            repo: repo.into(),
            rev: rev.into(),
            extensions: vec![],
            subdir: subdir.map(String::from),
            symbol: None,
            highlights_path: None,
        };
        let base = cache_slug(&mk("https://x/a", "main", None));
        assert!(
            base.starts_with("foo-"),
            "keeps a readable id prefix: {base}"
        );
        // Stable for identical input (same process).
        assert_eq!(base, cache_slug(&mk("https://x/a", "main", None)));
        // Any of repo / rev / subdir differing yields a different slug.
        assert_ne!(base, cache_slug(&mk("https://x/b", "main", None)));
        assert_ne!(base, cache_slug(&mk("https://x/a", "v2", None)));
        assert_ne!(base, cache_slug(&mk("https://x/a", "main", Some("ts"))));
    }

    /// `trust` makes a repo `is_trusted` and is idempotent, so a config-driven
    /// install on every launch only prompts once. Uses an explicit trust-file
    /// path so it doesn't race other tests over `XDG_CONFIG_HOME`.
    #[test]
    fn trust_persists_and_is_idempotent() {
        let tmp = std::env::temp_dir().join(format!("macros-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = tmp.join("trusted-grammars");

        let repo = "https://example.com/tree-sitter-foo";
        assert!(!is_trusted_in(&path, repo), "unknown repo starts untrusted");
        trust_in(&path, repo);
        assert!(is_trusted_in(&path, repo), "repo is trusted after approval");
        trust_in(&path, repo); // idempotent — no duplicate line
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.matches(repo).count(), 1, "no duplicate entries");
        assert!(!is_trusted_in(&path, "https://example.com/other"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
