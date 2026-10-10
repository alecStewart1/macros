//! `StringPool`: one stored copy of every distinct string.
//!
//! Cells that can't point into the source text (edited values, unescaped
//! quoted fields, low-cardinality columns) are interned here. Equal strings
//! always get the same `u32` id, so equality, sorting short-circuits, distinct
//! counts and the renderer's shaped-text cache can work on ids.
//!
//! Storage is an append-only arena plus a start offset per id; the hash table
//! holds only ids and re-derives each key from the arena, so a string is
//! stored exactly once (not once as a key and again as a value). Nothing is
//! ever freed — strings orphaned by edits are dropped by
//! [`Workbook::compact`](super::model::Workbook::compact), which rebuilds the pool.

use std::hash::Hasher;

use hashbrown::HashTable;
use rustc_hash::FxHasher;

/// FxHash of a string's bytes — the pool's key hash, also used by the parser
/// for its distinct-value sampling so both agree on what "equal" means.
pub fn hash_str(s: &str) -> u64 {
    let mut h = FxHasher::default();
    h.write(s.as_bytes());
    h.finish()
}

#[derive(Default)]
pub struct StringPool {
    /// Every interned string, back to back.
    arena: String,
    /// `starts[id]..starts[id + 1]` is string `id` in `arena` (one trailing
    /// sentinel). Implicit ends halve the index versus `(start, len)` pairs;
    /// `u64` because the arena may outgrow 4 GiB.
    starts: Vec<u64>,
    table: HashTable<u32>,
}

fn slice<'a>(arena: &'a str, starts: &[u64], id: u32) -> &'a str {
    let id = id as usize;
    &arena[starts[id] as usize..starts[id + 1] as usize]
}

impl StringPool {
    /// A copy that can read every string but intern no new ones (the dedup
    /// table is left out) — for read-only snapshots handed to Scheme.
    pub fn read_copy(&self) -> StringPool {
        StringPool {
            arena: self.arena.clone(),
            starts: self.starts.clone(),
            table: Default::default(),
        }
    }

    pub fn new() -> Self {
        Self {
            arena: String::new(),
            starts: vec![0],
            table: HashTable::new(),
        }
    }

    /// The id of `s`, adding it if this is its first appearance.
    pub fn intern(&mut self, s: &str) -> u32 {
        if self.starts.is_empty() {
            // `Default` builds an empty `starts`; restore the sentinel.
            self.starts.push(0);
        }
        let hash = hash_str(s);
        let (arena, starts) = (&self.arena, &self.starts);
        if let Some(&id) = self.table.find(hash, |&id| slice(arena, starts, id) == s) {
            return id;
        }
        let id = u32::try_from(self.starts.len() - 1).expect("string pool exceeds u32 ids");
        self.arena.push_str(s);
        self.starts.push(self.arena.len() as u64);
        let (arena, starts) = (&self.arena, &self.starts);
        self.table
            .insert_unique(hash, id, |&id| hash_str(slice(arena, starts, id)));
        id
    }

    /// The id of `s` if it has been interned, without adding it.
    #[allow(dead_code)] // model API kept for multi-sheet / `.xlsx`
    pub fn lookup(&self, s: &str) -> Option<u32> {
        let (arena, starts) = (&self.arena, &self.starts);
        self.table
            .find(hash_str(s), |&id| slice(arena, starts, id) == s)
            .copied()
    }

    /// String `id`. Panics on an id this pool never issued.
    pub fn get(&self, id: u32) -> &str {
        slice(&self.arena, &self.starts, id)
    }

    /// Number of distinct strings.
    pub fn len(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    #[allow(dead_code)] // model API kept for multi-sheet / `.xlsx`
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total bytes of string data held (the arena size).
    #[allow(dead_code)] // model API kept for multi-sheet / `.xlsx`
    pub fn bytes(&self) -> usize {
        self.arena.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_pool_dedupes() {
        let mut pool = StringPool::new();
        let a = pool.intern("alpha");
        let b = pool.intern("beta");
        let a2 = pool.intern("alpha");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert_eq!(pool.get(a), "alpha");
        assert_eq!(pool.get(b), "beta");
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.bytes(), "alphabeta".len());
        assert_eq!(pool.lookup("beta"), Some(b));
        assert_eq!(pool.lookup("gamma"), None);
    }

    #[test]
    fn grid_pool_many_strings_survive_rehash() {
        let mut pool = StringPool::default();
        let ids: Vec<u32> = (0..20_000)
            .map(|i| pool.intern(&format!("value-{i}")))
            .collect();
        for (i, &id) in ids.iter().enumerate() {
            assert_eq!(pool.get(id), format!("value-{i}"));
            assert_eq!(pool.intern(&format!("value-{i}")), id);
        }
        assert_eq!(pool.len(), 20_000);
        // Empty and unicode strings are ordinary keys.
        let e = pool.intern("");
        assert_eq!(pool.get(e), "");
        let u = pool.intern("日本語😀");
        assert_eq!(pool.get(u), "日本語😀");
    }
}
