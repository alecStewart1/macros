//! Encoding and CSV dialect detection, and the transcoding both ways.
//!
//! [`Dialect`] holds everything the writer needs to reproduce the file's
//! format: encoding and BOM, delimiter, quote and escape style, the line
//! ending for new rows, and whether the file ended with a newline. Detection
//! looks at the first [`SAMPLE_BYTES`] only, so it costs the same on any file.

use std::sync::Arc;

use encoding_rs::{UTF_16BE, UTF_16LE, WINDOWS_1252};

use super::infer::{ColType, classify};
use super::parse::{Syntax, next_record, unescape};
use super::sheet::Terminator;

/// How much of the file sniffing looks at.
pub const SAMPLE_BYTES: usize = 64 * 1024;
/// Records examined for delimiter consistency and the header guess.
const SAMPLE_RECORDS: usize = 200;
const HEADER_RECORDS: usize = 50;

/// Delimiter candidates, in tie-break preference order.
const DELIMITERS: [u8; 4] = *b",\t;|";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// The fallback for bytes that aren't valid UTF-8. WHATWG's Windows-1252
    /// maps all 256 byte values, so decoding and re-encoding is lossless.
    Windows1252,
}

/// How a quote is written inside a quoted field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// `""` (RFC 4180).
    Doubled,
    /// `\"` (and `\\` for a backslash). `""` is still read as a quote.
    Backslash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialect {
    pub encoding: Encoding,
    /// Whether the file starts with a byte-order mark (written back if so).
    pub bom: bool,
    pub delimiter: u8,
    pub quote: u8,
    pub escape: Escape,
    /// The dominant line ending: used for new and re-serialized rows that have
    /// no terminator of their own.
    pub line_ending: Terminator,
    /// Whether the last record is followed by a line ending.
    pub trailing_newline: bool,
    /// The initial header-row guess (the sheet's flag is authoritative).
    pub header: bool,
}

impl Default for Dialect {
    fn default() -> Self {
        Dialect {
            encoding: Encoding::Utf8,
            bom: false,
            delimiter: b',',
            quote: b'"',
            escape: Escape::Doubled,
            line_ending: Terminator::Lf,
            trailing_newline: true,
            header: false,
        }
    }
}

impl Dialect {
    /// The delimiter an extension implies, for save-as: `.tsv`/`.tab` → tab,
    /// `.psv` → pipe, `.csv` → comma. `None` keeps the current dialect.
    pub fn delimiter_for_extension(ext: &str) -> Option<u8> {
        match ext.to_ascii_lowercase().as_str() {
            "tsv" | "tab" => Some(b'\t'),
            "psv" => Some(b'|'),
            "csv" => Some(b','),
            _ => None,
        }
    }

    /// This dialect with another delimiter (for save-as by extension).
    pub fn with_delimiter(&self, delimiter: u8) -> Dialect {
        Dialect {
            delimiter,
            ..self.clone()
        }
    }
}

/// The encoding from a BOM, else UTF-8 if the first [`SAMPLE_BYTES`] validate,
/// else Windows-1252. Returns `(encoding, has_bom)`.
pub fn detect_encoding(bytes: &[u8]) -> (Encoding, bool) {
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        return (Encoding::Utf8, true);
    }
    if bytes.starts_with(b"\xFF\xFE") {
        return (Encoding::Utf16Le, true);
    }
    if bytes.starts_with(b"\xFE\xFF") {
        return (Encoding::Utf16Be, true);
    }
    let sample = &bytes[..bytes.len().min(SAMPLE_BYTES)];
    match std::str::from_utf8(sample) {
        Ok(_) => (Encoding::Utf8, false),
        // A multi-byte character cut off by the sample's end is still UTF-8.
        Err(e) if e.error_len().is_none() && sample.len() < bytes.len() => (Encoding::Utf8, false),
        Err(_) => (Encoding::Windows1252, false),
    }
}

pub(super) fn bom_len(encoding: Encoding, bom: bool) -> usize {
    match (encoding, bom) {
        (_, false) => 0,
        (Encoding::Utf16Le | Encoding::Utf16Be, true) => 2,
        // UTF-8's; also a UTF-8 file that fell back to Windows-1252.
        (_, true) => 3,
    }
}

/// Transcode a whole file to the UTF-8 text a sheet's cells point into, BOM
/// stripped. UTF-8 that turns out invalid past the sample is decoded as
/// Windows-1252 instead (byte-for-byte reversible), so the returned encoding
/// and BOM flag may differ from the detected ones.
pub fn decode(bytes: Vec<u8>, encoding: Encoding, bom: bool) -> (Arc<str>, Encoding, bool) {
    let skip = bom_len(encoding, bom);
    match encoding {
        Encoding::Utf8 => match std::str::from_utf8(&bytes[skip..]) {
            Ok(text) => (Arc::from(text), Encoding::Utf8, bom),
            Err(_) => {
                // Keep the BOM out of the first cell; it's written back.
                let (text, _) = WINDOWS_1252.decode_without_bom_handling(&bytes[skip..]);
                (Arc::from(&*text), Encoding::Windows1252, bom)
            }
        },
        _ => (decode_slice(&bytes[skip..], encoding), encoding, bom),
    }
}

/// Decode `bytes` (no BOM) as `encoding`. UTF-8 stops at the first invalid or
/// truncated sequence — this is for prefix windows, where the cut is expected.
pub fn decode_slice(bytes: &[u8], encoding: Encoding) -> Arc<str> {
    match encoding {
        Encoding::Utf8 => match std::str::from_utf8(bytes) {
            Ok(text) => Arc::from(text),
            Err(e) => Arc::from(std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or("")),
        },
        Encoding::Utf16Le => Arc::from(&*UTF_16LE.decode_without_bom_handling(bytes).0),
        Encoding::Utf16Be => Arc::from(&*UTF_16BE.decode_without_bom_handling(bytes).0),
        Encoding::Windows1252 => Arc::from(&*WINDOWS_1252.decode_without_bom_handling(bytes).0),
    }
}

/// Encode UTF-8 `text` back to `encoding`, prefixed with the BOM if `bom`.
///
/// encoding_rs has no UTF-16 encoder (WHATWG never encodes to UTF-16), so
/// that is done by hand. A character Windows-1252 can't represent (typed into
/// a cell) comes out as an HTML numeric reference, encoding_rs's convention.
pub fn encode(text: String, encoding: Encoding, bom: bool) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 if bom => {
            let mut out = Vec::with_capacity(text.len() + 3);
            out.extend_from_slice(b"\xEF\xBB\xBF");
            out.extend_from_slice(text.as_bytes());
            out
        }
        Encoding::Utf8 => text.into_bytes(),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let le = encoding == Encoding::Utf16Le;
            let mut out = Vec::with_capacity(text.len() * 2 + 2);
            if bom {
                out.extend_from_slice(if le { b"\xFF\xFE" } else { b"\xFE\xFF" });
            }
            for unit in text.encode_utf16() {
                out.extend_from_slice(&if le {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            out
        }
        Encoding::Windows1252 => {
            // A UTF-8 file that fell back to Windows-1252 keeps its BOM.
            let mut out = if bom {
                b"\xEF\xBB\xBF".to_vec()
            } else {
                Vec::new()
            };
            out.extend_from_slice(&WINDOWS_1252.encode(&text).0);
            out
        }
    }
}

/// The longest prefix of `s` of at most `max` bytes that ends on a char
/// boundary.
fn floor_prefix(s: &str, max: usize) -> &str {
    &s[..s.floor_char_boundary(max)]
}

fn dominant_line_ending(sample: &[u8]) -> Terminator {
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < sample.len() {
        match sample[i] {
            b'\r' if sample.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    if crlf > lf && crlf >= cr {
        Terminator::CrLf
    } else if cr > lf && cr > crlf {
        Terminator::Cr
    } else {
        Terminator::Lf
    }
}

/// `\"` is the escape when it appears and `""` never does inside a field (a
/// bare `""` field — an empty quoted value — doesn't count).
fn detect_escape(sample: &[u8], delimiter: u8) -> Escape {
    let backslash = sample.windows(2).filter(|w| w == b"\\\"").count();
    if backslash == 0 {
        return Escape::Doubled;
    }
    // Text like `"C:\"" end"` reads either way; prefer the reading under
    // which every quoted field closes where a field ends.
    let doubled_bad = misplaced_closes(sample, delimiter, Escape::Doubled);
    let backslash_bad = misplaced_closes(sample, delimiter, Escape::Backslash);
    if doubled_bad != backslash_bad {
        return if doubled_bad < backslash_bad {
            Escape::Doubled
        } else {
            Escape::Backslash
        };
    }
    let boundary =
        |b: Option<&u8>| matches!(b, None | Some(b'\n' | b'\r')) || b == Some(&delimiter);
    let doubled = (0..sample.len().saturating_sub(1))
        .filter(|&i| sample[i] == b'"' && sample[i + 1] == b'"')
        // `\""` is an escaped quote followed by the closing one.
        .filter(|&i| i == 0 || sample[i - 1] != b'\\')
        .filter(|&i| {
            let before = if i == 0 { None } else { sample.get(i - 1) };
            !(boundary(before) && boundary(sample.get(i + 2)))
        })
        .count();
    if doubled == 0 {
        Escape::Backslash
    } else {
        Escape::Doubled
    }
}

/// Quoted fields in `sample` that, read with `escape`, close somewhere other
/// than at the end of a field (or never close).
fn misplaced_closes(sample: &[u8], delimiter: u8, escape: Escape) -> usize {
    let mut bad = 0;
    let mut i = 0;
    let mut field_start = true;
    while i < sample.len() {
        let b = sample[i];
        if field_start && b == b'"' {
            i += 1;
            loop {
                match sample.get(i) {
                    None => {
                        bad += 1;
                        break;
                    }
                    Some(b'\\') if escape == Escape::Backslash => i += 2,
                    Some(b'"') if escape == Escape::Doubled && sample.get(i + 1) == Some(&b'"') => {
                        i += 2
                    }
                    Some(b'"') => {
                        i += 1;
                        if !matches!(sample.get(i), None | Some(b'\n' | b'\r'))
                            && sample.get(i) != Some(&delimiter)
                        {
                            bad += 1;
                        }
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            field_start = false;
            continue;
        }
        field_start = b == delimiter || b == b'\n' || b == b'\r';
        i += 1;
    }
    bad
}

/// Field counts of the sample's non-blank records under `syn` (dropping the
/// last record when the sample was cut mid-file, since it may be partial).
fn field_counts(sample: &str, syn: &Syntax, truncated: bool) -> Vec<usize> {
    let bytes = sample.as_bytes();
    let mut fields = Vec::new();
    let mut counts = Vec::new();
    let mut pos = 0;
    while counts.len() < SAMPLE_RECORDS {
        let Some(rec) = next_record(bytes, pos, syn, &mut fields) else {
            break;
        };
        if truncated && rec.next >= bytes.len() {
            break;
        }
        if !fields.is_empty() {
            counts.push(fields.len());
        }
        pos = rec.next;
    }
    counts
}

/// `(mode field count, fraction of records with it)`.
fn consistency(counts: &[usize]) -> (usize, f64) {
    if counts.is_empty() {
        return (0, 0.0);
    }
    let mut sorted = counts.to_vec();
    sorted.sort_unstable();
    let (mut best, mut best_n) = (sorted[0], 0);
    let mut i = 0;
    while i < sorted.len() {
        let j = sorted[i..].partition_point(|&x| x == sorted[i]) + i;
        if j - i > best_n {
            (best, best_n) = (sorted[i], j - i);
        }
        i = j;
    }
    (best, best_n as f64 / counts.len() as f64)
}

fn pick_delimiter(sample: &str, escape: Escape, ext_hint: Option<&str>, truncated: bool) -> u8 {
    let hinted = ext_hint
        .and_then(Dialect::delimiter_for_extension)
        .filter(|&d| d != b',');
    let mut best: Option<(u8, f64)> = None;
    for delim in DELIMITERS {
        let syn = Syntax::new(delim, b'"', escape);
        let (mode, score) = consistency(&field_counts(sample, &syn, truncated));
        if mode < 2 {
            continue;
        }
        // An extension that names the delimiter wins whenever it splits rows.
        if Some(delim) == hinted {
            return delim;
        }
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((delim, score));
        }
    }
    best.map(|(d, _)| d).or(hinted).unwrap_or(b',')
}

/// Row 1 is a header when it is all non-empty, non-numeric text and either
/// some column below it is numeric, or its values are all distinct (with at
/// least one row below).
fn detect_header(sample: &str, syn: &Syntax, truncated: bool) -> bool {
    let bytes = sample.as_bytes();
    let mut fields = Vec::new();
    let mut scratch = String::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut pos = 0;
    while rows.len() < HEADER_RECORDS {
        let Some(rec) = next_record(bytes, pos, syn, &mut fields) else {
            break;
        };
        if truncated && rec.next >= bytes.len() {
            break;
        }
        pos = rec.next;
        if fields.is_empty() {
            if rows.is_empty() {
                return false; // a blank first line is not a header
            }
            continue;
        }
        rows.push(
            fields
                .iter()
                .map(|f| {
                    if f.escaped {
                        unescape(&sample[f.start..f.end], syn, &mut scratch);
                        scratch.clone()
                    } else {
                        sample[f.start..f.end].to_string()
                    }
                })
                .collect(),
        );
    }
    let Some((first, rest)) = rows.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    let all_text = first
        .iter()
        .all(|v| !v.trim().is_empty() && classify(v) == ColType::Text);
    if !all_text {
        return false;
    }
    let numeric_below = (0..first.len()).any(|c| {
        let mut values = rest
            .iter()
            .filter_map(|r| r.get(c))
            .filter(|v| !v.trim().is_empty())
            .peekable();
        values.peek().is_some() && values.all(|v| classify(v).is_numeric())
    });
    let mut unique: Vec<&str> = first.iter().map(String::as_str).collect();
    unique.sort_unstable();
    unique.dedup();
    numeric_below || unique.len() == first.len()
}

/// Detect the dialect of `source` (already transcoded; `encoding`/`bom`
/// describe the original). `ends_with_newline` is passed in because `source`
/// may be only a prefix of the file.
pub fn sniff(
    source: &str,
    encoding: Encoding,
    bom: bool,
    ends_with_newline: bool,
    ext_hint: Option<&str>,
) -> Dialect {
    let sample = floor_prefix(source, SAMPLE_BYTES);
    let truncated = sample.len() < source.len();
    let bytes = sample.as_bytes();
    // Escape style first (with comma, the common case, for its boundary test);
    // the delimiter scan then honours it.
    let escape = detect_escape(bytes, b',');
    let delimiter = pick_delimiter(sample, escape, ext_hint, truncated);
    let escape = detect_escape(bytes, delimiter);
    let syn = Syntax::new(delimiter, b'"', escape);
    Dialect {
        encoding,
        bom,
        delimiter,
        quote: b'"',
        escape,
        line_ending: dominant_line_ending(bytes),
        trailing_newline: ends_with_newline,
        header: detect_header(sample, &syn, truncated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff_str(s: &str, ext: Option<&str>) -> Dialect {
        sniff(s, Encoding::Utf8, false, s.ends_with('\n'), ext)
    }

    #[test]
    fn grid_sniff_delimiters() {
        assert_eq!(sniff_str("a,b,c\n1,2,3\n4,5,6\n", None).delimiter, b',');
        assert_eq!(sniff_str("a\tb\tc\n1\t2\t3\n", None).delimiter, b'\t');
        assert_eq!(sniff_str("a;b;c\n1;2;3\n4;5;6\n", None).delimiter, b';');
        assert_eq!(sniff_str("a|b|c\n1|2|3\n", None).delimiter, b'|');
        // Commas inside quotes don't count.
        assert_eq!(
            sniff_str("name;note\n\"Smith, J\";\"a, b, c\"\nx;y\n", None).delimiter,
            b';'
        );
        // Consistency beats raw count: commas appear but vary per line.
        assert_eq!(
            sniff_str("a;b\nhello, world;2\nx;y,z,w\nq;r\n", None).delimiter,
            b';'
        );
        // Extension hint: a .tsv with a consistent comma split still uses tabs
        // when tabs split rows too.
        assert_eq!(sniff_str("a,b\tc\n1,2\t3\n", Some("tsv")).delimiter, b'\t');
        assert_eq!(sniff_str("a,b|c\n1,2|3\n", Some("psv")).delimiter, b'|');
        // One column: falls back to the hint, else comma.
        assert_eq!(sniff_str("a\nb\n", Some("tsv")).delimiter, b'\t');
        assert_eq!(sniff_str("a\nb\n", None).delimiter, b',');
    }

    #[test]
    fn grid_sniff_line_endings_and_escape() {
        let d = sniff_str("a,b\r\n1,2\r\n3,4\n", None);
        assert_eq!(d.line_ending, Terminator::CrLf);
        assert!(!sniff("a,b", Encoding::Utf8, false, false, None).trailing_newline);
        assert_eq!(sniff_str("a,b\r1,2\r", None).line_ending, Terminator::Cr);
        assert_eq!(
            sniff_str("a,\"say \"\"hi\"\"\"\n", None).escape,
            Escape::Doubled
        );
        assert_eq!(
            sniff_str("a,\"say \\\"hi\\\"\"\n", None).escape,
            Escape::Backslash
        );
        // An empty quoted field is not a doubled-quote escape.
        assert_eq!(
            sniff_str("\"\",\"x \\\" y\"\n", None).escape,
            Escape::Backslash
        );
    }

    #[test]
    fn grid_sniff_header() {
        // Text over numbers.
        assert!(sniff_str("name,age\nann,31\nbob,42\n", None).header);
        // Unique text names over text.
        assert!(sniff_str("first,last\nann,lee\nbob,ray\n", None).header);
        // Numeric first row: data.
        assert!(!sniff_str("1,2,3\n4,5,6\n", None).header);
        // Repeated, non-numeric-below first row: data.
        assert!(!sniff_str("a,a\nb,c\n", None).header);
        // Empty cell in row 1: data.
        assert!(!sniff_str("name,,x\nann,1,2\n", None).header);
        // A single row can't be judged.
        assert!(!sniff_str("name,age\n", None).header);
        // Quoted header values are unescaped before judging.
        assert!(sniff_str("\"id \"\"x\"\"\",n\na,1\n", None).header);
    }

    #[test]
    fn grid_encoding_detection_and_round_trip() {
        assert_eq!(detect_encoding(b"\xEF\xBB\xBFa,b"), (Encoding::Utf8, true));
        assert_eq!(detect_encoding(b"\xFF\xFEa\0"), (Encoding::Utf16Le, true));
        assert_eq!(detect_encoding(b"\xFE\xFF\0a"), (Encoding::Utf16Be, true));
        assert_eq!(detect_encoding("é,ü".as_bytes()), (Encoding::Utf8, false));
        assert_eq!(
            detect_encoding(b"caf\xE9,1"),
            (Encoding::Windows1252, false)
        );

        // Every byte value survives Windows-1252 decode → encode.
        let all: Vec<u8> = (0..=255).collect();
        let (text, enc, bom) = decode(all.clone(), Encoding::Windows1252, false);
        assert_eq!(encode(text.to_string(), enc, bom), all);

        // UTF-8 valid in the sample but invalid later falls back to 1252.
        let mut late = vec![b'a'; SAMPLE_BYTES + 10];
        late.push(0xFF);
        let (enc, _) = detect_encoding(&late);
        assert_eq!(enc, Encoding::Utf8);
        let (text, enc, bom) = decode(late.clone(), enc, false);
        assert_eq!(enc, Encoding::Windows1252);
        assert_eq!(encode(text.to_string(), enc, bom), late);
    }
}
