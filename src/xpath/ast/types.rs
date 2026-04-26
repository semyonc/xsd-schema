// ============================================================================
// Type Expressions
// ============================================================================

use super::{AstNodeId, KindTest, QName, SourceSpan};

/// Kind of type expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeExprKind {
    /// `expr instance of type`
    InstanceOf,
    /// `expr treat as type`
    TreatAs,
    /// `expr cast as type`
    CastAs,
    /// `expr castable as type`
    CastableAs,
}

/// Type expression (`instance of`, `treat as`, `cast as`, `castable as`).
#[derive(Debug, Clone)]
pub struct TypeExprNode {
    /// Kind of type expression.
    pub kind: TypeExprKind,
    /// Operand expression.
    pub operand: AstNodeId,
    /// Target type (AST form with raw strings).
    pub target_type: SequenceTypeNode,
    /// Source location.
    pub span: SourceSpan,
    /// Resolved atomic type QName (populated during binding for Atomic item types).
    /// Uses interned NameIds and resolved namespace URIs.
    pub resolved_atomic_type: Option<crate::namespace::qname::QualifiedName>,
}

impl TypeExprNode {
    pub fn new(
        kind: TypeExprKind,
        operand: AstNodeId,
        target_type: SequenceTypeNode,
        span: SourceSpan,
    ) -> Self {
        Self {
            kind,
            operand,
            target_type,
            span,
            resolved_atomic_type: None,
        }
    }
}

/// Occurrence indicator for sequence types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OccurrenceIndicator {
    /// Exactly one (no indicator).
    #[default]
    One,
    /// Zero or one (`?`).
    ZeroOrOne,
    /// Zero or more (`*`).
    ZeroOrMore,
    /// One or more (`+`).
    OneOrMore,
}

/// Sequence type (`item-type occurrence-indicator?`).
#[derive(Debug, Clone)]
pub struct SequenceTypeNode {
    /// Item type (None = `empty-sequence()`).
    pub item_type: Option<ItemTypeNode>,
    /// Occurrence indicator.
    pub occurrence: OccurrenceIndicator,
    /// Source location.
    pub span: SourceSpan,
}

impl SequenceTypeNode {
    /// `empty-sequence()`
    pub fn empty(span: SourceSpan) -> Self {
        Self {
            item_type: None,
            occurrence: OccurrenceIndicator::One,
            span,
        }
    }

    /// Single item type with optional occurrence.
    pub fn single(
        item_type: ItemTypeNode,
        occurrence: OccurrenceIndicator,
        span: SourceSpan,
    ) -> Self {
        Self {
            item_type: Some(item_type),
            occurrence,
            span,
        }
    }
}

/// Item type in a sequence type.
#[derive(Debug, Clone)]
pub enum ItemTypeNode {
    /// `item()` - any item.
    Item,
    /// Atomic type (QName like `xs:integer`).
    Atomic(QName),
    /// Kind test (`node()`, `element()`, etc.).
    Kind(KindTest),
}
