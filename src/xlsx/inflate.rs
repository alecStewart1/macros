//! DEFLATE decompression (RFC 1951): stored, fixed-Huffman and
//! dynamic-Huffman blocks.
//!
//! Input comes from files of unknown origin, so every step is checked: a
//! truncated stream, an over-subscribed code, a distance reaching before the
//! start of the output or a reserved symbol is an [`InflateError`], never a
//! panic. Each loop iteration consumes input bits or returns, so a malformed
//! stream can't spin forever, and output is capped by the caller's `limit`
//! (a ZIP entry's declared size, itself at most [`MAX_OUTPUT`]) so a
//! "zip bomb" stops at the cap instead of exhausting memory.
//!
//! Huffman codes decode through a single lookup table indexed by the next
//! `max_len` input bits (bit-reversed, as DEFLATE packs codes MSB-first into
//! an LSB-first stream). Tables are at most 2^15 entries and a dynamic block
//! usually covers tens of KiB of output, so building them is cheap next to
//! decoding bit by bit.

use std::fmt;

/// The hard ceiling on one decompressed part: 1 GiB.
pub const MAX_OUTPUT: usize = 1 << 30;

/// Why a DEFLATE stream couldn't be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InflateError(pub &'static str);

impl fmt::Display for InflateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "deflate: {}", self.0)
    }
}

type Result<T> = std::result::Result<T, InflateError>;

/// Base match length for length symbols 257..=285, and their extra bits.
pub(super) const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
pub(super) const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Base distance for distance symbols 0..=29, and their extra bits.
pub(super) const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
pub(super) const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Order in which a dynamic block lists the code-length code's lengths.
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Decompress a raw DEFLATE stream (no zlib/gzip wrapper). Output longer than
/// `limit` bytes is an error. Bytes after the final block are ignored.
pub fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let limit = limit.min(MAX_OUTPUT);
    // Deflated XML is typically 5–10× smaller; start there, bounded so a
    // lying size can't make us reserve a gigabyte up front.
    let mut out = Vec::with_capacity(input.len().saturating_mul(6).min(limit).min(64 << 20));
    let mut br = Bits::new(input);
    let mut fixed: Option<(Huffman, Huffman)> = None;
    loop {
        let last = br.bits(1)? == 1;
        match br.bits(2)? {
            0 => stored(&mut br, &mut out, limit)?,
            1 => {
                let (lit, dist) = fixed.get_or_insert_with(fixed_tables);
                codes(&mut br, &mut out, limit, lit, dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut br)?;
                codes(&mut br, &mut out, limit, &lit, &dist)?;
            }
            _ => return Err(InflateError("reserved block type")),
        }
        if last {
            return Ok(out);
        }
    }
}

/// An LSB-first bit reader with a 64-bit buffer.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            pos: 0,
            buf: 0,
            cnt: 0,
        }
    }

    fn refill(&mut self) {
        while self.cnt <= 56 && self.pos < self.data.len() {
            self.buf |= (self.data[self.pos] as u64) << self.cnt;
            self.pos += 1;
            self.cnt += 8;
        }
    }

    /// The next `n` (≤ 16) bits.
    fn bits(&mut self, n: u32) -> Result<u32> {
        if self.cnt < n {
            self.refill();
            if self.cnt < n {
                return Err(InflateError("unexpected end of stream"));
            }
        }
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.cnt -= n;
        Ok(v)
    }
}

/// A canonical Huffman code as a direct lookup table: entry = `sym << 4 | len`,
/// `len == 0` marking a bit pattern no code covers (an incomplete code).
struct Huffman {
    table: Vec<u16>,
    bits: u32,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        count[0] = 0;
        // Over-subscribed codes are ambiguous; incomplete ones are legal (a
        // single distance code, say) and simply leave table holes.
        let mut left: i32 = 1;
        for &c in &count[1..] {
            left = (left << 1) - c as i32;
            if left < 0 {
                return Err(InflateError("over-subscribed Huffman code"));
            }
        }
        let max = (1..16).rev().find(|&l| count[l] > 0).unwrap_or(1) as u32;
        let mut next = [0u16; 16];
        let mut code = 0u16;
        for len in 1..16 {
            code = (code + count[len - 1]) << 1;
            next[len] = code;
        }
        let mut table = vec![0u16; 1 << max];
        for (sym, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let len = len as u32;
            let c = next[len as usize];
            next[len as usize] += 1;
            let rev = (c.reverse_bits() >> (16 - len)) as usize;
            let entry = (sym as u16) << 4 | len as u16;
            let mut i = rev;
            while i < table.len() {
                table[i] = entry;
                i += 1 << len;
            }
        }
        Ok(Huffman { table, bits: max })
    }

    fn decode(&self, br: &mut Bits) -> Result<u16> {
        if br.cnt < self.bits {
            br.refill();
        }
        // Near the end of input fewer than `bits` bits may remain; the zero
        // padding is harmless as long as the matched code fits in what's real.
        let e = self.table[(br.buf & ((1u64 << self.bits) - 1)) as usize];
        let len = (e & 15) as u32;
        if len == 0 {
            return Err(InflateError("invalid Huffman code"));
        }
        if len > br.cnt {
            return Err(InflateError("unexpected end of stream"));
        }
        br.buf >>= len;
        br.cnt -= len;
        Ok(e >> 4)
    }
}

fn stored(br: &mut Bits, out: &mut Vec<u8>, limit: usize) -> Result<()> {
    // Skip to a byte boundary; whole bytes still buffered are read through
    // `bits` below, the rest straight from the input.
    let skip = br.cnt % 8;
    br.bits(skip)?;
    let len = br.bits(16)?;
    let nlen = br.bits(16)?;
    if len != !nlen & 0xFFFF {
        return Err(InflateError("stored block length mismatch"));
    }
    let mut len = len as usize;
    if out.len() + len > limit {
        return Err(InflateError("output exceeds size limit"));
    }
    while len > 0 && br.cnt >= 8 {
        out.push(br.bits(8)? as u8);
        len -= 1;
    }
    let end = br.pos.checked_add(len).filter(|&e| e <= br.data.len());
    let Some(end) = end else {
        return Err(InflateError("unexpected end of stream"));
    };
    out.extend_from_slice(&br.data[br.pos..end]);
    br.pos = end;
    Ok(())
}

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lit = [0u8; 288];
    lit[..144].fill(8);
    lit[144..256].fill(9);
    lit[256..280].fill(7);
    lit[280..].fill(8);
    // Both are complete codes, so construction can't fail.
    let lit = Huffman::new(&lit).unwrap_or(Huffman {
        table: vec![0],
        bits: 0,
    });
    let dist = Huffman::new(&[5u8; 30]).unwrap_or(Huffman {
        table: vec![0],
        bits: 0,
    });
    (lit, dist)
}

fn dynamic_tables(br: &mut Bits) -> Result<(Huffman, Huffman)> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(InflateError("too many length or distance codes"));
    }
    let mut clen = [0u8; 19];
    for &i in &CLEN_ORDER[..hclen] {
        clen[i] = br.bits(3)? as u8;
    }
    let clcode = Huffman::new(&clen)?;
    let mut lengths = [0u8; 286 + 30];
    let total = hlit + hdist;
    let mut i = 0;
    while i < total {
        let sym = clcode.decode(br)?;
        let (value, repeat) = match sym {
            0..=15 => (sym as u8, 1),
            16 => {
                if i == 0 {
                    return Err(InflateError("repeat with no previous length"));
                }
                (lengths[i - 1], 3 + br.bits(2)? as usize)
            }
            17 => (0, 3 + br.bits(3)? as usize),
            _ => (0, 11 + br.bits(7)? as usize),
        };
        if i + repeat > total {
            return Err(InflateError("code lengths overflow"));
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        return Err(InflateError("no end-of-block code"));
    }
    let lit = Huffman::new(&lengths[..hlit])?;
    let dist = Huffman::new(&lengths[hlit..total])?;
    Ok((lit, dist))
}

fn codes(
    br: &mut Bits,
    out: &mut Vec<u8>,
    limit: usize,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<()> {
    loop {
        let sym = lit.decode(br)? as usize;
        if sym < 256 {
            if out.len() >= limit {
                return Err(InflateError("output exceeds size limit"));
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let li = sym - 257;
        if li >= LEN_BASE.len() {
            return Err(InflateError("invalid length symbol"));
        }
        let len = LEN_BASE[li] as usize + br.bits(LEN_EXTRA[li] as u32)? as usize;
        let di = dist.decode(br)? as usize;
        if di >= DIST_BASE.len() {
            return Err(InflateError("invalid distance symbol"));
        }
        let d = DIST_BASE[di] as usize + br.bits(DIST_EXTRA[di] as u32)? as usize;
        if d > out.len() {
            return Err(InflateError("distance before start of output"));
        }
        if out.len() + len > limit {
            return Err(InflateError("output exceeds size limit"));
        }
        let start = out.len() - d;
        if d >= len {
            out.extend_from_within(start..start + len);
        } else {
            // Overlapping copy: the match repeats its own output.
            for k in 0..len {
                let b = out[start + k];
                out.push(b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::crc32::crc32;
    use super::*;

    /// Raw DEFLATE streams made by Python's zlib (see `testdata/gen.py`),
    /// with the length and CRC-32 of what each must decompress to.
    const MANIFEST: &str = include_str!("testdata/inflate/manifest.txt");

    fn fixture(name: &str) -> &'static [u8] {
        match name {
            "empty" => include_bytes!("testdata/inflate/empty.deflate"),
            "stored" => include_bytes!("testdata/inflate/stored.deflate"),
            "stored_multi" => include_bytes!("testdata/inflate/stored_multi.deflate"),
            "fixed" => include_bytes!("testdata/inflate/fixed.deflate"),
            "dynamic" => include_bytes!("testdata/inflate/dynamic.deflate"),
            "long_matches" => include_bytes!("testdata/inflate/long_matches.deflate"),
            "binary" => include_bytes!("testdata/inflate/binary.deflate"),
            "mixed" => include_bytes!("testdata/inflate/mixed.deflate"),
            other => panic!("unknown fixture {other}"),
        }
    }

    fn cases() -> Vec<(&'static str, usize, u32)> {
        MANIFEST
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let mut it = l.split_whitespace();
                let name = it.next().unwrap();
                let len = it.next().unwrap().parse().unwrap();
                let crc = u32::from_str_radix(it.next().unwrap(), 16).unwrap();
                (name, len, crc)
            })
            .collect()
    }

    #[test]
    fn python_fixtures_decode() {
        let cases = cases();
        assert!(cases.len() >= 8);
        for (name, len, crc) in cases {
            let out = inflate(fixture(name), MAX_OUTPUT).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(out.len(), len, "{name}");
            assert_eq!(crc32(&out), crc, "{name}");
        }
    }

    #[test]
    fn hand_built_blocks() {
        // A stored block: final, type 0, LEN=3, NLEN=!3, "abc".
        let s = [0x01, 0x03, 0x00, 0xFC, 0xFF, b'a', b'b', b'c'];
        assert_eq!(inflate(&s, 100).unwrap(), b"abc");
        // Fixed block for "a" then end-of-block (zlib's output for b"a").
        assert_eq!(inflate(&[0x4B, 0x04, 0x00], 100).unwrap(), b"a");
    }

    #[test]
    fn corrupt_streams_error() {
        let cases: &[&[u8]] = &[
            &[],
            &[0x07],                               // reserved block type 3
            &[0x01, 0x03, 0x00, 0x00, 0x00],       // NLEN mismatch
            &[0x01, 0x05, 0x00, 0xFA, 0xFF, b'a'], // stored block truncated
            &[0x4B],                               // fixed block truncated
            &[0x03, 0x82, 0x00], // fixed: distance before start (sym 257, dist 1 w/o output)
            &[0x00],             // non-final block then nothing
        ];
        for c in cases {
            assert!(inflate(c, 1 << 20).is_err(), "{c:?}");
        }
    }

    #[test]
    fn truncations_and_flips_never_panic() {
        for (name, _, crc) in cases() {
            let data = fixture(name);
            for cut in 0..data.len().min(300) {
                let _ = inflate(&data[..cut], 1 << 24);
            }
            let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ crc as u64;
            for _ in 0..300 {
                let mut d = data.to_vec();
                if d.is_empty() {
                    break;
                }
                for _ in 0..3 {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    let i = (rng % d.len() as u64) as usize;
                    d[i] ^= 1 << (rng >> 32 & 7);
                }
                let _ = inflate(&d, 1 << 24);
            }
        }
    }

    #[test]
    fn random_garbage_never_panics() {
        let mut rng = 12345u64;
        for n in 0..2000 {
            let len = n % 97;
            let d: Vec<u8> = (0..len)
                .map(|_| {
                    rng = rng
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    (rng >> 56) as u8
                })
                .collect();
            let _ = inflate(&d, 1 << 20);
        }
    }

    #[test]
    fn output_limit_enforced() {
        let data = fixture("long_matches");
        let (_, len, _) = cases().into_iter().find(|c| c.0 == "long_matches").unwrap();
        assert!(inflate(data, len).is_ok());
        assert_eq!(
            inflate(data, len - 1),
            Err(InflateError("output exceeds size limit"))
        );
    }
}
