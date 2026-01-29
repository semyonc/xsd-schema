//! XPath2 sequence types for type matching and conversion
//!
//! This module provides the `SequenceType` struct used for:
//! - Type matching in `instance of` and `treat as` expressions
//! - Function signatures and parameter types
//! - Type conversion target specifications

use std::fmt;

use crate::ids::SimpleTypeKey;
use crate::namespace::qname::QualifiedName;
use super::XmlTypeCode;

/// XPath2 sequence type cardinality
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum XmlTypeCardinality {
    /// Exactly one (T)
    #[default]
    One,
    /// Zero or one (T?)
    ZeroOrOne,
    /// One or more (T+)
    OneOrMore,
    /// Zero or more (T*)
    ZeroOrMore,
}

impl XmlTypeCardinality {
    /// Check if this cardinality allows empty sequences
    pub fn allows_empty(&self) -> bool {
        matches!(self, Self::ZeroOrOne | Self::ZeroOrMore)
    }

    /// Check if this cardinality allows multiple items
    pub fn allows_many(&self) -> bool {
        matches!(self, Self::OneOrMore | Self::ZeroOrMore)
    }

    /// Check if an actual count matches this cardinality
    pub fn matches_count(&self, count: usize) -> bool {
        match self {
            Self::One => count == 1,
            Self::ZeroOrOne => count <= 1,
            Self::OneOrMore => count >= 1,
            Self::ZeroOrMore => true,
        }
    }

    /// Get the symbol for this cardinality
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::One => "",
            Self::ZeroOrOne => "?",
            Self::OneOrMore => "+",
            Self::ZeroOrMore => "*",
        }
    }
}

impl fmt::Display for XmlTypeCardinality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.symbol())
    }
}

/// Item type in a sequence type
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemType {
    /// item() - any item
    AnyItem,
    /// node() - any node
    AnyNode,
    /// document-node() with optional element type
    Document(Option<Box<ItemType>>),
    /// element() with optional name and type
    Element(Option<NameTest>, Option<SimpleTypeKey>),
    /// attribute() with optional name and type
    Attribute(Option<NameTest>, Option<SimpleTypeKey>),
    /// schema-element()
    SchemaElement(QualifiedName),
    /// schema-attribute()
    SchemaAttribute(QualifiedName),
    /// text()
    Text,
    /// comment()
    Comment,
    /// processing-instruction()
    ProcessingInstruction(Option<String>),
    /// namespace-node()
    NamespaceNode,
    /// Atomic type (xs:string, xs:integer, etc.)
    AtomicType(XmlTypeCode),
    /// Schema-defined atomic type
    SchemaAtomicType(SimpleTypeKey),
}

impl ItemType {
    /// Get the type code for this item type
    pub fn type_code(&self) -> XmlTypeCode {
        match self {
            Self::AnyItem => XmlTypeCode::Item,
            Self::AnyNode => XmlTypeCode::Node,
            Self::Document(_) => XmlTypeCode::Document,
            Self::Element(_, _) | Self::SchemaElement(_) => XmlTypeCode::Element,
            Self::Attribute(_, _) | Self::SchemaAttribute(_) => XmlTypeCode::Attribute,
            Self::Text => XmlTypeCode::Text,
            Self::Comment => XmlTypeCode::Comment,
            Self::ProcessingInstruction(_) => XmlTypeCode::ProcessingInstruction,
            Self::NamespaceNode => XmlTypeCode::Namespace,
            Self::AtomicType(code) => *code,
            Self::SchemaAtomicType(_) => XmlTypeCode::AnyAtomicType,
        }
    }

    /// Check if this item type matches nodes
    pub fn is_node(&self) -> bool {
        matches!(
            self,
            Self::AnyNode
                | Self::Document(_)
                | Self::Element(_, _)
                | Self::Attribute(_, _)
                | Self::SchemaElement(_)
                | Self::SchemaAttribute(_)
                | Self::Text
                | Self::Comment
                | Self::ProcessingInstruction(_)
                | Self::NamespaceNode
        )
    }

    /// Check if this item type matches atomic values
    pub fn is_atomic(&self) -> bool {
        matches!(self, Self::AtomicType(_) | Self::SchemaAtomicType(_))
    }
}

/// Name test for element/attribute tests (resolved form with interned names)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameTest {
    /// Wildcard (*) - matches any name
    Wildcard,
    /// Namespace wildcard (*:local) - matches any namespace with specific local name
    /// The NameId is the interned local name
    NamespaceWildcard(crate::ids::NameId),
    /// Local name wildcard (prefix:*) - matches any local name in namespace
    /// The NameId is the resolved namespace URI
    LocalWildcard(crate::ids::NameId),
    /// Specific QName (fully resolved)
    QName(QualifiedName),
}

impl NameTest {
    /// Check if this is a wildcard (matches any name)
    pub fn is_wildcard(&self) -> bool {
        matches!(self, Self::Wildcard)
    }
}

/// XPath2 sequence type
///
/// Describes the expected type of a sequence of items.
/// Used for type checking, type matching, and conversions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceType {
    /// The item type
    pub item_type: ItemType,
    /// The occurrence indicator
    pub cardinality: XmlTypeCardinality,
}

impl SequenceType {
    /// Create a new sequence type
    pub fn new(item_type: ItemType, cardinality: XmlTypeCardinality) -> Self {
        Self {
            item_type,
            cardinality,
        }
    }

    /// Create a sequence type for exactly one item
    pub fn one(item_type: ItemType) -> Self {
        Self::new(item_type, XmlTypeCardinality::One)
    }

    /// Create a sequence type for zero or one items
    pub fn optional(item_type: ItemType) -> Self {
        Self::new(item_type, XmlTypeCardinality::ZeroOrOne)
    }

    /// Create a sequence type for one or more items
    pub fn plus(item_type: ItemType) -> Self {
        Self::new(item_type, XmlTypeCardinality::OneOrMore)
    }

    /// Create a sequence type for zero or more items
    pub fn star(item_type: ItemType) -> Self {
        Self::new(item_type, XmlTypeCardinality::ZeroOrMore)
    }

    /// Create empty-sequence() type
    pub fn empty() -> Self {
        Self::new(ItemType::AnyItem, XmlTypeCardinality::ZeroOrMore)
    }

    /// Create item()* - any sequence
    pub fn any() -> Self {
        Self::star(ItemType::AnyItem)
    }

    /// Create item() - exactly one item
    pub fn item() -> Self {
        Self::one(ItemType::AnyItem)
    }

    /// Create node() - exactly one node
    pub fn node() -> Self {
        Self::one(ItemType::AnyNode)
    }

    /// Create node()* - sequence of nodes
    pub fn nodes() -> Self {
        Self::star(ItemType::AnyNode)
    }

    // Convenience constructors for common atomic types

    /// xs:string
    pub fn string() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::String))
    }

    /// xs:string?
    pub fn string_optional() -> Self {
        Self::optional(ItemType::AtomicType(XmlTypeCode::String))
    }

    /// xs:boolean
    pub fn boolean() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::Boolean))
    }

    /// xs:integer
    pub fn integer() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::Integer))
    }

    /// xs:integer?
    pub fn integer_optional() -> Self {
        Self::optional(ItemType::AtomicType(XmlTypeCode::Integer))
    }

    /// xs:decimal
    pub fn decimal() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::Decimal))
    }

    /// xs:double
    pub fn double() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::Double))
    }

    /// xs:double?
    pub fn double_optional() -> Self {
        Self::optional(ItemType::AtomicType(XmlTypeCode::Double))
    }

    /// xs:anyAtomicType
    pub fn any_atomic() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::AnyAtomicType))
    }

    /// xs:anyAtomicType?
    pub fn any_atomic_optional() -> Self {
        Self::optional(ItemType::AtomicType(XmlTypeCode::AnyAtomicType))
    }

    /// xs:anyAtomicType*
    pub fn any_atomic_star() -> Self {
        Self::star(ItemType::AtomicType(XmlTypeCode::AnyAtomicType))
    }

    /// xs:dateTime
    pub fn datetime() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::DateTime))
    }

    /// xs:date
    pub fn date() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::Date))
    }

    /// xs:time
    pub fn time() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::Time))
    }

    /// xs:duration
    pub fn duration() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::Duration))
    }

    /// xs:QName
    pub fn qname() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::QName))
    }

    /// xs:anyURI
    pub fn any_uri() -> Self {
        Self::one(ItemType::AtomicType(XmlTypeCode::AnyUri))
    }

    // Query methods

    /// Get the type code for the item type
    pub fn type_code(&self) -> XmlTypeCode {
        self.item_type.type_code()
    }

    /// Check if this sequence type matches nodes
    pub fn is_node(&self) -> bool {
        self.item_type.is_node()
    }

    /// Check if this sequence type matches atomic values
    pub fn is_atomic(&self) -> bool {
        self.item_type.is_atomic()
    }

    /// Check if this sequence type is numeric (decimal, float, double, integer types)
    pub fn is_numeric(&self) -> bool {
        self.type_code().is_numeric()
    }

    /// Check if this sequence type allows empty sequences
    pub fn allows_empty(&self) -> bool {
        self.cardinality.allows_empty()
    }
}

impl Default for SequenceType {
    fn default() -> Self {
        Self::any()
    }
}

impl fmt::Display for SequenceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Format item type
        match &self.item_type {
            ItemType::AnyItem => write!(f, "item()")?,
            ItemType::AnyNode => write!(f, "node()")?,
            ItemType::Document(None) => write!(f, "document-node()")?,
            ItemType::Document(Some(elem)) => write!(f, "document-node({:?})", elem)?,
            ItemType::Element(None, None) => write!(f, "element()")?,
            ItemType::Element(Some(name), None) => write!(f, "element({:?})", name)?,
            ItemType::Element(name, Some(_type_key)) => {
                write!(f, "element({:?}, ...)", name)?;
            }
            ItemType::Attribute(None, None) => write!(f, "attribute()")?,
            ItemType::Attribute(Some(name), None) => write!(f, "attribute({:?})", name)?,
            ItemType::Attribute(name, Some(_type_key)) => {
                write!(f, "attribute({:?}, ...)", name)?;
            }
            ItemType::SchemaElement(qn) => write!(f, "schema-element({:?})", qn)?,
            ItemType::SchemaAttribute(qn) => write!(f, "schema-attribute({:?})", qn)?,
            ItemType::Text => write!(f, "text()")?,
            ItemType::Comment => write!(f, "comment()")?,
            ItemType::ProcessingInstruction(None) => write!(f, "processing-instruction()")?,
            ItemType::ProcessingInstruction(Some(name)) => {
                write!(f, "processing-instruction({})", name)?;
            }
            ItemType::NamespaceNode => write!(f, "namespace-node()")?,
            ItemType::AtomicType(code) => {
                if let Some(name) = code.local_name() {
                    write!(f, "xs:{}", name)?;
                } else {
                    write!(f, "{:?}", code)?;
                }
            }
            ItemType::SchemaAtomicType(_key) => write!(f, "schema-type(...)")?,
        }

        // Format cardinality
        write!(f, "{}", self.cardinality)?;

        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cardinality_matches_count() {
        assert!(XmlTypeCardinality::One.matches_count(1));
        assert!(!XmlTypeCardinality::One.matches_count(0));
        assert!(!XmlTypeCardinality::One.matches_count(2));

        assert!(XmlTypeCardinality::ZeroOrOne.matches_count(0));
        assert!(XmlTypeCardinality::ZeroOrOne.matches_count(1));
        assert!(!XmlTypeCardinality::ZeroOrOne.matches_count(2));

        assert!(!XmlTypeCardinality::OneOrMore.matches_count(0));
        assert!(XmlTypeCardinality::OneOrMore.matches_count(1));
        assert!(XmlTypeCardinality::OneOrMore.matches_count(100));

        assert!(XmlTypeCardinality::ZeroOrMore.matches_count(0));
        assert!(XmlTypeCardinality::ZeroOrMore.matches_count(1));
        assert!(XmlTypeCardinality::ZeroOrMore.matches_count(100));
    }

    #[test]
    fn test_cardinality_symbols() {
        assert_eq!(XmlTypeCardinality::One.symbol(), "");
        assert_eq!(XmlTypeCardinality::ZeroOrOne.symbol(), "?");
        assert_eq!(XmlTypeCardinality::OneOrMore.symbol(), "+");
        assert_eq!(XmlTypeCardinality::ZeroOrMore.symbol(), "*");
    }

    #[test]
    fn test_sequence_type_display() {
        assert_eq!(SequenceType::item().to_string(), "item()");
        assert_eq!(SequenceType::string().to_string(), "xs:string");
        assert_eq!(SequenceType::string_optional().to_string(), "xs:string?");
        assert_eq!(SequenceType::nodes().to_string(), "node()*");
        assert_eq!(SequenceType::integer().to_string(), "xs:integer");
    }

    #[test]
    fn test_sequence_type_is_node() {
        assert!(SequenceType::node().is_node());
        assert!(SequenceType::nodes().is_node());
        assert!(!SequenceType::string().is_node());
        assert!(!SequenceType::any_atomic().is_node());
    }

    #[test]
    fn test_sequence_type_is_atomic() {
        assert!(SequenceType::string().is_atomic());
        assert!(SequenceType::integer().is_atomic());
        assert!(SequenceType::any_atomic().is_atomic());
        assert!(!SequenceType::node().is_atomic());
        assert!(!SequenceType::item().is_atomic());
    }

    #[test]
    fn test_sequence_type_is_numeric() {
        assert!(SequenceType::decimal().is_numeric());
        assert!(SequenceType::integer().is_numeric());
        assert!(SequenceType::double().is_numeric());
        assert!(!SequenceType::string().is_numeric());
        assert!(!SequenceType::boolean().is_numeric());
    }

    #[test]
    fn test_item_type_type_code() {
        assert_eq!(ItemType::AnyItem.type_code(), XmlTypeCode::Item);
        assert_eq!(ItemType::AnyNode.type_code(), XmlTypeCode::Node);
        assert_eq!(ItemType::Text.type_code(), XmlTypeCode::Text);
        assert_eq!(
            ItemType::AtomicType(XmlTypeCode::String).type_code(),
            XmlTypeCode::String
        );
    }
}
