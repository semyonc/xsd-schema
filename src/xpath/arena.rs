//! Arena-based storage for XPath AST nodes.
//!
//! This module provides an arena allocator for AST nodes, allowing efficient
//! storage and reference by ID. This approach avoids recursive ownership issues
//! and enables efficient tree manipulation.

use crate::xpath::ast::AstNode;

/// Unique identifier for an AST node within an arena.
///
/// This is a simple index into the arena's node vector. The value 0 is reserved
/// as a sentinel for "no node" in some contexts.
pub type AstNodeId = u32;

/// Sentinel value indicating no node (used for optional references).
pub const NO_NODE: AstNodeId = u32::MAX;

/// Source location span within the input string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceSpan {
    /// Start byte offset (inclusive).
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
}

impl SourceSpan {
    /// Create a new source span.
    #[inline]
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Create an empty span at a single position.
    #[inline]
    pub fn at(pos: usize) -> Self {
        Self {
            start: pos,
            end: pos,
        }
    }

    /// Merge two spans into one that covers both.
    #[inline]
    pub fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Check if this span is empty (zero length).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// Get the length of this span in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

/// Arena for storing AST nodes.
///
/// Nodes are stored in a contiguous vector and referenced by `AstNodeId`.
/// This enables efficient allocation and avoids recursive Box structures.
#[derive(Debug, Default, Clone)]
pub struct AstArena {
    nodes: Vec<AstNode>,
}

impl AstArena {
    /// Create a new empty arena.
    #[inline]
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Create an arena with pre-allocated capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(capacity),
        }
    }

    /// Add a node to the arena and return its ID.
    #[inline]
    pub fn add(&mut self, node: AstNode) -> AstNodeId {
        let id = self.nodes.len() as AstNodeId;
        self.nodes.push(node);
        id
    }

    /// Get a reference to a node by ID.
    ///
    /// # Panics
    /// Panics if the ID is out of bounds.
    #[inline]
    pub fn get(&self, id: AstNodeId) -> &AstNode {
        &self.nodes[id as usize]
    }

    /// Get a mutable reference to a node by ID.
    ///
    /// # Panics
    /// Panics if the ID is out of bounds.
    #[inline]
    pub fn get_mut(&mut self, id: AstNodeId) -> &mut AstNode {
        &mut self.nodes[id as usize]
    }

    /// Try to get a reference to a node by ID.
    #[inline]
    pub fn try_get(&self, id: AstNodeId) -> Option<&AstNode> {
        self.nodes.get(id as usize)
    }

    /// Try to get a mutable reference to a node by ID.
    #[inline]
    pub fn try_get_mut(&mut self, id: AstNodeId) -> Option<&mut AstNode> {
        self.nodes.get_mut(id as usize)
    }

    /// Get the number of nodes in the arena.
    #[inline]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if the arena is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Clear all nodes from the arena.
    #[inline]
    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Iterate over all nodes in the arena.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (AstNodeId, &AstNode)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (i as AstNodeId, n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xpath::ast::{AstNode, ValueNode};

    #[test]
    fn test_source_span() {
        let span = SourceSpan::new(10, 20);
        assert_eq!(span.start, 10);
        assert_eq!(span.end, 20);
        assert_eq!(span.len(), 10);
        assert!(!span.is_empty());

        let empty = SourceSpan::at(5);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
    }

    #[test]
    fn test_span_merge() {
        let a = SourceSpan::new(10, 20);
        let b = SourceSpan::new(15, 30);
        let merged = a.merge(b);
        assert_eq!(merged.start, 10);
        assert_eq!(merged.end, 30);
    }

    #[test]
    fn test_arena_basic() {
        let mut arena = AstArena::new();
        assert!(arena.is_empty());

        let id1 = arena.add(AstNode::Value(ValueNode::Empty));
        let id2 = arena.add(AstNode::Value(ValueNode::Boolean(true)));

        assert_eq!(arena.len(), 2);
        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
    }

    #[test]
    fn test_arena_get() {
        let mut arena = AstArena::new();
        let id = arena.add(AstNode::Value(ValueNode::String("test".to_string())));

        match arena.get(id) {
            AstNode::Value(ValueNode::String(s)) => assert_eq!(s, "test"),
            _ => panic!("Unexpected node type"),
        }
    }

    #[test]
    fn test_arena_try_get() {
        let arena = AstArena::new();
        assert!(arena.try_get(0).is_none());
        assert!(arena.try_get(100).is_none());
    }

    #[test]
    fn test_arena_iter() {
        let mut arena = AstArena::new();
        arena.add(AstNode::Value(ValueNode::Empty));
        arena.add(AstNode::Value(ValueNode::Boolean(true)));

        let ids: Vec<_> = arena.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![0, 1]);
    }
}
