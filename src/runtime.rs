use std::path::{Path, PathBuf};

use include_dir::{Dir, include_dir};

/// The embedded Steel runtime — everything under `scheme/`, baked into the
/// binary at compile time. Top-level `*.scm` files are evaluated at startup;
/// `themes/*.scm` are loaded on demand by `load-theme`.
static SCHEME: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/scheme");

/// The contents of the top-level `scheme/*.scm` files, sorted by name, for
/// startup evaluation. (Subdirectories like `themes/` are not included.)
pub fn startup_scripts() -> Vec<&'static str> {
    let mut files: Vec<_> = SCHEME
        .files()
        .filter(|f| f.path().extension().is_some_and(|e| e == "scm"))
        .collect();
    files.sort_by_key(|f| f.path());
    files.iter().filter_map(|f| f.contents_utf8()).collect()
}

/// Names defined at top level by the bundled `scheme/*.scm` library — every
/// `(define (foo …))`, `(define foo …)`, and `(define-syntax foo …)`. These are
/// real bindings once the runtime loads, but the built-in Scheme analyzer (a
/// separate compile-only engine) doesn't see them, so it would flag a user
/// config that calls `lsp-language`, `evil-define-key`, etc. as "unknown
/// function". Seeding the analyzer with these names fixes that.
pub fn defined_names() -> Vec<String> {
    // Top-level forms start at column 0 in the bundled files.
    startup_scripts()
        .iter()
        .flat_map(|src| src.lines())
        .filter_map(define_name)
        .collect()
}

/// The name bound by a top-level `(define name …)`, `(define (name …) …)`,
/// `(define-syntax name …)`, or `(define-command (name …) …)` line, if the line
/// is such a define. Shared by [`defined_names`] and [`function_docs`].
pub(crate) fn define_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("(define-syntax ")
        .or_else(|| line.strip_prefix("(define-command "))
        .or_else(|| line.strip_prefix("(define "))?;
    // `(define (name …)` — skip the opening paren of the arg list.
    let token = rest
        .trim_start()
        .strip_prefix('(')
        .unwrap_or(rest.trim_start());
    let name: String = token
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '(' && *c != ')')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// One documented top-level definition: its name, the `;;` comment block
/// immediately preceding it (markers stripped), the source file it lives in, and
/// the 1-based line of its `define`. Steel has no runtime docstrings, so the
/// source comment IS the documentation; the `file`/`line` let `describe-function`
/// / `helm-apropos` jump to the implementation.
#[derive(Debug, Clone)]
pub struct DocEntry {
    pub name: String,
    pub doc: String,
    pub file: String,
    pub line: usize,
}

/// Scrape every top-level `(define …)` / `(define-syntax …)` in `src`, pairing
/// each with its preceding `;;` comment block (markers stripped) and attributing
/// it to `file` with its 1-based line. Shared by the startup seed over the
/// bundled library ([`builtin_docs`]) and the runtime indexer over dynamically
/// loaded packages (`index-doc-file`), so built-ins and packages are documented
/// by exactly the same rules. A define with no preceding comment gets an empty
/// doc string.
pub fn scrape_docs(src: &str, file: &str) -> Vec<DocEntry> {
    let mut docs = Vec::new();
    // Accumulate consecutive comment lines; they document the next define.
    let mut comment: Vec<&str> = Vec::new();
    for (i, line) in src.lines().enumerate() {
        if let Some(rest) = line.trim_start().strip_prefix(";;") {
            comment.push(rest.strip_prefix(' ').unwrap_or(rest));
            continue; // keep the block growing — don't clear
        }
        if let Some(name) = define_name(line) {
            docs.push(DocEntry {
                name,
                doc: comment.join("\n").trim().to_string(),
                file: file.to_string(),
                line: i + 1, // 1-based, like an editor's line numbers
            });
        }
        comment.clear(); // a non-comment line breaks contiguity
    }
    docs
}

/// Every bundled built-in definition with its doc and source location. The file
/// is the definition's path in the compile-time source tree
/// (`$CARGO_MANIFEST_DIR/scheme/…`): present when running from a checkout (so a
/// jump opens the real file) and gracefully absent for an installed binary,
/// where the caller falls back to showing the doc.
pub fn builtin_docs() -> Vec<DocEntry> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/scheme");
    let mut files: Vec<_> = SCHEME
        .files()
        .filter(|f| f.path().extension().is_some_and(|e| e == "scm"))
        .collect();
    files.sort_by_key(|f| f.path());
    let mut docs = Vec::new();
    for f in files {
        if let Some(src) = f.contents_utf8() {
            let file = format!("{root}/{}", f.path().display());
            docs.extend(scrape_docs(src, &file));
        }
    }
    docs
}

/// The names of the bundled themes — the stems of `scheme/themes/*.scm`, sorted.
/// Scheme reads this via `(theme-names)` to offer completion for `load-theme`
/// (both the minibuffer prompt and the in-buffer argument CAPF).
pub fn theme_names() -> Vec<String> {
    let mut names: Vec<String> = SCHEME
        .get_dir("themes")
        .into_iter()
        .flat_map(|d| d.files())
        .filter(|f| f.path().extension().is_some_and(|e| e == "scm"))
        .filter_map(|f| {
            f.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(String::from)
        })
        .collect();
    names.sort();
    names
}

/// A theme definition by name — the contents of `scheme/themes/<name>.scm`.
pub fn theme_script(name: &str) -> Option<&'static str> {
    SCHEME
        .get_file(format!("themes/{name}.scm"))
        .and_then(|f| f.contents_utf8())
}

/// Import the login shell's `PATH` into our own environment, so subprocesses
/// (LSP servers, `call-process`, `make-process`) can find tools installed via
/// nvm/pyenv/cargo/brew/asdf/mise/etc.
///
/// Why this is needed: on macOS (and some Linux desktops) a GUI app launched
/// from Finder/Dock/`.desktop` inherits a minimal `launchd` environment — not
/// the user's shell `PATH` — so `typescript-language-server`, `rust-analyzer`,
/// and friends are invisible. We recover the real `PATH` by asking the login
/// shell. Windows has no such stripping (GUI apps inherit the registry `PATH`),
/// so this is a no-op there.
///
/// Must be called at the very start of `main`, before any threads spawn, since
/// it mutates the process environment (`set_var` is unsound once other threads
/// may be reading the environment).
#[cfg(unix)]
pub fn import_shell_env() {
    use std::io::IsTerminal;

    // Launched from a terminal? Then we already inherited a real shell `PATH`;
    // skip the ~50-150ms subshell cost.
    if std::io::stdout().is_terminal() {
        return;
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    // `-l -i` so login + interactive rc files run — nvm/pyenv/asdf typically
    // inject `PATH` from `.zshrc`/`.bashrc` (interactive), not just profile.
    // `output()` feeds the child a null stdin, so an interactive shell can't
    // block waiting for input.
    let Ok(out) = std::process::Command::new(&shell)
        .args([
            "-l",
            "-i",
            "-c",
            "command -v printf >/dev/null && printf %s \"$PATH\"",
        ])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let path = String::from_utf8_lossy(&out.stdout);
    let path = path.trim();
    if !path.is_empty() {
        // SAFETY: called before any threads are spawned (top of `main`).
        unsafe { std::env::set_var("PATH", path) };
    }
}

/// No-op on Windows: GUI apps already inherit the registry-assembled `PATH`.
#[cfg(not(unix))]
pub fn import_shell_env() {}

/// The user's home directory, from `$HOME` (Unix) or `%USERPROFILE%` (Windows).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Candidate config directories, most-specific first:
///   1. `$XDG_CONFIG_HOME/macros` (explicit override, any OS)
///   2. `~/.config/macros`        (macOS + Linux; the documented location)
///   3. `%APPDATA%/macros`        (native Windows config)
fn config_dirs() -> Vec<PathBuf> {
    // A non-empty env var joined with the app sub-directory.
    let from_env = |key: &str| {
        std::env::var_os(key)
            .filter(|x| !x.is_empty())
            .map(|v| PathBuf::from(v).join("macros"))
    };
    let mut dirs = Vec::new();
    dirs.extend(from_env("XDG_CONFIG_HOME"));
    if let Some(home) = home_dir() {
        dirs.push(home.join(".config").join("macros"));
    }
    dirs.extend(from_env("APPDATA"));
    dirs
}

/// The user's `init.scm` and the directory it lives in, if present. Absent =
/// `None` (the editor just uses its built-in defaults, no error).
pub fn user_config() -> Option<(PathBuf, String)> {
    config_dirs().into_iter().find_map(|dir| {
        std::fs::read_to_string(dir.join("init.scm"))
            .ok()
            .map(|src| (dir, src))
    })
}

/// Whether `path` is editor-aware Scheme — config / extension code that should
/// use the `macros-scheme` language (a built-in Steel server seeded with the
/// editor API, plus the `macros-steel` major mode) rather than the vanilla
/// `scheme` server. True when the file lives under the user's config directory,
/// or is named `*.macros.scm` (an explicit opt-in usable from anywhere). Plain
/// `.scm` files are left as "just the interpreter".
pub fn is_macros_scheme_path(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.ends_with(".macros.scm") || config_dirs().iter().any(|dir| path.starts_with(dir))
}

/// The active config directory: where `init.scm` was found, else the primary
/// candidate location (so it resolves even before the user writes a config).
/// `package-install` clones into its `packages/` subdirectory — exposed to Scheme
/// as `(packages-directory)`.
pub fn config_directory() -> PathBuf {
    user_config()
        .map(|(dir, _)| dir)
        .or_else(|| config_dirs().into_iter().next())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A small prelude, evaluated before `init.scm`, that exposes `*config-directory*`
/// and an `include` helper resolving paths against it — so a config can split
/// across files with `(include "keymaps.scm")`. (`load` remains available for
/// absolute paths.) Built on Steel's native `load`, which evaluates the file in
/// the current engine so its definitions land in the global scope.
pub fn include_prelude(dir: &Path) -> String {
    let escaped = dir
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        "(define *config-directory* \"{escaped}\")\n\
         (define (include path) (load (string-append *config-directory* \"/\" path)))\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_startup_and_themes() {
        // Startup scripts are present and the keymap is among them.
        let scripts = startup_scripts();
        assert!(!scripts.is_empty());
        assert!(scripts.iter().any(|s| s.contains("forward-char")));

        // Themes resolve and contain set-face calls; unknown ones don't.
        assert!(theme_script("macros-dark").is_some_and(|s| s.contains("set-face")));
        assert!(theme_script("macros-light").is_some());
        assert!(theme_script("nope").is_none());

        // `theme-names` enumerates the bundled themes (stems, sorted, no .scm).
        let names = theme_names();
        assert_eq!(
            names,
            vec!["macros-dark".to_string(), "macros-light".to_string()]
        );
    }

    /// Every color literal in the bundled themes must parse (catches a bad hex
    /// or stray digit — including the 8-digit `#rrggbbaa` translucent values).
    #[test]
    fn bundled_theme_colors_parse() {
        for name in ["macros-dark", "macros-light"] {
            let src = theme_script(name).unwrap();
            let mut count = 0;
            // Each face line is `(set-face "name" "#hex")` — grab the quoted hex.
            for line in src.lines() {
                if let Some(rest) = line.trim().strip_prefix("(set-face ")
                    && let Some(hex_start) = rest.find("\"#")
                {
                    let hex = &rest[hex_start + 1..];
                    let hex = &hex[..hex.find('"').expect("closing quote")];
                    assert!(
                        crate::theme::parse_color(hex).is_some(),
                        "{name}: unparseable color {hex:?} in line {line:?}"
                    );
                    count += 1;
                }
            }
            assert!(count > 10, "{name}: expected many faces, found {count}");
        }
    }

    #[test]
    fn function_docs_scrape_preceding_comments() {
        let docs = builtin_docs();
        let entry = |name: &str| docs.iter().find(|e| e.name == name);
        let find = |name: &str| entry(name).map(|e| e.doc.clone());

        // Every define is captured…
        assert!(!docs.is_empty());
        // …and a documented one carries its preceding ;; block.
        let doc = find("save-excursion").expect("save-excursion should be scraped");
        assert!(doc.contains("restore point"), "got: {doc:?}");
        // The ;; prefix is stripped (doc text, not raw comment).
        assert!(
            !doc.contains(";;"),
            "comment markers should be stripped: {doc:?}"
        );

        // The tree-sitter rainbow demo's public setters are discoverable.
        assert!(find("rainbow-set-colors").is_some());

        // Markdown's override variables each carry their OWN docstring, so
        // apropos / describe-function explain them to someone writing overrides.
        let hide =
            find("*markdown-hide-heading-markers*").expect("hide-markers var should be scraped");
        assert!(hide.contains("init.scm"), "got: {hide:?}");
        let scale = find("*markdown-scale-headings*").expect("scale var should be scraped");
        assert!(
            scale.contains("larger font"),
            "the scale var needs its own doc, not the previous block: {scale:?}"
        );

        // Source location is captured: a real bundled file, on a sane 1-based
        // line. This is what lets helm-apropos jump to the implementation.
        let se = entry("save-excursion").unwrap();
        assert!(
            se.file.ends_with(".scm"),
            "file should be a .scm: {:?}",
            se.file
        );
        assert!(se.line >= 1, "line should be 1-based, got {}", se.line);
    }

    /// `scrape_docs` attributes defines to the file it is told, and counts lines
    /// from 1 — the contract `index-doc-file` relies on for packages.
    #[test]
    fn scrape_docs_records_file_and_line() {
        let src = "; not a doc comment\n\
                   ;; doc for foo\n\
                   (define (foo x) x)\n\
                   \n\
                   (define bar 1)\n";
        let docs = scrape_docs(src, "/pkg/thing.scm");
        let foo = docs.iter().find(|e| e.name == "foo").unwrap();
        assert_eq!(foo.doc, "doc for foo");
        assert_eq!(foo.file, "/pkg/thing.scm");
        assert_eq!(foo.line, 3);
        // A define with no preceding ;; block has an empty doc, still located.
        let bar = docs.iter().find(|e| e.name == "bar").unwrap();
        assert_eq!(bar.doc, "");
        assert_eq!(bar.line, 5);
    }

    #[test]
    fn include_prelude_escapes_and_resolves() {
        // The config dir becomes a valid Steel string literal (backslashes and
        // quotes escaped — important for Windows paths).
        let prelude = include_prelude(Path::new(r"C:\Users\me\macros"));
        assert!(prelude.contains(r#""C:\\Users\\me\\macros""#));
        assert!(prelude.contains("(define (include path)"));
        // A POSIX path passes through cleanly.
        let prelude = include_prelude(Path::new("/home/me/.config/macros"));
        assert!(prelude.contains(r#""/home/me/.config/macros""#));
    }
}

#[cfg(test)]
mod config_integration {
    use super::*;
    #[test]
    fn user_config_with_include_roundtrip() {
        // A temp config dir as XDG_CONFIG_HOME/macros with init.scm that includes
        // a second file — exactly the main.rs flow.
        let root = std::env::temp_dir().join(format!("macros-cfg-{}", std::process::id()));
        let dir = root.join("macros");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.scm"), "(include \"more.scm\")\n").unwrap();
        std::fs::write(dir.join("more.scm"), "(set-option \"font-size\" \"17\")\n").unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &root);
        }

        let (dir2, src) = user_config().expect("init.scm found via XDG_CONFIG_HOME");
        assert_eq!(dir2, dir);
        assert!(src.contains("include"));

        // Eval prelude + init through a real engine; the included set-option lands.
        let (engine, _rx) = crate::script::ScriptEngine::new();
        engine.eval(&include_prelude(&dir2)).unwrap();
        let result = engine.eval(&src).unwrap();
        let has_set = result.commands.iter().any(|c| {
            matches!(
            c, crate::script::EditorCommand::SetOption(k, v) if k == "font-size" && *v == "17")
        });
        assert!(
            has_set,
            "included file's set-option should apply, got {:?}",
            result.commands
        );

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
