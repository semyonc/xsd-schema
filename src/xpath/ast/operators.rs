// ============================================================================
// Operators
// ============================================================================

/// Range expression (`expr to expr`).
#[derive(Debug, Clone)]
pub struct RangeNode {
    /// Start of range.
    pub start: AstNodeId,
    /// End of range.
    pub end: AstNodeId,
    /// Source location.
    pub span: SourceSpan,
}

impl RangeNode {
    pub fn new(start: AstNodeId, end: AstNodeId, span: SourceSpan) -> Self {
        Self { start, end, span }
    }
}

/// Unary operator kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOpKind {
    /// Unary minus (`-expr`).
    Negate,
    /// Unary plus (`+expr`).
    Plus,
}

/// Unary operator expression.
#[derive(Debug, Clone)]
pub struct UnaryOpNode {
    /// Operator kind.
    pub kind: UnaryOpKind,
    /// Operand expression.
    pub operand: AstNodeId,
    /// Source location.
    pub span: SourceSpan,
}

impl UnaryOpNode {
    pub fn new(kind: UnaryOpKind, operand: AstNodeId, span: SourceSpan) -> Self {
        Self {
            kind,
            operand,
            span,
        }
    }
}

/// Binary operator kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOpKind {
    // Logical
    /// `or`
    Or,
    /// `and`
    And,

    // General comparisons
    /// `=`
    GeneralEq,
    /// `!=`
    GeneralNe,
    /// `<`
    GeneralLt,
    /// `<=`
    GeneralLe,
    /// `>`
    GeneralGt,
    /// `>=`
    GeneralGe,

    // Value comparisons
    /// `eq`
    ValueEq,
    /// `ne`
    ValueNe,
    /// `lt`
    ValueLt,
    /// `le`
    ValueLe,
    /// `gt`
    ValueGt,
    /// `ge`
    ValueGe,

    // Node comparisons
    /// `is`
    Is,
    /// `<<`
    Before,
    /// `>>`
    After,

    // Arithmetic
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `div`
    Div,
    /// `idiv`
    IDiv,
    /// `mod`
    Mod,

    // Sequence operations
    /// `union` or `|`
    Union,
    /// `intersect`
    Intersect,
    /// `except`
    Except,
}

/// Binary operator expression.
#[derive(Debug, Clone)]
pub struct BinaryOpNode {
    /// Operator kind.
    pub kind: BinaryOpKind,
    /// Left operand.
    pub left: AstNodeId,
    /// Right operand.
    pub right: AstNodeId,
    /// Source location.
    pub span: SourceSpan,
}

impl BinaryOpNode {
    pub fn new(kind: BinaryOpKind, left: AstNodeId, right: AstNodeId, span: SourceSpan) -> Self {
        Self {
            kind,
            left,
            right,
            span,
        }
    }
}

