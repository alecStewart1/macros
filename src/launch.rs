//! Files the OS asks Macros to open: command-line paths (`macros a.rs`, the
//! Linux `.desktop` file's `Exec=macros %F`, Windows "Open with") and macOS
//! Finder opens, which arrive as `application:openURLs:` events instead of argv.
//!
//! Paths that arrive before the first window has finished startup are queued
//! and opened right after its `after-init-hook` (session restore), so a
//! restored desktop can't bury them. The queue also tells session.scm the
//! launch was on explicit files (`command-line-files`), which keeps it from
//! restoring or claiming the saved desktop. Once startup is done, new paths are
//! handed back to the caller to open straight away.

use std::path::PathBuf;
use std::sync::Mutex;

struct State {
    pending: Vec<PathBuf>,
    /// The first window has drained the queue; later paths open immediately.
    ready: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    pending: Vec::new(),
    ready: false,
});

/// The non-flag command-line arguments, as paths.
pub fn args() -> Vec<PathBuf> {
    std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .collect()
}

/// Queue `paths` for the first window, or — once startup is done — return
/// them for the caller to open now.
pub fn queue(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut state = STATE.lock().unwrap();
    if state.ready {
        paths
    } else {
        state.pending.extend(paths);
        Vec::new()
    }
}

/// The first window's drain, after session restore: everything queued so
/// far. Marks startup done, so later calls return nothing.
pub fn take_startup() -> Vec<PathBuf> {
    let mut state = STATE.lock().unwrap();
    state.ready = true;
    std::mem::take(&mut state.pending)
}

/// The files this launch was asked to open, while startup is still running
/// (empty after). Backs Scheme's `(command-line-files)`.
pub fn startup_files() -> Vec<String> {
    let state = STATE.lock().unwrap();
    state
        .pending
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// A `file://` URL from a macOS open event → its local path. Other schemes
/// (a future `macros://` handler, say) aren't files and yield `None`.
pub fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file://localhost/x` and `file:///x` both name `/x`.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(percent_decode(rest)?))
}

/// Decode `%XX` escapes (UTF-8), as Finder URL-encodes spaces and non-ASCII.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::path_from_file_url;
    use std::path::PathBuf;

    #[test]
    fn file_urls_decode_to_paths() {
        assert_eq!(
            path_from_file_url("file:///Users/me/My%20Notes/caf%C3%A9.md"),
            Some(PathBuf::from("/Users/me/My Notes/café.md"))
        );
        assert_eq!(
            path_from_file_url("file://localhost/tmp/a.csv"),
            Some(PathBuf::from("/tmp/a.csv"))
        );
        assert_eq!(path_from_file_url("https://example.com/a.rs"), None);
        assert_eq!(path_from_file_url("file:///bad%zz"), None);
    }
}
