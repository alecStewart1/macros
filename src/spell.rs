//! Native OS spell checking — the engine behind `scheme/spell.scm`'s
//! `'native` backend.
//!
//! On macOS this wraps AppKit's `NSSpellChecker`: the system dictionaries,
//! the user's learned words, and automatic language identification, with no
//! external tools to install.
//!
//! The synchronous `NSSpellChecker` calls wedge permanently inside a running
//! `NSApplication` — their XPC wait spins in a dispatch_semaphore_timedwait
//! plus reconnectOnError loop and the reply never arrives (observed live via
//! `sample`; the same calls work fine in this binary before the app starts,
//! and the async candidates API never invokes its completion handler
//! either). So:
//! * checking uses the asynchronous `requestCheckingOfString` API, which
//!   does complete in-app;
//! * suggestions and learn re-exec this binary in a helper mode
//!   (`--spell-suggest` / `--spell-learn`, see `main`), where the
//!   synchronous calls work — on a worker thread, results delivered via a
//!   callback;
//! * session "ignore" is implemented in Scheme (an ignore-list filter in
//!   spell.scm), not via the OS.
//!
//! On other platforms every function is a stub — Scheme probes
//! `spell-native-available?` and falls back to an `aspell -a` pipe.
//!
//! `NSSpellChecker` speaks UTF-16 ranges; the public API here speaks UTF-8
//! byte offsets (the editor's native unit), converting at the boundary.

/// Whether the native backend exists on this platform.
pub fn available() -> bool {
    cfg!(target_os = "macos")
}

/// Map a UTF-16 code-unit range into UTF-8 byte offsets in `text`.
/// Returns `None` when the range doesn't land on char boundaries (it always
/// does for ranges produced by the OS over the same string).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn utf16_range_to_bytes(text: &str, start16: usize, len16: usize) -> Option<(usize, usize)> {
    let end16 = start16.checked_add(len16)?; // NSNotFound locations overflow
    let (mut u16pos, mut start, mut end) = (0usize, None, None);
    for (bpos, ch) in text.char_indices() {
        if u16pos == start16 {
            start = start.or(Some(bpos));
        }
        if u16pos == end16 {
            end = end.or(Some(bpos));
            break;
        }
        u16pos += ch.len_utf16();
    }
    if u16pos == start16 {
        start = start.or(Some(text.len()));
    }
    if u16pos == end16 {
        end = end.or(Some(text.len()));
    }
    Some((start?, end?))
}

#[cfg(target_os = "macos")]
mod native {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2_app_kit::NSSpellChecker;
    use objc2_foundation::{
        NSArray, NSInteger, NSOrthography, NSRange, NSString, NSTextCheckingResult,
        NSTextCheckingType,
    };

    /// Session-wide spell-document tag: scopes `ignoreWord` entries so they
    /// last for this run of the editor without touching the learned-word list.
    fn document_tag() -> isize {
        use std::sync::OnceLock;
        static TAG: OnceLock<isize> = OnceLock::new();
        *TAG.get_or_init(NSSpellChecker::uniqueSpellDocumentTag)
    }

    /// Spell-check `text` asynchronously: `deliver` is called once — from an
    /// arbitrary thread — with every misspelled word as `(start, end, word)`
    /// UTF-8 byte ranges. Non-blocking; safe to call from the main thread.
    pub fn request_check<F>(text: String, deliver: F)
    where
        F: Fn(Vec<(usize, usize, String)>) + Send + Sync + 'static,
    {
        let checker = NSSpellChecker::sharedSpellChecker();
        let ns = NSString::from_str(&text);
        let range = NSRange::new(0, ns.length());
        let block = RcBlock::new(
            move |_seq: NSInteger,
                  results: NonNull<NSArray<NSTextCheckingResult>>,
                  _orthography: NonNull<NSOrthography>,
                  _word_count: NSInteger| {
                let mut out = Vec::new();
                // SAFETY: AppKit hands the completion handler a valid, non-null
                // NSArray that it keeps alive for the duration of the call.
                for result in unsafe { results.as_ref() } {
                    if !result.resultType().contains(NSTextCheckingType::Spelling) {
                        continue;
                    }
                    let r = result.range();
                    if let Some((s, e)) = super::utf16_range_to_bytes(&text, r.location, r.length) {
                        out.push((s, e, text[s..e].to_string()));
                    }
                }
                deliver(out);
            },
        );
        // SAFETY: every argument is a live, correctly typed Objective-C object or
        // plain value; AppKit retains `block` (an `RcBlock`) until it has run.
        unsafe {
            checker.requestCheckingOfString_range_types_options_inSpellDocumentWithTag_completionHandler(
                &ns,
                range,
                NSTextCheckingType::Spelling.0,
                None,
                document_tag(),
                Some(&block),
            )
        };
    }

    /// Correction candidates for a (whole) misspelled word. Synchronous —
    /// only safe in a plain process (the `--spell-suggest` helper mode), it
    /// never returns inside the running app.
    pub fn guesses(word: &str) -> Vec<String> {
        let checker = NSSpellChecker::sharedSpellChecker();
        let ns = NSString::from_str(word);
        let range = NSRange::new(0, ns.length());
        checker
            .guessesForWordRange_inString_language_inSpellDocumentWithTag(
                range,
                &ns,
                None,
                document_tag(),
            )
            .map(|list| list.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    }

    /// Add `word` to the user's permanent learned-word dictionary.
    /// Synchronous — only safe in the `--spell-learn` helper process.
    pub fn learn(word: &str) {
        NSSpellChecker::sharedSpellChecker().learnWord(&NSString::from_str(word));
    }
}

#[cfg(target_os = "macos")]
pub use native::{guesses, learn, request_check};

#[cfg(not(target_os = "macos"))]
mod stub {
    pub fn request_check<F>(_text: String, deliver: F)
    where
        F: Fn(Vec<(usize, usize, String)>) + Send + Sync + 'static,
    {
        deliver(Vec::new());
    }
    pub fn guesses(_word: &str) -> Vec<String> {
        Vec::new()
    }
    pub fn learn(_word: &str) {}
}

#[cfg(not(target_os = "macos"))]
pub use stub::{guesses, learn, request_check};

/// Correction candidates, asynchronously: re-exec this binary in
/// `--spell-suggest` helper mode on a worker thread (where the synchronous
/// checker works — see the module docs) and deliver its stdout lines.
pub fn request_suggestions<F>(word: String, deliver: F)
where
    F: Fn(Vec<String>) + Send + Sync + 'static,
{
    std::thread::spawn(move || {
        let suggestions = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                std::process::Command::new(exe)
                    .args(["--spell-suggest", &word])
                    .output()
                    .ok()
            })
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        deliver(suggestions);
    });
}

/// Add a word to the OS user dictionary via the `--spell-learn` helper
/// process (fire-and-forget, off the UI thread).
pub fn learn_async(word: String) {
    std::thread::spawn(move || {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe)
                .args(["--spell-learn", &word])
                .status();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::utf16_range_to_bytes;

    #[test]
    fn utf16_offsets_map_to_bytes_across_multibyte_chars() {
        // "café wrld": é is 1 UTF-16 unit but 2 UTF-8 bytes.
        let text = "café wrld";
        // "wrld" is UTF-16 units [5, 9) but bytes [6, 10).
        assert_eq!(utf16_range_to_bytes(text, 5, 4), Some((6, 10)));
        assert_eq!(&text[6..10], "wrld");
        // Astral chars take 2 UTF-16 units and 4 bytes: "𝄞x" → x at u16 2, byte 4.
        let astral = "𝄞x";
        assert_eq!(utf16_range_to_bytes(astral, 2, 1), Some((4, 5)));
        // Range ending at end-of-string.
        assert_eq!(utf16_range_to_bytes("ab", 1, 1), Some((1, 2)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_backend_reports_available() {
        // The checker itself is main-thread-affine (cargo test runs on worker
        // threads), so only the availability probe is exercised here; the
        // check/suggest paths are covered by using spell-mode in the editor.
        assert!(super::available());
    }
}
