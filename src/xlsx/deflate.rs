//! DEFLATE compression for the writer: greedy LZ77 over a hash chain, coded
//! as one fixed-Huffman block.
//!
//! Fixed codes skip building and transmitting per-block trees, which keeps
//! this short and easy to trust; on SpreadsheetML (long runs of repeated tag
//! names) it still gets most of what zlib's default level does. The ZIP
//! writer stores an entry instead whenever this doesn't make it smaller.

use super::inflate::{DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA};

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
/// Candidates examined per position. Longer chains find slightly longer
/// matches on pathological input at a steep cost in time.
const MAX_CHAIN: usize = 48;

/// Compress `data` as a raw DEFLATE stream (no zlib/gzip wrapper).
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter {
        out: Vec::with_capacity(data.len() / 3 + 16),
        buf: 0,
        cnt: 0,
    };
    w.put(1, 1); // BFINAL
    w.put(1, 2); // BTYPE = fixed Huffman
    let mut head = vec![u32::MAX; 1 << HASH_BITS];
    let mut prev = vec![u32::MAX; WINDOW];
    let mut i = 0;
    while i < data.len() {
        let (len, dist) = longest_match(data, i, &head, &prev);
        if len >= MIN_MATCH {
            put_match(&mut w, len, dist);
            for p in i..i + len {
                insert(data, p, &mut head, &mut prev);
            }
            i += len;
        } else {
            put_literal(&mut w, data[i] as u16);
            insert(data, i, &mut head, &mut prev);
            i += 1;
        }
    }
    put_literal(&mut w, 256);
    w.finish()
}

fn hash(data: &[u8], i: usize) -> usize {
    let v = (data[i] as u32) << 16 | (data[i + 1] as u32) << 8 | data[i + 2] as u32;
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

fn insert(data: &[u8], i: usize, head: &mut [u32], prev: &mut [u32]) {
    if i + MIN_MATCH > data.len() {
        return;
    }
    let h = hash(data, i);
    prev[i % WINDOW] = head[h];
    head[h] = i as u32;
}

fn longest_match(data: &[u8], i: usize, head: &[u32], prev: &[u32]) -> (usize, usize) {
    if i + MIN_MATCH > data.len() {
        return (0, 0);
    }
    let max = (data.len() - i).min(MAX_MATCH);
    let (mut best_len, mut best_dist) = (0, 0);
    let mut cand = head[hash(data, i)];
    for _ in 0..MAX_CHAIN {
        if cand == u32::MAX {
            break;
        }
        let c = cand as usize;
        // Chains hold stale entries from slots the window has since reused;
        // anything not strictly behind `i` within the window ends the walk.
        if c >= i || i - c > WINDOW {
            break;
        }
        if data[c + best_len.min(max - 1)] == data[i + best_len.min(max - 1)] {
            let len = data[c..c + max]
                .iter()
                .zip(&data[i..i + max])
                .take_while(|(a, b)| a == b)
                .count();
            if len > best_len {
                best_len = len;
                best_dist = i - c;
                if len == max {
                    break;
                }
            }
        }
        cand = prev[c % WINDOW];
    }
    (best_len, best_dist)
}

/// Fixed-Huffman literal/length code (RFC 1951 §3.2.6) as (code, bits).
fn fixed_code(sym: u16) -> (u32, u32) {
    match sym {
        0..=143 => (0x30 + sym as u32, 8),
        144..=255 => (0x190 + (sym as u32 - 144), 9),
        256..=279 => (sym as u32 - 256, 7),
        _ => (0xC0 + (sym as u32 - 280), 8),
    }
}

fn put_literal(w: &mut BitWriter, sym: u16) {
    let (code, bits) = fixed_code(sym);
    w.put_code(code, bits);
}

fn put_match(w: &mut BitWriter, len: usize, dist: usize) {
    // The last base not above the value; 258 thus takes its own code 285.
    let li = LEN_BASE
        .iter()
        .rposition(|&b| b as usize <= len)
        .unwrap_or(0);
    put_literal(w, 257 + li as u16);
    w.put((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
    let di = DIST_BASE
        .iter()
        .rposition(|&b| b as usize <= dist)
        .unwrap_or(0);
    w.put_code(di as u32, 5);
    w.put(
        (dist - DIST_BASE[di] as usize) as u32,
        DIST_EXTRA[di] as u32,
    );
}

struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    cnt: u32,
}

impl BitWriter {
    /// `n` bits of `v`, least significant first (header fields, extra bits).
    fn put(&mut self, v: u32, n: u32) {
        self.buf |= (v as u64) << self.cnt;
        self.cnt += n;
        while self.cnt >= 8 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.cnt -= 8;
        }
    }

    /// A Huffman code, which DEFLATE packs most significant bit first.
    fn put_code(&mut self, code: u32, n: u32) {
        self.put(code.reverse_bits() >> (32 - n), n);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.cnt > 0 {
            self.out.push(self.buf as u8);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::super::inflate::{MAX_OUTPUT, inflate};
    use super::*;

    fn round_trip(data: &[u8]) -> usize {
        let packed = deflate(data);
        assert_eq!(inflate(&packed, MAX_OUTPUT).unwrap(), data);
        packed.len()
    }

    #[test]
    fn round_trips() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"ab");
        round_trip(b"abc");
        round_trip(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        round_trip(&[0u8; 100_000]);
        let all: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
        round_trip(&all);
        let mut rng = 7u64;
        let noise: Vec<u8> = (0..50_000)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng as u8
            })
            .collect();
        round_trip(&noise);
        // Repeats farther apart than the window must not match across it.
        let mut far = noise.clone();
        far.extend_from_slice(&noise[..40_000]);
        round_trip(&far);
    }

    #[test]
    fn compresses_spreadsheet_xml() {
        let mut xml = String::new();
        for r in 1..2000 {
            xml.push_str(&format!(
                "<row r=\"{r}\"><c r=\"A{r}\"><v>{}</v></c><c r=\"B{r}\" t=\"inlineStr\"><is><t>item {}</t></is></c></row>",
                r * 7,
                r % 13
            ));
        }
        let n = round_trip(xml.as_bytes());
        assert!(n * 4 < xml.len(), "{n} vs {}", xml.len());
    }

    #[test]
    fn every_length_and_distance_code() {
        // Matches of every length 3..=258 at assorted distances.
        let mut data = Vec::new();
        let mut rng = 99u64;
        for len in 3..=258usize {
            let start = data.len();
            for _ in 0..(len % 50 + 1) {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                data.push(rng as u8);
            }
            let from = start.saturating_sub(len * 37 % 30_000);
            for k in 0..len {
                let b = data[from + k % (data.len() - from)];
                data.push(b);
            }
        }
        round_trip(&data);
    }
}
