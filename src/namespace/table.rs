//! String interning via NameTable
//!
//! The NameTable provides O(1) string interning using a chained hash table.
//! All names and namespace URIs in the schema model go through this table
//! to ensure deduplication and fast equality checks via NameId.
//!
//! Design per XML_NAME_TABLE.md:
//! - Entry: {hash, next, text: Box<str>}
//! - NameId(0) reserved for empty string
//! - Rehashing when entries.len() > buckets.len()
//! - Pre-seed with standard namespaces

use crate::ids::NameId;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Entry in the name table
#[derive(Debug)]
struct Entry {
    /// Cached hash value
    hash: u64,
    /// Next entry in chain (-1 = none)
    next: i32,
    /// The interned string
    text: Box<str>,
}

/// String interning table for names and namespace URIs
///
/// Provides O(1) average-case lookup and insertion.
/// All strings in the schema model should be interned through this table.
///
/// # Example
///
/// ```
/// use xsd_schema::namespace::NameTable;
///
/// let mut table = NameTable::new();
/// let id1 = table.add("hello");
/// let id2 = table.add("hello");
/// assert_eq!(id1, id2); // Same string -> same ID
/// assert_eq!(table.resolve(id1), "hello");
/// ```
#[derive(Debug)]
pub struct NameTable {
    /// All entries (indexed by NameId)
    entries: Vec<Entry>,
    /// Hash buckets (index into entries via first entry in chain)
    buckets: Vec<i32>,
}

impl NameTable {
    /// Initial bucket count
    const INITIAL_BUCKETS: usize = 256;

    /// Create a new name table pre-seeded with standard namespaces
    pub fn new() -> Self {
        let mut table = Self {
            entries: Vec::with_capacity(Self::INITIAL_BUCKETS),
            buckets: vec![-1; Self::INITIAL_BUCKETS],
        };

        // Pre-seed standard values
        // NameId(0) = empty string (reserved)
        table.add("");

        // Standard namespace URIs
        table.add(XS_NAMESPACE);
        table.add(XSI_NAMESPACE);
        table.add(XML_NAMESPACE);
        table.add(XMLNS_NAMESPACE);

        // Standard prefixes
        table.add("xs");
        table.add("xsd");
        table.add("xsi");
        table.add("xml");
        table.add("xmlns");

        table
    }

    /// Add a string to the table, returning its NameId
    ///
    /// If the string already exists, returns the existing NameId.
    /// Otherwise, creates a new entry and returns a new NameId.
    pub fn add(&mut self, value: &str) -> NameId {
        let hash = Self::hash_str(value);

        // Check if already present
        if let Some(id) = self.find(value, hash) {
            return id;
        }

        // Need to insert
        self.insert(value, hash)
    }

    /// Get the NameId for a string if it exists
    pub fn get(&self, value: &str) -> Option<NameId> {
        let hash = Self::hash_str(value);
        self.find(value, hash)
    }

    /// Resolve a NameId to its string value
    ///
    /// # Panics
    ///
    /// Panics if the NameId is invalid (out of bounds).
    pub fn resolve(&self, id: NameId) -> &str {
        &self.entries[id.0 as usize].text
    }

    /// Try to resolve a NameId to its string value
    pub fn try_resolve(&self, id: NameId) -> Option<&str> {
        self.entries.get(id.0 as usize).map(|e| e.text.as_ref())
    }

    /// Get the number of interned strings
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the table is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Hash a string
    fn hash_str(value: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    /// Find an existing entry
    fn find(&self, value: &str, hash: u64) -> Option<NameId> {
        let bucket_idx = (hash as usize) % self.buckets.len();
        let mut entry_idx = self.buckets[bucket_idx];

        while entry_idx >= 0 {
            let entry = &self.entries[entry_idx as usize];
            if entry.hash == hash && entry.text.as_ref() == value {
                return Some(NameId(entry_idx as u32));
            }
            entry_idx = entry.next;
        }

        None
    }

    /// Insert a new entry
    fn insert(&mut self, value: &str, hash: u64) -> NameId {
        // Check if we need to rehash
        if self.entries.len() >= self.buckets.len() {
            self.rehash();
        }

        let id = NameId(self.entries.len() as u32);
        let bucket_idx = (hash as usize) % self.buckets.len();

        // Insert at head of chain
        let next = self.buckets[bucket_idx];
        self.entries.push(Entry {
            hash,
            next,
            text: value.into(),
        });
        self.buckets[bucket_idx] = id.0 as i32;

        id
    }

    /// Rehash the table (double bucket count)
    fn rehash(&mut self) {
        let new_size = self.buckets.len() * 2;
        self.buckets = vec![-1; new_size];

        // Re-insert all entries into new buckets
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            let bucket_idx = (entry.hash as usize) % new_size;
            entry.next = self.buckets[bucket_idx];
            self.buckets[bucket_idx] = idx as i32;
        }
    }
}

impl Default for NameTable {
    fn default() -> Self {
        Self::new()
    }
}

// Well-known namespace URIs
pub const XS_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";
pub const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
pub const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// Well-known NameIds (pre-seeded in NameTable::new())
pub mod well_known {
    use crate::ids::NameId;

    /// Empty string
    pub const EMPTY: NameId = NameId(0);

    /// XSD namespace URI
    pub const XS_NAMESPACE: NameId = NameId(1);

    /// XSD instance namespace URI
    pub const XSI_NAMESPACE: NameId = NameId(2);

    /// XML namespace URI
    pub const XML_NAMESPACE: NameId = NameId(3);

    /// XMLNS namespace URI
    pub const XMLNS_NAMESPACE: NameId = NameId(4);

    /// "xs" prefix
    pub const XS_PREFIX: NameId = NameId(5);

    /// "xsd" prefix
    pub const XSD_PREFIX: NameId = NameId(6);

    /// "xsi" prefix
    pub const XSI_PREFIX: NameId = NameId(7);

    /// "xml" prefix
    pub const XML_PREFIX: NameId = NameId(8);

    /// "xmlns" prefix
    pub const XMLNS_PREFIX: NameId = NameId(9);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_string_is_id_zero() {
        let table = NameTable::new();
        assert_eq!(table.resolve(NameId(0)), "");
    }

    #[test]
    fn test_add_and_resolve() {
        let mut table = NameTable::new();
        let id = table.add("hello");
        assert_eq!(table.resolve(id), "hello");
    }

    #[test]
    fn test_deduplication() {
        let mut table = NameTable::new();
        let id1 = table.add("hello");
        let id2 = table.add("hello");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_different_strings_different_ids() {
        let mut table = NameTable::new();
        let id1 = table.add("hello");
        let id2 = table.add("world");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_get_existing() {
        let mut table = NameTable::new();
        let id = table.add("test");
        assert_eq!(table.get("test"), Some(id));
    }

    #[test]
    fn test_get_nonexistent() {
        let table = NameTable::new();
        assert_eq!(table.get("nonexistent"), None);
    }

    #[test]
    fn test_well_known_namespaces() {
        let table = NameTable::new();
        assert_eq!(table.resolve(well_known::XS_NAMESPACE), XS_NAMESPACE);
        assert_eq!(table.resolve(well_known::XSI_NAMESPACE), XSI_NAMESPACE);
        assert_eq!(table.resolve(well_known::XML_NAMESPACE), XML_NAMESPACE);
        assert_eq!(table.resolve(well_known::XMLNS_NAMESPACE), XMLNS_NAMESPACE);
    }

    #[test]
    fn test_well_known_prefixes() {
        let table = NameTable::new();
        assert_eq!(table.resolve(well_known::XS_PREFIX), "xs");
        assert_eq!(table.resolve(well_known::XSD_PREFIX), "xsd");
        assert_eq!(table.resolve(well_known::XSI_PREFIX), "xsi");
        assert_eq!(table.resolve(well_known::XML_PREFIX), "xml");
    }

    #[test]
    fn test_rehashing() {
        let mut table = NameTable::new();
        // Insert enough entries to trigger rehashing
        for i in 0..1000 {
            let s = format!("string_{}", i);
            table.add(&s);
        }
        // Verify all strings can still be found
        for i in 0..1000 {
            let s = format!("string_{}", i);
            assert!(table.get(&s).is_some(), "Failed to find: {}", s);
        }
    }

    #[test]
    fn test_unicode_strings() {
        let mut table = NameTable::new();
        let id1 = table.add("日本語");
        let id2 = table.add("日本語");
        assert_eq!(id1, id2);
        assert_eq!(table.resolve(id1), "日本語");
    }
}
