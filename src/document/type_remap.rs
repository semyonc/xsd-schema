//! Type remap table for mapping 24-bit node indices to full [`TypeKey`] values.
//!
//! Each element/attribute node in the [`BufferDocument`](super) carries a 24-bit
//! `type_index` field.  The [`TypeRemapTable`] maps those compact indices back
//! to arena-based [`TypeKey`] values used by the schema model.

use crate::ids::TypeKey;
use std::collections::HashMap;

/// Maps compact 24-bit indices ↔ full [`TypeKey`] values.
///
/// Index 0 is a dummy placeholder (returned as `None` by [`get`](Self::get))
/// and is never stored in the dedup map.
pub struct TypeRemapTable {
    /// Indexed entries (index 0 = dummy placeholder).
    entries: Vec<TypeKey>,
    /// Reverse lookup for deduplication.
    dedup: HashMap<TypeKey, u32>,
}

impl TypeRemapTable {
    /// Creates a new table with a dummy placeholder at index 0.
    pub fn new() -> Self {
        use crate::ids::SimpleTypeKey;
        use slotmap::Key;

        // Dummy TypeKey at index 0 — never returned by `get`.
        let dummy = TypeKey::Simple(SimpleTypeKey::null());

        Self {
            entries: vec![dummy],
            dedup: HashMap::new(),
        }
    }

    /// Registers a [`TypeKey`] and returns its 24-bit index.
    ///
    /// If `tk` was already registered, returns the existing index.
    ///
    /// # Panics (debug only)
    ///
    /// Panics if the table exceeds 2^24 entries.
    pub fn register(&mut self, tk: TypeKey) -> u32 {
        if let Some(&idx) = self.dedup.get(&tk) {
            return idx;
        }
        let idx = self.entries.len() as u32;
        debug_assert!(
            idx <= 0xFF_FFFF,
            "TypeRemapTable index {idx} exceeds 24-bit range"
        );
        self.entries.push(tk);
        self.dedup.insert(tk, idx);
        idx
    }

    /// Returns the [`TypeKey`] for the given index, or `None` for index 0.
    pub fn get(&self, idx: u32) -> Option<TypeKey> {
        if idx == 0 {
            return None;
        }
        self.entries.get(idx as usize).copied()
    }
}

impl Default for TypeRemapTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ComplexTypeKey, SimpleTypeKey, TypeKey};
    use slotmap::SlotMap;

    /// Helper: create a real `SimpleTypeKey` via slotmap.
    fn make_simple_key() -> SimpleTypeKey {
        let mut sm = SlotMap::with_key();
        sm.insert(())
    }

    /// Helper: create a real `ComplexTypeKey` via slotmap.
    fn make_complex_key() -> ComplexTypeKey {
        let mut sm = SlotMap::with_key();
        sm.insert(())
    }

    #[test]
    fn index_zero_returns_none() {
        let table = TypeRemapTable::new();
        assert_eq!(table.get(0), None);
    }

    #[test]
    fn register_and_get_simple() {
        let mut table = TypeRemapTable::new();
        let sk = make_simple_key();
        let tk = TypeKey::Simple(sk);

        let idx = table.register(tk);
        assert_eq!(idx, 1);
        assert_eq!(table.get(idx), Some(tk));
    }

    #[test]
    fn register_and_get_complex() {
        let mut table = TypeRemapTable::new();
        let ck = make_complex_key();
        let tk = TypeKey::Complex(ck);

        let idx = table.register(tk);
        assert_eq!(idx, 1);
        assert_eq!(table.get(idx), Some(tk));
    }

    #[test]
    fn register_same_key_twice_returns_same_index() {
        let mut table = TypeRemapTable::new();
        let sk = make_simple_key();
        let tk = TypeKey::Simple(sk);

        let idx1 = table.register(tk);
        let idx2 = table.register(tk);
        assert_eq!(idx1, idx2);
    }

    #[test]
    fn simple_vs_complex_preserved() {
        let mut table = TypeRemapTable::new();
        let sk = make_simple_key();
        let ck = make_complex_key();
        let tk_s = TypeKey::Simple(sk);
        let tk_c = TypeKey::Complex(ck);

        let idx_s = table.register(tk_s);
        let idx_c = table.register(tk_c);
        assert_ne!(idx_s, idx_c);

        match table.get(idx_s) {
            Some(TypeKey::Simple(_)) => {}
            other => panic!("Expected Simple, got {other:?}"),
        }
        match table.get(idx_c) {
            Some(TypeKey::Complex(_)) => {}
            other => panic!("Expected Complex, got {other:?}"),
        }
    }

    #[test]
    fn multiple_distinct_keys_distinct_indices() {
        let mut table = TypeRemapTable::new();

        // Use a single SlotMap so each insert produces a distinct key.
        let mut sm: SlotMap<SimpleTypeKey, ()> = SlotMap::with_key();
        let mut indices = Vec::new();
        for _ in 0..10 {
            let sk = sm.insert(());
            indices.push(table.register(TypeKey::Simple(sk)));
        }

        // All indices should be unique.
        let unique: std::collections::HashSet<_> = indices.iter().collect();
        assert_eq!(unique.len(), indices.len());
    }

    #[test]
    fn get_out_of_range_returns_none() {
        let table = TypeRemapTable::new();
        assert_eq!(table.get(999), None);
    }
}
