//! Small cross-cutting helpers with no better home.

/// Expand a leading `~/` to the home directory ([`crate::runtime::home_dir`]:
/// `$HOME`, else `%USERPROFILE%`, since Windows rarely sets `HOME`) — the one
/// shell convenience that `std::process::Command` and the raw filesystem APIs
/// don't provide. Used at every spot that takes a user-supplied program or
/// file path (LSP commands, `call-process` / `make-process`, file finding), so
/// `~/bin/foo` resolves instead of being looked up as a literal path beginning
/// with `~`.
///
/// Only a leading `~/` is special; bare names, absolute paths, and a `~` that
/// appears anywhere else are returned unchanged. `~user` (another user's home)
/// is intentionally not supported — it needs passwd lookups and is rarely what
/// anyone means in this editor's configs.
pub(crate) fn expand_tilde(input: &str) -> String {
    if let Some(rest) = input.strip_prefix("~/")
        && let Some(home) = crate::runtime::home_dir()
    {
        return format!("{}/{rest}", home.display());
    }
    input.to_string()
}

/// Spawn a child without giving it a console window of its own. The Windows
/// build is a GUI-subsystem app (no console — see the `windows_subsystem`
/// attribute in main.rs), so every console program it starts (LSP servers,
/// git, gpg, `call-process`, …) would otherwise pop up a fresh terminal
/// window. `CREATE_NO_WINDOW` suppresses that; piped stdio is unaffected. A
/// no-op off Windows. Apply it at every production `Command` spawn site.
pub(crate) trait HideConsole {
    fn hide_console(&mut self) -> &mut Self;
}

impl HideConsole for std::process::Command {
    fn hide_console(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

/// Collapse every run of whitespace (spaces, tabs, newlines) to a single space
/// and trim the ends. Used anywhere multi-line text must render on ONE line —
/// gpui's `shape_line` *panics* (aborting the app) on an embedded newline, and
/// LSP servers routinely hand back multi-line completion `detail` and diagnostic
/// messages. Collapsing (rather than only stripping newlines) also keeps a
/// re-flowed signature or error tidy in the popup / echo area.
pub(crate) fn single_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true; // start true to trim leading whitespace
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{expand_tilde, single_line};

    #[test]
    fn single_line_collapses_and_trims() {
        assert_eq!(single_line("a\nb"), "a b");
        assert_eq!(
            single_line("('a -> 'b) ->\n  'a list ->\n  'b list"),
            "('a -> 'b) -> 'a list -> 'b list"
        );
        assert_eq!(
            single_line("  leading\ttabs\r\nand crlf  "),
            "leading tabs and crlf"
        );
        assert_eq!(single_line("nochange"), "nochange");
        assert!(!single_line("x\ny").contains('\n'));
    }

    #[test]
    fn expands_leading_home_only() {
        // SAFETY: single-threaded test; no other thread reads HOME concurrently.
        unsafe { std::env::set_var("HOME", "/home/tester") };
        assert_eq!(
            expand_tilde("~/.opam/bin/ocaml_lsp"),
            "/home/tester/.opam/bin/ocaml_lsp"
        );
        // Bare names and absolute paths pass through untouched.
        assert_eq!(expand_tilde("rust-analyzer"), "rust-analyzer");
        assert_eq!(expand_tilde("/usr/bin/gopls"), "/usr/bin/gopls");
        // Only a leading `~/` is special; `~` elsewhere is literal.
        assert_eq!(expand_tilde("./~/foo"), "./~/foo");
        assert_eq!(expand_tilde("~user/foo"), "~user/foo");
    }
}

/// Whether the file at `p` has an execute bit (always true off Unix, where the
/// extension decides).
#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_: &std::path::Path) -> bool {
    true
}

/// The full path of executable `name` on `PATH` (Emacs `executable-find`). A
/// name with a path separator is checked as given. On Windows, `.exe` / `.cmd`
/// / `.bat` are tried too.
pub fn executable_find(name: &str) -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    if name.is_empty() {
        return None;
    }
    let candidates = |dir: &Path| -> Vec<PathBuf> {
        let base = dir.join(name);
        if cfg!(windows) {
            // Suffixed forms first (Emacs `exec-suffixes` order): npm installs
            // an extensionless sh shim next to `foo.cmd`, and every file counts
            // as "executable" here, so trying the bare name first would return
            // the shim, which Windows can't run.
            [".exe", ".cmd", ".bat", ""]
                .iter()
                .map(|ext| PathBuf::from(format!("{}{ext}", base.display())))
                .collect()
        } else {
            vec![base]
        }
    };
    let runnable = |p: &Path| p.is_file() && is_executable(p);
    if name.contains(std::path::MAIN_SEPARATOR) || name.contains('/') {
        let p = PathBuf::from(name);
        return runnable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| candidates(&dir))
        .find(|p| runnable(p))
}

#[cfg(test)]
mod executable_find_tests {
    #[cfg(unix)]
    use crate::util::executable_find;

    #[test]
    #[cfg(unix)]
    fn finds_programs_on_path() {
        assert!(executable_find("sh").is_some_and(|p| p.ends_with("sh")));
        assert!(executable_find("surely-not-a-real-program-xyz").is_none());
        assert!(executable_find("/bin/sh").is_some());
        assert!(executable_find("").is_none());
    }
}
