use crate::navigator::DomNodeType;

// ── Page-addressing constants ──────────────────────────────────────────

/// Page size exponent: 2^12 = 4096 nodes per page.
pub const PAGE_SHIFT: u32 = 12;

/// Nodes per page (4096). Each page = 4096 × 16 bytes = 64 KB.
pub const PAGE_SIZE: u32 = 1 << PAGE_SHIFT;

/// Bitmask for extracting the slot within a page.
pub const PAGE_MASK: u32 = PAGE_SIZE - 1; // 0xFFF

/// NULL sentinel — never a valid node index.
pub const NULL: u32 = u32::MAX;

// ── Page-addressing helpers ────────────────────────────────────────────

/// Returns the page number for a flat node index.
#[inline]
pub fn page_of(node_ref: u32) -> u32 {
    node_ref >> PAGE_SHIFT
}

/// Returns the slot (offset within page) for a flat node index.
#[inline]
pub fn slot_of(node_ref: u32) -> u32 {
    node_ref & PAGE_MASK
}

/// Constructs a flat node index from a page number and slot.
///
/// # Panics (debug only)
///
/// Panics if `slot >= PAGE_SIZE`, which would alias bits into the page portion.
#[inline]
pub fn node_ref_from(page: u32, slot: u32) -> u32 {
    debug_assert!(
        slot < PAGE_SIZE,
        "slot {slot} >= PAGE_SIZE ({PAGE_SIZE})"
    );
    (page << PAGE_SHIFT) | slot
}

// ── NodeType ───────────────────────────────────────────────────────────

/// Discriminant stored in the low 4 bits of [`Node::props_type`].
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeType {
    /// Sentinel: marks end of document.
    #[default]
    Nul = 0,
    /// Document root.
    Root = 1,
    /// Element node (value = QNameAtom index).
    Element = 2,
    /// Attribute name node of a two-node pair (value = QNameAtom index).
    Attribute = 3,
    /// String content of a two-node pair (attribute value or PI data).
    ChildValue = 4,
    /// Text node (value = string index).
    Text = 5,
    /// Whitespace-only text node.
    Whitespace = 6,
    /// Schema-significant whitespace.
    SignificantWhitespace = 7,
    /// Comment node (value = string index).
    Comment = 8,
    /// Processing instruction target (value = string index); data in next ChildValue.
    ProcessingInstruction = 9,
}

impl From<NodeType> for DomNodeType {
    fn from(nt: NodeType) -> Self {
        match nt {
            NodeType::Root => DomNodeType::Root,
            NodeType::Element => DomNodeType::Element,
            NodeType::Attribute => DomNodeType::Attribute,
            NodeType::Text => DomNodeType::Text,
            NodeType::Whitespace => DomNodeType::Whitespace,
            NodeType::SignificantWhitespace => DomNodeType::SignificantWhitespace,
            NodeType::Comment => DomNodeType::Comment,
            NodeType::ProcessingInstruction => DomNodeType::ProcessingInstruction,
            NodeType::Nul | NodeType::ChildValue => {
                unreachable!("Nul and ChildValue have no DomNodeType equivalent")
            }
        }
    }
}

impl NodeType {
    /// Decode from the low 4 bits of a `props_type` value.
    #[inline]
    fn from_bits(bits: u32) -> Self {
        match bits & Node::NODE_TYPE_MASK {
            0 => NodeType::Nul,
            1 => NodeType::Root,
            2 => NodeType::Element,
            3 => NodeType::Attribute,
            4 => NodeType::ChildValue,
            5 => NodeType::Text,
            6 => NodeType::Whitespace,
            7 => NodeType::SignificantWhitespace,
            8 => NodeType::Comment,
            9 => NodeType::ProcessingInstruction,
            _ => NodeType::Nul, // reserved values → Nul
        }
    }
}

// ── Node ───────────────────────────────────────────────────────────────

/// 16-byte flat node in the `BufferDocument` node array.
///
/// Layout of `props_type` (32 bits):
/// - Bits \[3:0\]  — [`NodeType`] discriminant (4 bits)
/// - Bits \[7:4\]  — property flags (`HAS_ATTRIBUTE`, `HAS_CHILDREN`, `IS_COMPLEX_TYPE`, `HAS_NMSP_DECLS`)
/// - Bits \[31:8\] — 24-bit type index into `TypeRemapTable` (0 = untyped)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// Index of the next sibling node, or [`NULL`].
    pub next_sibling: u32,
    /// Index of the parent node, or [`NULL`].
    pub parent: u32,
    /// Packed field: node type (4 bits) | flags (4 bits) | type_index (24 bits).
    pub props_type: u32,
    /// Interpretation depends on node type (QNameAtom index, string index, etc.).
    pub value: u32,
}

impl Node {
    // ── Bitmask constants ──────────────────────────────────────────────

    const NODE_TYPE_MASK: u32 = 0x0F; // bits [3:0]
    const TYPE_INDEX_SHIFT: u32 = 8; // bits [31:8]

    /// Element has attribute children.
    pub const HAS_ATTRIBUTE: u32 = 0x10;
    /// Element/Root has content children.
    pub const HAS_CHILDREN: u32 = 0x20;
    /// `type_index` references a complex type in the remap table.
    pub const IS_COMPLEX_TYPE: u32 = 0x40;
    /// Element declares namespace bindings.
    pub const HAS_NMSP_DECLS: u32 = 0x80;

    // ── Accessors ──────────────────────────────────────────────────────

    /// Returns the [`NodeType`] stored in the low 4 bits.
    #[inline]
    pub fn node_type(self) -> NodeType {
        NodeType::from_bits(self.props_type)
    }

    /// Returns the raw flag nibble (bits \[7:4\]).
    #[inline]
    pub fn flags(self) -> u32 {
        self.props_type & 0xF0
    }

    /// Returns the 24-bit type index (bits \[31:8\]).
    #[inline]
    pub fn type_index(self) -> u32 {
        self.props_type >> Self::TYPE_INDEX_SHIFT
    }

    /// Overwrites the [`NodeType`] in bits \[3:0\], preserving other fields.
    #[inline]
    pub fn set_node_type(&mut self, nt: NodeType) {
        self.props_type = (self.props_type & !Self::NODE_TYPE_MASK) | (nt as u32);
    }

    /// Sets a flag bit (e.g. [`HAS_ATTRIBUTE`](Self::HAS_ATTRIBUTE)).
    #[inline]
    pub fn set_flag(&mut self, flag: u32) {
        self.props_type |= flag;
    }

    /// Clears a flag bit.
    #[inline]
    pub fn clear_flag(&mut self, flag: u32) {
        self.props_type &= !flag;
    }

    /// Tests whether a flag bit is set.
    #[inline]
    pub fn has_flag(self, flag: u32) -> bool {
        self.props_type & flag != 0
    }

    /// Sets the 24-bit type index in bits \[31:8\].
    ///
    /// # Panics (debug only)
    ///
    /// Panics if `idx` exceeds 24 bits (> 0xFF_FFFF).
    #[inline]
    pub fn set_type_index(&mut self, idx: u32) {
        debug_assert!(
            idx <= 0xFF_FFFF,
            "type_index {idx} exceeds 24-bit range"
        );
        self.props_type =
            (self.props_type & 0xFF) | (idx << Self::TYPE_INDEX_SHIFT);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    #[test]
    fn node_size_is_16_bytes() {
        assert_eq!(mem::size_of::<Node>(), 16);
    }

    #[test]
    fn node_is_copy_clone_default() {
        let a = Node::default();
        let b = a; // Copy
        #[allow(clippy::clone_on_copy)]
        let c = a.clone(); // Clone — intentionally testing the Clone impl
        assert_eq!(a, b);
        assert_eq!(a, c);
        // Default zeroes
        assert_eq!(a.next_sibling, 0);
        assert_eq!(a.parent, 0);
        assert_eq!(a.props_type, 0);
        assert_eq!(a.value, 0);
    }

    #[test]
    fn node_type_round_trip() {
        let variants = [
            NodeType::Nul,
            NodeType::Root,
            NodeType::Element,
            NodeType::Attribute,
            NodeType::ChildValue,
            NodeType::Text,
            NodeType::Whitespace,
            NodeType::SignificantWhitespace,
            NodeType::Comment,
            NodeType::ProcessingInstruction,
        ];
        for nt in variants {
            let mut node = Node::default();
            node.set_node_type(nt);
            assert_eq!(node.node_type(), nt, "round-trip failed for {nt:?}");
        }
    }

    #[test]
    fn node_type_preserves_other_bits() {
        let mut node = Node::default();
        node.set_flag(Node::HAS_CHILDREN);
        node.set_type_index(42);
        node.set_node_type(NodeType::Element);
        assert_eq!(node.node_type(), NodeType::Element);
        assert!(node.has_flag(Node::HAS_CHILDREN));
        assert_eq!(node.type_index(), 42);
    }

    #[test]
    fn flags_set_clear_has() {
        let mut node = Node::default();
        assert!(!node.has_flag(Node::HAS_ATTRIBUTE));
        assert!(!node.has_flag(Node::HAS_CHILDREN));
        assert!(!node.has_flag(Node::IS_COMPLEX_TYPE));
        assert!(!node.has_flag(Node::HAS_NMSP_DECLS));

        node.set_flag(Node::HAS_ATTRIBUTE);
        assert!(node.has_flag(Node::HAS_ATTRIBUTE));

        node.set_flag(Node::HAS_CHILDREN);
        assert!(node.has_flag(Node::HAS_CHILDREN));
        assert!(node.has_flag(Node::HAS_ATTRIBUTE)); // still set

        node.clear_flag(Node::HAS_ATTRIBUTE);
        assert!(!node.has_flag(Node::HAS_ATTRIBUTE));
        assert!(node.has_flag(Node::HAS_CHILDREN)); // unchanged
    }

    #[test]
    fn flags_nibble() {
        let mut node = Node::default();
        node.set_flag(Node::HAS_ATTRIBUTE | Node::HAS_NMSP_DECLS);
        assert_eq!(node.flags(), Node::HAS_ATTRIBUTE | Node::HAS_NMSP_DECLS);
    }

    #[test]
    fn type_index_set_get() {
        let mut node = Node::default();
        node.set_node_type(NodeType::Element);
        node.set_flag(Node::HAS_CHILDREN);

        node.set_type_index(0);
        assert_eq!(node.type_index(), 0);

        node.set_type_index(1);
        assert_eq!(node.type_index(), 1);

        node.set_type_index(0xFF_FFFF); // max 24-bit
        assert_eq!(node.type_index(), 0xFF_FFFF);

        // Verify node_type and flags are preserved
        assert_eq!(node.node_type(), NodeType::Element);
        assert!(node.has_flag(Node::HAS_CHILDREN));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "type_index")]
    fn type_index_overflow_panics_in_debug() {
        let mut node = Node::default();
        node.set_type_index(0x0100_0000); // 25 bits — too large
    }

    #[test]
    fn page_addressing_round_trip() {
        // First node on first page
        assert_eq!(page_of(0), 0);
        assert_eq!(slot_of(0), 0);
        assert_eq!(node_ref_from(0, 0), 0);

        // Last slot on first page
        assert_eq!(page_of(PAGE_SIZE - 1), 0);
        assert_eq!(slot_of(PAGE_SIZE - 1), PAGE_SIZE - 1);
        assert_eq!(node_ref_from(0, PAGE_SIZE - 1), PAGE_SIZE - 1);

        // First slot on second page
        assert_eq!(page_of(PAGE_SIZE), 1);
        assert_eq!(slot_of(PAGE_SIZE), 0);
        assert_eq!(node_ref_from(1, 0), PAGE_SIZE);

        // Arbitrary position
        let page = 7u32;
        let slot = 123u32;
        let r = node_ref_from(page, slot);
        assert_eq!(page_of(r), page);
        assert_eq!(slot_of(r), slot);
    }

    #[test]
    fn null_sentinel() {
        assert_eq!(NULL, u32::MAX);
        // NULL should decode to a very large page, not page 0
        assert_ne!(page_of(NULL), 0);
    }

    #[test]
    fn dom_node_type_conversion() {
        assert_eq!(DomNodeType::from(NodeType::Root), DomNodeType::Root);
        assert_eq!(DomNodeType::from(NodeType::Element), DomNodeType::Element);
        assert_eq!(DomNodeType::from(NodeType::Attribute), DomNodeType::Attribute);
        assert_eq!(DomNodeType::from(NodeType::Text), DomNodeType::Text);
        assert_eq!(DomNodeType::from(NodeType::Whitespace), DomNodeType::Whitespace);
        assert_eq!(
            DomNodeType::from(NodeType::SignificantWhitespace),
            DomNodeType::SignificantWhitespace
        );
        assert_eq!(DomNodeType::from(NodeType::Comment), DomNodeType::Comment);
        assert_eq!(
            DomNodeType::from(NodeType::ProcessingInstruction),
            DomNodeType::ProcessingInstruction
        );
    }

    #[test]
    #[should_panic(expected = "Nul and ChildValue")]
    fn dom_node_type_nul_panics() {
        let _ = DomNodeType::from(NodeType::Nul);
    }

    #[test]
    #[should_panic(expected = "Nul and ChildValue")]
    fn dom_node_type_child_value_panics() {
        let _ = DomNodeType::from(NodeType::ChildValue);
    }

    #[test]
    fn node_type_default_is_nul() {
        assert_eq!(NodeType::default(), NodeType::Nul);
    }

    #[test]
    fn page_constants() {
        assert_eq!(PAGE_SHIFT, 12);
        assert_eq!(PAGE_SIZE, 4096);
        assert_eq!(PAGE_MASK, 0xFFF);
        assert_eq!(1u32 << PAGE_SHIFT, PAGE_SIZE);
        assert_eq!(PAGE_SIZE - 1, PAGE_MASK);
    }
}
