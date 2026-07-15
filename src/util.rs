//! Small cross-cutting helpers with no better home.

/// Expand a leading `~/` to `$HOME` — the one shell convenience that
/// `std::process::Command` and the raw filesystem APIs don't provide. Used at
/// every spot that takes a user-supplied program or file path (LSP commands,
/// `call-process` / `make-process`, file finding), so `~/bin/foo` resolves
/// instead of being looked up as a literal path beginning with `~`.
///
/// Only a leading `~/` is special; bare names, absolute paths, and a `~` that
/// appears anywhere else are returned unchanged. `~user` (another user's home)
/// is intentionally not supported — it needs passwd lookups and is rarely what
/// anyone means in this editor's configs.
pub(crate) fn expand_tilde(input: &str) -> String {
    if let Some(rest) = input.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    input.to_string()
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
