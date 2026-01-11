//! XPath 2.0 Abstract Syntax Tree (AST) definitions.
//!
//! This module defines the AST node types for the XPath 2.0 parser.
//! All types are stubbed (fields defined but minimal behavior) to enable
//! parser development. Full evaluation semantics will be added later.

use crate::xpath::arena::{AstNodeId, SourceSpan};

// ============================================================================
// Core AST Node Enum
// ============================================================================

/// The main AST node type, encompassing all possible expression types.
#[derive(Debug, Clone)]
pub enum AstNode {
    /// Comma-separated sequence of expressions.
    Expr(ExprNode),
    /// Literal value (string, number, boolean, empty).
    Value(ValueNode),
    /// Context item reference (`.`).
    ContextItem(ContextItemNode),
    /// Variable reference (`$name`).
    VarRef(VarRefNode),
    /// Conditional expression (`if ... then ... else`).
    If(IfNode),
    /// For expression (`for $x in ... return`).
    For(ForNode),
    /// Quantified expression (`some`/`every ... satisfies`).
    Quantified(QuantifiedNode),
    /// Function call (`name(...)`).
    FunctionCall(FunctionCallNode),
    /// Path expression (absolute or relative).
    PathExpr(PathExprNode),
    /// Filter expression (`expr[predicate]`).
    FilterExpr(FilterExprNode),
    /// Range expression (`expr to expr`).
    Range(RangeNode),
    /// Unary operator (`+` or `-`).
    UnaryOp(UnaryOpNode),
    /// Binary operator (arithmetic, comparison, logical, etc.).
    BinaryOp(BinaryOpNode),
    /// Single path step (axis + node test + predicates).
    PathStep(PathStepNode),
    /// Type expression (instance of, treat as, cast as, castable as).
    TypeExpr(TypeExprNode),
}

// ============================================================================
// Expression Nodes
// ============================================================================

/// Comma-separated sequence of expressions.
///
/// In XPath, `(a, b, c)` creates a sequence containing results of a, b, and c.
#[derive(Debug, Clone)]
pub struct ExprNode {
    /// List of expression IDs in the sequence.
    pub items: Vec<AstNodeId>,
    /// Source location.
    pub span: SourceSpan,
}

impl ExprNode {
    /// Create a sequence with a single expression.
    pub fn single(id: AstNodeId, span: SourceSpan) -> Self {
        Self {
            items: vec![id],
            span,
        }
    }

    /// Create a sequence with multiple expressions.
    pub fn sequence(items: Vec<AstNodeId>, span: SourceSpan) -> Self {
        Self { items, span }
    }

    /// Append an expression to the sequence.
    pub fn append(&mut self, id: AstNodeId, end: usize) {
        self.items.push(id);
        self.span.end = end;
    }
}

/// Literal value node.
#[derive(Debug, Clone)]
pub enum ValueNode {
    /// Empty sequence `()`.
    Empty,
    /// String literal.
    String(String),
    /// Boolean literal (used internally, not in XPath syntax).
    Boolean(bool),
    /// Integer literal (stored as string for arbitrary precision).
    Integer(String),
    /// Decimal literal (stored as string for arbitrary precision).
    Decimal(String),
    /// Double literal (stored as string to preserve exact representation).
    Double(String),
}

/// Context item reference (`.`).
#[derive(Debug, Clone)]
pub struct ContextItemNode {
    /// Source location.
    pub span: SourceSpan,
}

impl ContextItemNode {
    pub fn new(span: SourceSpan) -> Self {
        Self { span }
    }
}

/// Variable reference (`$prefix:localname` or `$localname`).
#[derive(Debug, Clone)]
pub struct VarRefNode {
    /// Namespace prefix (empty string if none).
    pub prefix: String,
    /// Local name.
    pub local_name: String,
    /// Resolved variable slot (set during binding phase).
    pub slot: Option<u32>,
    /// Source location.
    pub span: SourceSpan,
}

impl VarRefNode {
    pub fn new(prefix: String, local_name: String, span: SourceSpan) -> Self {
        Self {
            prefix,
            local_name,
            slot: None,
            span,
        }
    }
}

// ============================================================================
// Control Flow Nodes
// ============================================================================

/// Conditional expression (`if (test) then expr else expr`).
#[derive(Debug, Clone)]
pub struct IfNode {
    /// Test/condition expression.
    pub test: AstNodeId,
    /// Then branch expression.
    pub then_branch: AstNodeId,
    /// Else branch expression.
    pub else_branch: AstNodeId,
    /// Source location.
    pub span: SourceSpan,
}

impl IfNode {
    pub fn new(
        test: AstNodeId,
        then_branch: AstNodeId,
        else_branch: AstNodeId,
        span: SourceSpan,
    ) -> Self {
        Self {
            test,
            then_branch,
            else_branch,
            span,
        }
    }
}

/// Variable binding in a for/quantified expression.
#[derive(Debug, Clone)]
pub struct ForBinding {
    /// Variable name prefix.
    pub prefix: String,
    /// Variable local name.
    pub local_name: String,
    /// Expression producing the sequence to iterate.
    pub in_expr: AstNodeId,
    /// Resolved variable slot (set during binding phase).
    pub slot: Option<u32>,
    /// Source location.
    pub span: SourceSpan,
}

impl ForBinding {
    pub fn new(prefix: String, local_name: String, in_expr: AstNodeId, span: SourceSpan) -> Self {
        Self {
            prefix,
            local_name,
            in_expr,
            slot: None,
            span,
        }
    }
}

/// For expression (`for $x in expr return expr`).
///
/// Multiple bindings: `for $x in xs, $y in ys return ...`
#[derive(Debug, Clone)]
pub struct ForNode {
    /// List of variable bindings.
    pub bindings: Vec<ForBinding>,
    /// Return expression.
    pub return_expr: AstNodeId,
    /// Source location.
    pub span: SourceSpan,
}

impl ForNode {
    pub fn new(bindings: Vec<ForBinding>, return_expr: AstNodeId, span: SourceSpan) -> Self {
        Self {
            bindings,
            return_expr,
            span,
        }
    }
}

/// Kind of quantified expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantifierKind {
    /// `some $x in ... satisfies`
    Some,
    /// `every $x in ... satisfies`
    Every,
}

/// Quantified expression (`some`/`every $x in expr satisfies expr`).
#[derive(Debug, Clone)]
pub struct QuantifiedNode {
    /// Quantifier kind (some or every).
    pub kind: QuantifierKind,
    /// List of variable bindings.
    pub bindings: Vec<ForBinding>,
    /// Satisfies expression.
    pub satisfies: AstNodeId,
    /// Source location.
    pub span: SourceSpan,
}

impl QuantifiedNode {
    pub fn new(
        kind: QuantifierKind,
        bindings: Vec<ForBinding>,
        satisfies: AstNodeId,
        span: SourceSpan,
    ) -> Self {
        Self {
            kind,
            bindings,
            satisfies,
            span,
        }
    }
}

// ============================================================================
// Function Call
// ============================================================================

/// Function call expression (`prefix:name(args...)`).
#[derive(Debug, Clone)]
pub struct FunctionCallNode {
    /// Namespace prefix (empty string if none, defaults to fn namespace).
    pub prefix: String,
    /// Function local name.
    pub local_name: String,
    /// Argument expressions.
    pub args: Vec<AstNodeId>,
    /// Source location.
    pub span: SourceSpan,
}

impl FunctionCallNode {
    pub fn new(prefix: String, local_name: String, args: Vec<AstNodeId>, span: SourceSpan) -> Self {
        Self {
            prefix,
            local_name,
            args,
            span,
        }
    }
}

// ============================================================================
// Path Expressions
// ============================================================================

/// XPath axis specifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// `child::` (default for element names)
    Child,
    /// `descendant::`
    Descendant,
    /// `attribute::` (abbreviated `@`)
    Attribute,
    /// `self::`
    SelfAxis,
    /// `descendant-or-self::`
    DescendantOrSelf,
    /// `following-sibling::`
    FollowingSibling,
    /// `following::`
    Following,
    /// `parent::` (abbreviated `..`)
    Parent,
    /// `ancestor::`
    Ancestor,
    /// `preceding-sibling::`
    PrecedingSibling,
    /// `preceding::`
    Preceding,
    /// `ancestor-or-self::`
    AncestorOrSelf,
    /// `namespace::`
    Namespace,
}

impl Axis {
    /// Check if this is a reverse axis (traverses in reverse document order).
    pub fn is_reverse(&self) -> bool {
        matches!(
            self,
            Axis::Parent
                | Axis::Ancestor
                | Axis::PrecedingSibling
                | Axis::Preceding
                | Axis::AncestorOrSelf
        )
    }

    /// Check if this is a forward axis.
    pub fn is_forward(&self) -> bool {
        !self.is_reverse()
    }
}

/// Node test in a path step.
#[derive(Debug, Clone)]
pub enum NodeTest {
    /// Name test (QName with optional wildcards).
    Name(NameTest),
    /// Kind test (`node()`, `element()`, etc.).
    Kind(KindTest),
}

/// Name test with optional wildcards.
#[derive(Debug, Clone)]
pub struct NameTest {
    /// Namespace prefix (None = wildcard `*:local`, Some("") = no prefix).
    pub prefix: Option<String>,
    /// Local name (None = wildcard `prefix:*` or `*`).
    pub local_name: Option<String>,
}

impl NameTest {
    /// Match any node: `*`.
    pub fn any() -> Self {
        Self {
            prefix: None,
            local_name: None,
        }
    }

    /// Match any local name in a namespace: `prefix:*`.
    pub fn any_in_ns(prefix: String) -> Self {
        Self {
            prefix: Some(prefix),
            local_name: None,
        }
    }

    /// Match any namespace with a specific local name: `*:local`.
    pub fn any_ns(local_name: String) -> Self {
        Self {
            prefix: None,
            local_name: Some(local_name),
        }
    }

    /// Match a specific QName.
    pub fn qname(prefix: String, local_name: String) -> Self {
        Self {
            prefix: Some(prefix),
            local_name: Some(local_name),
        }
    }
}

/// Kind test (`node()`, `text()`, `element()`, etc.).
#[derive(Debug, Clone)]
pub enum KindTest {
    /// `node()` - matches any node.
    AnyKind,
    /// `text()` - matches text nodes.
    Text,
    /// `comment()` - matches comment nodes.
    Comment,
    /// `processing-instruction()` or `processing-instruction('name')`.
    ProcessingInstruction(Option<String>),
    /// `document-node()` or `document-node(element(...))`.
    Document(Option<Box<KindTest>>),
    /// `element()` or `element(name)` or `element(name, type)`.
    Element(ElementTest),
    /// `attribute()` or `attribute(name)` or `attribute(name, type)`.
    Attribute(AttributeTest),
    /// `schema-element(name)`.
    SchemaElement(String),
    /// `schema-attribute(name)`.
    SchemaAttribute(String),
}

/// Element test: `element()`, `element(name)`, or `element(name, type)`.
#[derive(Debug, Clone, Default)]
pub struct ElementTest {
    /// Element name (None = wildcard).
    pub name: Option<QName>,
    /// Type annotation (None = any type).
    pub type_name: Option<QName>,
    /// Whether the type allows nilled elements.
    pub nillable: bool,
}

/// Attribute test: `attribute()`, `attribute(name)`, or `attribute(name, type)`.
#[derive(Debug, Clone, Default)]
pub struct AttributeTest {
    /// Attribute name (None = wildcard).
    pub name: Option<QName>,
    /// Type annotation (None = any type).
    pub type_name: Option<QName>,
}

/// Qualified name (prefix:local or just local).
#[derive(Debug, Clone)]
pub struct QName {
    /// Namespace prefix (empty string if none).
    pub prefix: String,
    /// Local part.
    pub local: String,
}

impl QName {
    pub fn new(prefix: String, local: String) -> Self {
        Self { prefix, local }
    }

    pub fn local_only(local: String) -> Self {
        Self {
            prefix: String::new(),
            local,
        }
    }
}

/// Single step in a path expression.
#[derive(Debug, Clone)]
pub struct PathStepNode {
    /// Axis specifier.
    pub axis: Axis,
    /// Node test.
    pub test: NodeTest,
    /// Predicates (expression IDs).
    pub predicates: Vec<AstNodeId>,
    /// Source location.
    pub span: SourceSpan,
}

impl PathStepNode {
    pub fn new(axis: Axis, test: NodeTest, span: SourceSpan) -> Self {
        Self {
            axis,
            test,
            predicates: Vec::new(),
            span,
        }
    }

    pub fn with_predicates(
        axis: Axis,
        test: NodeTest,
        predicates: Vec<AstNodeId>,
        span: SourceSpan,
    ) -> Self {
        Self {
            axis,
            test,
            predicates,
            span,
        }
    }

    /// Abbreviated parent step (`..`).
    pub fn abbrev_parent(span: SourceSpan) -> Self {
        Self {
            axis: Axis::Parent,
            test: NodeTest::Kind(KindTest::AnyKind),
            predicates: Vec::new(),
            span,
        }
    }
}

/// Path expression (sequence of steps).
#[derive(Debug, Clone)]
pub struct PathExprNode {
    /// Whether the path starts from root (`/`).
    pub is_absolute: bool,
    /// Steps in the path (IDs of PathStep nodes or filter expressions).
    pub steps: Vec<AstNodeId>,
    /// Source location.
    pub span: SourceSpan,
}

impl PathExprNode {
    /// Root-only path (`/`).
    pub fn root_only(span: SourceSpan) -> Self {
        Self {
            is_absolute: true,
            steps: Vec::new(),
            span,
        }
    }

    /// Absolute path (`/a/b`).
    pub fn absolute(steps: Vec<AstNodeId>, span: SourceSpan) -> Self {
        Self {
            is_absolute: true,
            steps,
            span,
        }
    }

    /// Relative path (`a/b`).
    pub fn relative(steps: Vec<AstNodeId>, span: SourceSpan) -> Self {
        Self {
            is_absolute: false,
            steps,
            span,
        }
    }
}

/// Filter expression (`primary[predicate][predicate]...`).
#[derive(Debug, Clone)]
pub struct FilterExprNode {
    /// Base/primary expression.
    pub base: AstNodeId,
    /// Predicate expressions.
    pub predicates: Vec<AstNodeId>,
    /// Source location.
    pub span: SourceSpan,
}

impl FilterExprNode {
    pub fn new(base: AstNodeId, predicates: Vec<AstNodeId>, span: SourceSpan) -> Self {
        Self {
            base,
            predicates,
            span,
        }
    }
}

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

// ============================================================================
// Type Expressions
// ============================================================================

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
    /// Target type.
    pub target_type: SequenceTypeNode,
    /// Source location.
    pub span: SourceSpan,
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
    pub fn single(item_type: ItemTypeNode, occurrence: OccurrenceIndicator, span: SourceSpan) -> Self {
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_axis_direction() {
        assert!(Axis::Child.is_forward());
        assert!(Axis::Parent.is_reverse());
        assert!(Axis::Ancestor.is_reverse());
        assert!(Axis::Descendant.is_forward());
    }

    #[test]
    fn test_name_test() {
        let any = NameTest::any();
        assert!(any.prefix.is_none());
        assert!(any.local_name.is_none());

        let qname = NameTest::qname("xs".to_string(), "integer".to_string());
        assert_eq!(qname.prefix, Some("xs".to_string()));
        assert_eq!(qname.local_name, Some("integer".to_string()));
    }

    #[test]
    fn test_value_node() {
        let s = ValueNode::String("hello".to_string());
        match s {
            ValueNode::String(v) => assert_eq!(v, "hello"),
            _ => panic!("Expected string"),
        }

        let i = ValueNode::Integer("42".to_string());
        match i {
            ValueNode::Integer(v) => assert_eq!(v, "42"),
            _ => panic!("Expected integer"),
        }
    }
}
