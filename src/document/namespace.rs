//! Namespace chain storage for the document buffer.
//!
//! [`NamespaceNode`] is a 16-byte linked-list node holding a single namespace
//! binding (prefix → namespace URI).  Chains are stored in a page-based arena
//! ([`NamespacePageFactory`]) that mirrors the [`NodePages`](super::page::NodePages)
//! pattern.

use std::cell::Cell;

use bumpalo::Bump;

use crate::ids::NameId;

// ── NsRef ─────────────────────────────────────────────────────────────

/// Opaque index into a [`NamespacePageFactory`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NsRef(pub u32);

impl Default for NsRef {
    /// Default is the NULL sentinel, not slot 0.
    fn default() -> Self {
        Self::NULL
    }
}

impl NsRef {
    /// Sentinel value that marks end-of-chain or "no namespace".
    pub const NULL: NsRef = NsRef(u32::MAX);

    /// Returns `true` if this reference is the NULL sentinel.
    #[inline]
    pub fn is_null(self) -> bool {
        self == Self::NULL
    }
}

// ── Page-addressing constants ─────────────────────────────────────────

/// Page size exponent: 2^12 = 4096 namespace nodes per page.
pub const NS_PAGE_SHIFT: u32 = 12;

/// Namespace nodes per page (4096).
pub const NS_PAGE_SIZE: u32 = 1 << NS_PAGE_SHIFT;

/// Bitmask for extracting the slot within a page.
pub const NS_PAGE_MASK: u32 = NS_PAGE_SIZE - 1; // 0xFFF

// ── NamespaceNode ─────────────────────────────────────────────────────

/// A single namespace binding in a linked chain — 12 bytes.
///
/// Each element that declares namespace bindings stores an [`NsRef`] head
/// that points to the first `NamespaceNode` in a chain.  The `next` field
/// links to the following binding (or [`NsRef::NULL`] for end-of-chain).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamespaceNode {
    pub prefix: NameId,
    pub namespace_uri: NameId,
    pub next: NsRef,
}

impl Default for NamespaceNode {
    fn default() -> Self {
        Self {
            prefix: NameId(0),
            namespace_uri: NameId(0),
            next: NsRef::NULL,
        }
    }
}

impl NamespaceNode {
    /// Creates a new namespace node.
    pub fn new(prefix: NameId, namespace_uri: NameId, next: NsRef) -> Self {
        Self {
            prefix,
            namespace_uri,
            next,
        }
    }
}

// ── NamespacePageFactory ──────────────────────────────────────────────

/// Arena-backed page array of [`Cell<NamespaceNode>`].
///
/// Mirrors [`NodePages`](super::page::NodePages) but for namespace bindings.
pub struct NamespacePageFactory<'a> {
    arena: &'a Bump,
    pages: Vec<&'a [Cell<NamespaceNode>]>,
    len: u32,
}

/// Returns the page number for a flat namespace index.
#[inline]
fn page_of(ns_ref: NsRef) -> u32 {
    ns_ref.0 >> NS_PAGE_SHIFT
}

/// Returns the slot (offset within page) for a flat namespace index.
#[inline]
fn slot_of(ns_ref: NsRef) -> u32 {
    ns_ref.0 & NS_PAGE_MASK
}

impl<'a> NamespacePageFactory<'a> {
    /// Creates a new factory with the first page pre-allocated.
    pub fn new(arena: &'a Bump) -> Self {
        let first_page = Self::make_page(arena);
        Self {
            arena,
            pages: vec![first_page],
            len: 0,
        }
    }

    /// Allocates the next namespace slot and returns its [`NsRef`].
    ///
    /// Returns `None` if the index would reach `u32::MAX` (reserved as NULL).
    pub fn alloc(&mut self) -> Option<NsRef> {
        let idx = self.len;
        if idx == u32::MAX {
            return None;
        }
        self.len = idx + 1;
        if idx > 0 && slot_of(NsRef(idx)) == 0 {
            self.allocate_page();
        }
        Some(NsRef(idx))
    }

    /// Reads the namespace node at the given reference.
    #[inline]
    pub fn get(&self, ns_ref: NsRef) -> NamespaceNode {
        let page = page_of(ns_ref) as usize;
        let slot = slot_of(ns_ref) as usize;
        self.pages[page][slot].get()
    }

    /// Writes a namespace node at the given reference (interior mutability).
    #[inline]
    pub fn set(&self, ns_ref: NsRef, node: NamespaceNode) {
        let page = page_of(ns_ref) as usize;
        let slot = slot_of(ns_ref) as usize;
        self.pages[page][slot].set(node);
    }

    /// Returns the total number of allocated namespace nodes.
    #[inline]
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Returns `true` if no namespace nodes have been allocated.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns an iterator over the chain starting at `head`.
    ///
    /// Yields `(prefix, namespace_uri)` pairs.  An [`NsRef::NULL`] head
    /// produces an empty iterator.
    pub fn iter_chain(&self, head: NsRef) -> NamespaceChain<'_, 'a> {
        NamespaceChain {
            factory: self,
            cursor: head,
        }
    }

    fn allocate_page(&mut self) {
        let page = Self::make_page(self.arena);
        self.pages.push(page);
    }

    fn make_page(arena: &Bump) -> &[Cell<NamespaceNode>] {
        arena.alloc_slice_fill_with(NS_PAGE_SIZE as usize, |_| {
            Cell::new(NamespaceNode::default())
        })
    }
}

// ── NamespaceChain iterator ───────────────────────────────────────────

/// Iterator over a linked chain of namespace bindings.
pub struct NamespaceChain<'f, 'a> {
    factory: &'f NamespacePageFactory<'a>,
    cursor: NsRef,
}

impl Iterator for NamespaceChain<'_, '_> {
    type Item = (NameId, NameId);

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor.is_null() {
            return None;
        }
        let node = self.factory.get(self.cursor);
        self.cursor = node.next;
        Some((node.prefix, node.namespace_uri))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    #[test]
    fn nsref_null_is_u32_max() {
        assert_eq!(NsRef::NULL.0, u32::MAX);
    }

    #[test]
    fn nsref_default_is_null() {
        assert_eq!(NsRef::default(), NsRef::NULL);
        assert!(NsRef::default().is_null());
    }

    #[test]
    fn nsref_is_null() {
        assert!(NsRef::NULL.is_null());
        assert!(!NsRef(0).is_null());
        assert!(!NsRef(42).is_null());
    }

    #[test]
    fn end_of_chain_is_null() {
        let node = NamespaceNode::new(NameId(1), NameId(2), NsRef::NULL);
        assert!(node.next.is_null());
    }

    #[test]
    fn namespace_node_is_12_bytes() {
        assert_eq!(mem::size_of::<NamespaceNode>(), 12);
    }

    #[test]
    fn sequential_alloc() {
        let arena = Bump::new();
        let mut factory = NamespacePageFactory::new(&arena);
        for expected in 0..10u32 {
            let ns_ref = factory.alloc().unwrap();
            assert_eq!(ns_ref.0, expected);
        }
    }

    #[test]
    fn set_get_round_trip() {
        let arena = Bump::new();
        let mut factory = NamespacePageFactory::new(&arena);
        let r = factory.alloc().unwrap();

        let node = NamespaceNode::new(NameId(10), NameId(20), NsRef::NULL);
        factory.set(r, node);
        assert_eq!(factory.get(r), node);
    }

    #[test]
    fn cross_page_allocation() {
        let arena = Bump::new();
        let mut factory = NamespacePageFactory::new(&arena);

        let count = NS_PAGE_SIZE + 1;
        for i in 0..count {
            let r = factory.alloc().unwrap();
            assert_eq!(r.0, i);
            factory.set(
                r,
                NamespaceNode::new(NameId(i), NameId(i + 1000), NsRef::NULL),
            );
        }
        assert_eq!(factory.len(), count);

        // Verify last node on first page
        let last_first = NsRef(NS_PAGE_SIZE - 1);
        assert_eq!(factory.get(last_first).prefix, NameId(NS_PAGE_SIZE - 1));

        // Verify first node on second page
        let first_second = NsRef(NS_PAGE_SIZE);
        assert_eq!(factory.get(first_second).prefix, NameId(NS_PAGE_SIZE));
    }

    #[test]
    fn single_node_chain() {
        let arena = Bump::new();
        let mut factory = NamespacePageFactory::new(&arena);
        let r = factory.alloc().unwrap();
        factory.set(r, NamespaceNode::new(NameId(1), NameId(2), NsRef::NULL));

        let items: Vec<_> = factory.iter_chain(r).collect();
        assert_eq!(items, vec![(NameId(1), NameId(2))]);
    }

    #[test]
    fn two_node_chain() {
        let arena = Bump::new();
        let mut factory = NamespacePageFactory::new(&arena);

        let r1 = factory.alloc().unwrap();
        let r0 = factory.alloc().unwrap();

        // Build chain: r0 -> r1 -> NULL
        factory.set(r1, NamespaceNode::new(NameId(3), NameId(4), NsRef::NULL));
        factory.set(r0, NamespaceNode::new(NameId(1), NameId(2), r1));

        let items: Vec<_> = factory.iter_chain(r0).collect();
        assert_eq!(items, vec![(NameId(1), NameId(2)), (NameId(3), NameId(4))]);
    }

    #[test]
    fn multi_node_chain() {
        let arena = Bump::new();
        let mut factory = NamespacePageFactory::new(&arena);

        let r2 = factory.alloc().unwrap();
        let r1 = factory.alloc().unwrap();
        let r0 = factory.alloc().unwrap();

        factory.set(r2, NamespaceNode::new(NameId(5), NameId(6), NsRef::NULL));
        factory.set(r1, NamespaceNode::new(NameId(3), NameId(4), r2));
        factory.set(r0, NamespaceNode::new(NameId(1), NameId(2), r1));

        let items: Vec<_> = factory.iter_chain(r0).collect();
        assert_eq!(
            items,
            vec![
                (NameId(1), NameId(2)),
                (NameId(3), NameId(4)),
                (NameId(5), NameId(6)),
            ]
        );
    }

    #[test]
    fn null_chain_yields_empty() {
        let arena = Bump::new();
        let factory = NamespacePageFactory::new(&arena);
        let items: Vec<_> = factory.iter_chain(NsRef::NULL).collect();
        assert!(items.is_empty());
    }

    #[test]
    fn len_and_is_empty() {
        let arena = Bump::new();
        let mut factory = NamespacePageFactory::new(&arena);
        assert_eq!(factory.len(), 0);
        assert!(factory.is_empty());

        factory.alloc().unwrap();
        assert_eq!(factory.len(), 1);
        assert!(!factory.is_empty());
    }
}
