//! The packed 8-byte cell reference.
//!
//! A million-row, 20-column sheet is 20M cells, so the per-cell cost decides
//! whether a 100 MB file fits in ~130 MB of memory. Every cell is one
//! [`CellRef`], and most never own a heap allocation: they either point back
//! into the sheet's source text (Span) or carry their bytes inline.
//!
//! Layout (as a little-endian `u64`; byte 7 is the high byte):
//!
//! ```text
//! bits 62..64  tag: 00 Empty, 01 Span, 10 Inline, 11 Interned
//! Span:     bits 0..40 offset into the sheet source, bits 40..62 length
//! Inline:   bytes 0..7 UTF-8 text, bits 56..59 (byte 7, low 3 bits) length
//! Interned: bytes 0..4 the StringPool id
//! ```
//!
//! It is a byte array rather than a `u64` so an Inline cell can hand out a
//! `&str` borrowed straight from its own bytes. Numbers are always decoded via
//! `u64::from_le_bytes`, so the layout is the same on every host.

use super::intern::StringPool;

const TAG_EMPTY: u8 = 0;
const TAG_SPAN: u8 = 1;
const TAG_INLINE: u8 = 2;
const TAG_INTERNED: u8 = 3;

/// Longest string stored inside the reference itself.
pub const INLINE_MAX: usize = 7;
/// Largest source offset a Span can address (40 bits — 1 TiB).
pub const SPAN_MAX_OFFSET: usize = (1 << 40) - 1;
/// Longest Span (22 bits — just under 4 MiB). Longer cells are interned.
pub const SPAN_MAX_LEN: usize = (1 << 22) - 1;

const OFFSET_MASK: u64 = (1 << 40) - 1;
const LEN_MASK: u64 = (1 << 22) - 1;

/// One cell, packed into 8 bytes (see the module doc for the layout).
///
/// All zeroes is Empty, so `CellRef::default()` and zero-filled storage are
/// empty cells. Equal Inline strings have equal bytes (unused bytes are zero),
/// so `==` on two Inline cells is string equality; across kinds it is not.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CellRef([u8; 8]);

/// A decoded view of a [`CellRef`], for code that cares how a cell is stored
/// (the writer's span check, compaction, the shaping-cache key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Empty,
    Span { offset: usize, len: usize },
    Inline,
    Interned(u32),
}

impl CellRef {
    pub const EMPTY: CellRef = CellRef([0; 8]);

    fn word(self) -> u64 {
        u64::from_le_bytes(self.0)
    }

    fn tag(self) -> u8 {
        self.0[7] >> 6
    }

    /// `s` stored inline, or `None` if it is longer than [`INLINE_MAX`].
    pub fn inline(s: &str) -> Option<CellRef> {
        if s.len() > INLINE_MAX {
            return None;
        }
        let mut bytes = [0u8; 8];
        bytes[..s.len()].copy_from_slice(s.as_bytes());
        bytes[7] = (TAG_INLINE << 6) | s.len() as u8;
        Some(CellRef(bytes))
    }

    /// A reference to `source[offset..offset + len]`, or `None` if either
    /// field overflows its bit width (the caller interns instead).
    pub fn span(offset: usize, len: usize) -> Option<CellRef> {
        if offset > SPAN_MAX_OFFSET || len > SPAN_MAX_LEN {
            return None;
        }
        let word = ((TAG_SPAN as u64) << 62) | ((len as u64) << 40) | offset as u64;
        Some(CellRef(word.to_le_bytes()))
    }

    /// A reference to string `id` in the workbook's [`StringPool`].
    pub fn interned(id: u32) -> CellRef {
        let word = ((TAG_INTERNED as u64) << 62) | id as u64;
        CellRef(word.to_le_bytes())
    }

    /// The canonical reference for an owned value (an edit, an unescaped
    /// field): Empty for `""`, Inline when short, otherwise interned.
    pub fn from_value(s: &str, pool: &mut StringPool) -> CellRef {
        if s.is_empty() {
            CellRef::EMPTY
        } else if let Some(cell) = CellRef::inline(s) {
            cell
        } else {
            CellRef::interned(pool.intern(s))
        }
    }

    pub fn kind(self) -> CellKind {
        let word = self.word();
        match self.tag() {
            TAG_EMPTY => CellKind::Empty,
            TAG_SPAN => CellKind::Span {
                offset: (word & OFFSET_MASK) as usize,
                len: ((word >> 40) & LEN_MASK) as usize,
            },
            TAG_INLINE => CellKind::Inline,
            _ => CellKind::Interned(word as u32),
        }
    }

    /// The pool id, if this cell is interned. Equal ids are equal strings,
    /// which lets sorting and dedupe skip the string compare.
    pub fn interned_id(self) -> Option<u32> {
        (self.tag() == TAG_INTERNED).then(|| self.word() as u32)
    }

    /// Whether the cell's value is `""`. Constructors never produce an empty
    /// Inline or Span, but a zero length is still treated as empty.
    pub fn is_empty(self) -> bool {
        match self.kind() {
            CellKind::Empty => true,
            CellKind::Span { len, .. } => len == 0,
            CellKind::Inline => self.0[7] & 0b111 == 0,
            CellKind::Interned(_) => false,
        }
    }

    /// The cell's text. Span cells borrow from `source` (the owning sheet's
    /// text), Interned cells from `pool`, Inline cells from `self`.
    pub fn get<'a>(&'a self, source: &'a str, pool: &'a StringPool) -> &'a str {
        match self.kind() {
            CellKind::Empty => "",
            // Slicing a `str` checks the char boundaries: a stale or foreign
            // span panics instead of producing invalid UTF-8.
            CellKind::Span { offset, len } => &source[offset..offset + len],
            CellKind::Inline => {
                let len = (self.0[7] & 0b111) as usize;
                // SAFETY: Inline cells are only built by `CellRef::inline`,
                // which copies the bytes of a `&str` (whole, ≤ 7 bytes), so
                // `self.0[..len]` is exactly that valid UTF-8 string.
                unsafe { std::str::from_utf8_unchecked(&self.0[..len]) }
            }
            CellKind::Interned(id) => pool.get(id),
        }
    }
}

impl std::fmt::Debug for CellRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind() {
            CellKind::Inline => {
                let len = (self.0[7] & 0b111) as usize;
                write!(f, "Inline({:?})", String::from_utf8_lossy(&self.0[..len]))
            }
            kind => write!(f, "{kind:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_cellref_is_8_bytes_and_zero_is_empty() {
        assert_eq!(std::mem::size_of::<CellRef>(), 8);
        assert_eq!(CellRef::default(), CellRef::EMPTY);
        assert_eq!(CellRef::EMPTY.kind(), CellKind::Empty);
        assert!(CellRef::EMPTY.is_empty());
    }

    #[test]
    fn grid_cellref_inline_edges() {
        let pool = StringPool::new();
        // Zero-length inline decodes to "" and counts as empty.
        let zero = CellRef::inline("").unwrap();
        assert_eq!(zero.kind(), CellKind::Inline);
        assert_eq!(zero.get("", &pool), "");
        assert!(zero.is_empty());
        // Exactly 7 bytes fits; 8 does not.
        let seven = CellRef::inline("abcdefg").unwrap();
        assert_eq!(seven.get("", &pool), "abcdefg");
        assert!(CellRef::inline("abcdefgh").is_none());
        // Multibyte UTF-8 totalling exactly 7 bytes: "é" (2) + "日" (3) + "ab".
        let s = "é日ab";
        assert_eq!(s.len(), 7);
        assert_eq!(CellRef::inline(s).unwrap().get("", &pool), s);
        // 4-byte char + 3-byte char = 7 bytes.
        let s = "😀日";
        assert_eq!(CellRef::inline(s).unwrap().get("", &pool), s);
        // Equal inline strings are equal references.
        assert_eq!(CellRef::inline("NY"), CellRef::inline("NY"));
        assert_ne!(CellRef::inline("NY"), CellRef::inline("NJ"));
    }

    #[test]
    fn grid_cellref_span_edges() {
        let pool = StringPool::new();
        let src = "hello, world";
        let c = CellRef::span(7, 5).unwrap();
        assert_eq!(c.kind(), CellKind::Span { offset: 7, len: 5 });
        assert_eq!(c.get(src, &pool), "world");
        // Maximum representable fields round-trip.
        let max = CellRef::span(SPAN_MAX_OFFSET, SPAN_MAX_LEN).unwrap();
        assert_eq!(
            max.kind(),
            CellKind::Span {
                offset: SPAN_MAX_OFFSET,
                len: SPAN_MAX_LEN
            }
        );
        // One past either limit falls back (the caller interns).
        assert!(CellRef::span(SPAN_MAX_OFFSET + 1, 1).is_none());
        assert!(CellRef::span(0, SPAN_MAX_LEN + 1).is_none());
        assert!(CellRef::span(0, 0).unwrap().is_empty());
    }

    #[test]
    fn grid_cellref_interned_and_from_value() {
        let mut pool = StringPool::new();
        assert_eq!(CellRef::from_value("", &mut pool), CellRef::EMPTY);
        assert_eq!(
            CellRef::from_value("short", &mut pool).kind(),
            CellKind::Inline
        );
        let a = CellRef::from_value("a longer value", &mut pool);
        let b = CellRef::from_value("a longer value", &mut pool);
        assert_eq!(a.interned_id(), b.interned_id());
        assert_eq!(a.get("", &pool), "a longer value");
        let max = CellRef::interned(u32::MAX);
        assert_eq!(max.kind(), CellKind::Interned(u32::MAX));
        assert_eq!(CellRef::inline("x").unwrap().interned_id(), None);
    }
}
