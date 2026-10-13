//! Crypto and encoding primitives (OAuth PKCE and general use): random
//! bytes, sha256, base64url / hex and charset decoding.

use base64::Engine as _;
use sha2::Digest as _;

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, _ctx: &EngineShared) {
    reg.section(Section::Crypto);
    // --- Crypto / encoding (OAuth PKCE + general use) ---------------------
    // Binary is exchanged with Scheme as hex strings (always valid UTF-8);
    // `hex->base64url` bridges hex into base64url so PKCE composes cleanly:
    //   (define verifier  (hex->base64url (random-bytes 32)))
    //   (define challenge (hex->base64url (sha256 verifier)))
    //   (define state     (hex->base64url (random-bytes 32)))
    reg.register_fn("random-bytes", |NonNeg(n): NonNeg| -> String {
        // Key/nonce material, not bulk data: cap so a typo can't OOM the editor.
        if n > 1 << 20 {
            return String::new();
        }
        let mut buf = vec![0u8; n];
        match getrandom::getrandom(&mut buf) {
            Ok(()) => hex::encode(&buf),
            Err(_) => String::new(),
        }
    });
    reg.register_fn("sha256", |input: String| -> String {
        let mut hasher = sha2::Sha256::new();
        hasher.update(input.as_bytes());
        hex::encode(hasher.finalize())
    });
    reg.register_fn("base64url-encode", |input: String| -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes())
    });
    reg.register_fn("hex->base64url", |input: String| -> String {
        match hex::decode(input.trim()) {
            Ok(bytes) => base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
            Err(_) => String::new(),
        }
    });
    // `(base64url-hex b64)` → hex of the decoded bytes, or "" on garbage.
    // The inverse of `hex->base64url`, but deliberately liberal in what it
    // accepts: both the URL-safe and standard alphabets, with or without `=`
    // padding, ignoring ASCII whitespace — so one primitive covers Gmail
    // body data (URL-safe), MIME/RFC 2047 words (standard, padded, often
    // line-wrapped), and anything a Scheme package meets in the wild.
    reg.register_fn("base64url-hex", |input: String| -> String {
        let normalized: String = input
            .chars()
            .filter(|c| !c.is_ascii_whitespace() && *c != '=')
            .map(|c| match c {
                '-' => '+',
                '_' => '/',
                c => c,
            })
            .collect();
        match base64::engine::general_purpose::STANDARD_NO_PAD.decode(normalized.as_bytes()) {
            Ok(bytes) => hex::encode(bytes),
            Err(_) => String::new(),
        }
    });
    // `(decode-charset hex label)` → the bytes decoded to a UTF-8 string.
    // `label` is a WHATWG encoding label ("utf-8", "windows-1252",
    // "iso-8859-1", "shift_jis", …); an unknown label falls back to UTF-8.
    // Decoding is lossy (U+FFFD for malformed sequences) — text extraction,
    // not validation. Bytes cross the boundary as hex, the same interchange
    // as `random-bytes` / `hex->base64url`.
    reg.register_fn("decode-charset", |input: String, label: String| -> String {
        let Ok(bytes) = hex::decode(input.trim()) else {
            return String::new();
        };
        let encoding =
            encoding_rs::Encoding::for_label(label.trim().as_bytes()).unwrap_or(encoding_rs::UTF_8);
        let (text, _, _) = encoding.decode(&bytes);
        text.into_owned()
    });
}
