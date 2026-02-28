//! QName atomization for the document buffer.
//!
//! [`QNameAtom`] is a 16-byte `Copy` struct holding interned name parts.
//! [`QNameTable`] deduplicates QNameAtoms via a chained hash table
//! (same pattern as [`NameTable`](crate::namespace::NameTable)).

use crate::ids::NameId;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Atomized qualified name — 16 bytes, `Copy`.
///
/// Two `QNameAtom` values are considered **equal** when all four fields match
/// (including `prefix`), which is the behaviour needed for table deduplication.
///
/// The manual [`Hash`] implementation, however, hashes only `local_name` and
/// `namespace_uri` because XML namespace identity ignores the prefix.
/// This means two atoms that differ only in prefix will hash to the same
/// bucket but will **not** compare as equal, so `QNameTable::atomize` will
/// store them as separate entries — which is the desired semantics (the
/// navigator needs to report the original prefix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QNameAtom {
    pub local_name: NameId,
    pub namespace_uri: NameId,
    pub prefix: NameId,
    pub local_name_hash: u32,
}

// Hash only local_name + namespace_uri (prefix is irrelevant per XML namespace identity).
// See doc-comment on the struct for rationale on the Hash/Eq asymmetry.
impl Hash for QNameAtom {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.local_name.hash(state);
        self.namespace_uri.hash(state);
    }
}

/// Sentinel at index 0 — represents "no name".
pub const EMPTY_QNAME: QNameAtom = QNameAtom {
    local_name: NameId(0),
    namespace_uri: NameId(0),
    prefix: NameId(0),
    local_name_hash: 0,
};

/// Chained hash table that deduplicates [`QNameAtom`] values.
///
/// Index 0 is always [`EMPTY_QNAME`] and is never placed into any bucket.
/// The internal hash used for bucket placement hashes **all four** fields
/// so that atoms differing only in prefix land in different buckets when
/// possible, avoiding long chains.
pub struct QNameTable {
    /// All atoms (index 0 = EMPTY_QNAME sentinel).
    atoms: Vec<QNameAtom>,
    /// Parallel chain links (-1 = end of chain).
    nexts: Vec<i32>,
    /// Bucket heads (-1 = empty bucket).
    buckets: Vec<i32>,
}

impl QNameTable {
    const INITIAL_BUCKETS: usize = 64;

    /// Creates a new table with [`EMPTY_QNAME`] at index 0.
    pub fn new() -> Self {
        let mut atoms = Vec::with_capacity(Self::INITIAL_BUCKETS);
        let mut nexts = Vec::with_capacity(Self::INITIAL_BUCKETS);

        // Sentinel at index 0 — not inserted into any bucket.
        atoms.push(EMPTY_QNAME);
        nexts.push(-1);

        Self {
            atoms,
            nexts,
            buckets: vec![-1; Self::INITIAL_BUCKETS],
        }
    }

    /// Inserts `qname` into the table if not already present, returning its index.
    ///
    /// Deduplication compares all four fields (including prefix).
    pub fn atomize(&mut self, qname: QNameAtom) -> u32 {
        let hash = Self::hash_atom(&qname);

        // Probe the chain for an exact match (all 4 fields via PartialEq).
        let bucket_idx = (hash as usize) % self.buckets.len();
        let mut entry_idx = self.buckets[bucket_idx];
        while entry_idx >= 0 {
            if self.atoms[entry_idx as usize] == qname {
                return entry_idx as u32;
            }
            entry_idx = self.nexts[entry_idx as usize];
        }

        // Rehash if load factor exceeded.
        if self.atoms.len() >= self.buckets.len() {
            self.rehash();
        }

        // Insert new entry.
        let new_idx = self.atoms.len() as u32;
        let bucket_idx = (hash as usize) % self.buckets.len();
        let head = self.buckets[bucket_idx];
        self.atoms.push(qname);
        self.nexts.push(head);
        self.buckets[bucket_idx] = new_idx as i32;

        new_idx
    }

    /// Returns the [`QNameAtom`] at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is out of range.
    #[inline]
    pub fn get(&self, idx: u32) -> QNameAtom {
        self.atoms[idx as usize]
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Hash all four fields for bucket placement.
    fn hash_atom(qname: &QNameAtom) -> u64 {
        let mut hasher = DefaultHasher::new();
        qname.local_name.hash(&mut hasher);
        qname.namespace_uri.hash(&mut hasher);
        qname.prefix.hash(&mut hasher);
        qname.local_name_hash.hash(&mut hasher);
        hasher.finish()
    }

    /// Double the bucket count and re-insert all entries (skipping index 0).
    fn rehash(&mut self) {
        let new_size = self.buckets.len() * 2;
        self.buckets = vec![-1; new_size];

        // Reset all chain links.
        for n in self.nexts.iter_mut() {
            *n = -1;
        }

        // Re-insert entries 1..len (skip sentinel at 0).
        for idx in 1..self.atoms.len() {
            let hash = Self::hash_atom(&self.atoms[idx]);
            let bucket_idx = (hash as usize) % new_size;
            self.nexts[idx] = self.buckets[bucket_idx];
            self.buckets[bucket_idx] = idx as i32;
        }
    }
}

impl Default for QNameTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_qname(local: u32, ns: u32, prefix: u32, hash: u32) -> QNameAtom {
        QNameAtom {
            local_name: NameId(local),
            namespace_uri: NameId(ns),
            prefix: NameId(prefix),
            local_name_hash: hash,
        }
    }

    #[test]
    fn empty_qname_at_index_zero() {
        let table = QNameTable::new();
        assert_eq!(table.get(0), EMPTY_QNAME);
    }

    #[test]
    fn dedup_identical_qnames() {
        let mut table = QNameTable::new();
        let q = make_qname(1, 2, 3, 100);
        let idx1 = table.atomize(q);
        let idx2 = table.atomize(q);
        assert_eq!(idx1, idx2);
        assert_eq!(idx1, 1); // first real entry after sentinel
    }

    #[test]
    fn different_qnames_different_indices() {
        let mut table = QNameTable::new();
        let q1 = make_qname(1, 2, 3, 100);
        let q2 = make_qname(4, 5, 6, 200);
        let idx1 = table.atomize(q1);
        let idx2 = table.atomize(q2);
        assert_ne!(idx1, idx2);
    }

    #[test]
    fn different_prefix_different_entry() {
        let mut table = QNameTable::new();
        let q1 = make_qname(1, 2, 3, 100);
        let q2 = make_qname(1, 2, 99, 100); // same local/ns/hash, different prefix
        let idx1 = table.atomize(q1);
        let idx2 = table.atomize(q2);
        assert_ne!(idx1, idx2, "Different prefix must produce a distinct entry");
    }

    #[test]
    fn get_round_trip() {
        let mut table = QNameTable::new();
        let q = make_qname(10, 20, 30, 42);
        let idx = table.atomize(q);
        assert_eq!(table.get(idx), q);
    }

    #[test]
    fn many_entries_trigger_rehash() {
        let mut table = QNameTable::new();
        let count = 1024;
        let mut indices = Vec::with_capacity(count);

        for i in 0..count as u32 {
            let q = make_qname(i, i + 1000, i % 5, i.wrapping_mul(2654435761));
            indices.push(table.atomize(q));
        }

        // Verify round-trip for every entry.
        for i in 0..count as u32 {
            let q = make_qname(i, i + 1000, i % 5, i.wrapping_mul(2654435761));
            assert_eq!(table.get(indices[i as usize]), q);
        }

        // Re-atomize should return the same index.
        for i in 0..count as u32 {
            let q = make_qname(i, i + 1000, i % 5, i.wrapping_mul(2654435761));
            assert_eq!(table.atomize(q), indices[i as usize]);
        }
    }

    #[test]
    fn hash_trait_excludes_prefix() {
        let q1 = make_qname(1, 2, 3, 100);
        let q2 = make_qname(1, 2, 99, 100); // only prefix differs

        let hash1 = {
            let mut h = DefaultHasher::new();
            q1.hash(&mut h);
            h.finish()
        };
        let hash2 = {
            let mut h = DefaultHasher::new();
            q2.hash(&mut h);
            h.finish()
        };

        assert_eq!(
            hash1, hash2,
            "Hash impl must ignore prefix (XML namespace identity)"
        );
    }

    #[test]
    fn hash_trait_differs_for_different_names() {
        let q1 = make_qname(1, 2, 3, 100);
        let q2 = make_qname(1, 99, 3, 100); // different namespace_uri

        let hash1 = {
            let mut h = DefaultHasher::new();
            q1.hash(&mut h);
            h.finish()
        };
        let hash2 = {
            let mut h = DefaultHasher::new();
            q2.hash(&mut h);
            h.finish()
        };

        assert_ne!(
            hash1, hash2,
            "Hash should differ when namespace_uri differs"
        );
    }
}
