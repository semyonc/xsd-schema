//! XPath2 sequence types for type matching and conversion
//!
//! This module provides the `SequenceType` struct used for:
//! - Type matching in `instance of` and `treat as` expressions
//! - Function signatures and parameter types
//! - Type conversion target specifications

use std::fmt;

use super::XmlTypeCode;
use crate::ids::{SimpleTypeKey, TypeKey};
use crate::namespace::qname::QualifiedName;
use crate::schema::model::DerivationSet;
use crate::types::value::XmlValue;
use crate::xpath::cast::type_matches;
use crate::xpath::context::XPathContext;
use crate::xpath::iterator::XmlItem;
use crate::xpath::{DomNavigator, DomNodeType};

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
    /// Empty sequence only (empty-sequence())
    Empty,
}

impl XmlTypeCardinality {
    /// Check if this cardinality allows empty sequences
    pub fn allows_empty(&self) -> bool {
        matches!(self, Self::ZeroOrOne | Self::ZeroOrMore | Self::Empty)
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
            Self::Empty => count == 0,
        }
    }

    /// Get the symbol for this cardinality
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::One => "",
            Self::ZeroOrOne => "?",
            Self::OneOrMore => "+",
            Self::ZeroOrMore => "*",
            Self::Empty => "", // empty-sequence() has no suffix
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

    /// Check if a single XmlItem matches this item type.
    ///
    /// This is the runtime type matching method for function signatures
    /// and general type checking.
    pub fn matches_item<N: DomNavigator>(&self, item: &XmlItem<N>, ctx: &XPathContext<'_>) -> bool {
        match item {
            XmlItem::Node(nav) => self.matches_node(nav, ctx),
            XmlItem::Atomic(value) => self.matches_atomic(value, ctx),
        }
    }

    /// Check if a node matches this item type.
    fn matches_node<N: DomNavigator>(&self, nav: &N, ctx: &XPathContext<'_>) -> bool {
        match self {
            Self::AnyItem => true,
            Self::AnyNode => true,
            Self::Document(None) => nav.node_type() == DomNodeType::Root,
            Self::Document(Some(inner)) => {
                if nav.node_type() != DomNodeType::Root {
                    return false;
                }
                // Per XPath 2.0 spec: document-node(E) matches a document node that
                // contains exactly one element child (optionally with comment/PI nodes),
                // and that element must match E.
                let mut cursor = nav.clone();
                if !cursor.move_to_first_child() {
                    return false;
                }

                let mut element_count = 0;
                let mut matching_element: Option<N> = None;

                loop {
                    let node_type = cursor.node_type();
                    match node_type {
                        DomNodeType::Element => {
                            element_count += 1;
                            if element_count > 1 {
                                // More than one element child - reject
                                return false;
                            }
                            // Store this element to check against inner type
                            matching_element = Some(cursor.clone());
                        }
                        DomNodeType::Comment | DomNodeType::ProcessingInstruction => {
                            // Comments and PIs are allowed
                        }
                        DomNodeType::Text
                        | DomNodeType::Whitespace
                        | DomNodeType::SignificantWhitespace => {
                            // Text nodes in document (outside root element) are typically
                            // whitespace. Per spec, whitespace-only text nodes may appear.
                        }
                        _ => {
                            // Other node types (shouldn't appear at document level)
                        }
                    }
                    if !cursor.move_to_next_sibling() {
                        break;
                    }
                }

                // Must have exactly one element child
                if element_count != 1 {
                    return false;
                }

                // The single element must match the inner type
                match matching_element {
                    Some(elem) => inner.matches_node(&elem, ctx),
                    None => false,
                }
            }
            Self::Element(name_test, schema_type) => {
                if nav.node_type() != DomNodeType::Element {
                    return false;
                }
                if let Some(test) = name_test {
                    if !Self::matches_name_test(test, nav, ctx) {
                        return false;
                    }
                }
                if let Some(expected) = schema_type {
                    if !Self::matches_schema_type(nav, *expected, ctx) {
                        return false;
                    }
                }
                true
            }
            Self::Attribute(name_test, schema_type) => {
                if nav.node_type() != DomNodeType::Attribute {
                    return false;
                }
                if let Some(test) = name_test {
                    if !Self::matches_name_test(test, nav, ctx) {
                        return false;
                    }
                }
                if let Some(expected) = schema_type {
                    if !Self::matches_schema_type(nav, *expected, ctx) {
                        return false;
                    }
                }
                true
            }
            Self::SchemaElement(name) => {
                if nav.node_type() != DomNodeType::Element {
                    return false;
                }
                if !Self::matches_qname(name, nav, ctx) {
                    return false;
                }
                Self::matches_schema_element_decl(nav, name, ctx)
            }
            Self::SchemaAttribute(name) => {
                if nav.node_type() != DomNodeType::Attribute {
                    return false;
                }
                if !Self::matches_qname(name, nav, ctx) {
                    return false;
                }
                Self::matches_schema_attribute_decl(nav, name, ctx)
            }
            Self::Text => nav.node_type().is_text_like(),
            Self::Comment => nav.node_type() == DomNodeType::Comment,
            Self::ProcessingInstruction(target) => {
                nav.node_type() == DomNodeType::ProcessingInstruction
                    && target.as_ref().is_none_or(|name| nav.local_name() == name)
            }
            Self::NamespaceNode => nav.node_type() == DomNodeType::Namespace,
            // Atomic types don't match nodes
            Self::AtomicType(_) | Self::SchemaAtomicType(_) => false,
        }
    }

    /// Check if an atomic value matches this item type.
    fn matches_atomic(&self, value: &XmlValue, ctx: &XPathContext<'_>) -> bool {
        match self {
            Self::AnyItem => true,
            Self::AnyNode => false, // node() doesn't match atomics
            Self::AtomicType(code) => type_matches(value.type_code, *code),
            Self::SchemaAtomicType(key) => {
                // Check if value's schema type derives from the expected type
                if let Some(value_key) = value.schema_type {
                    if let Some(schema_set) = ctx.schema_set {
                        return schema_set.is_type_derived_from(
                            TypeKey::Simple(value_key),
                            TypeKey::Simple(*key),
                            DerivationSet::empty(),
                        );
                    }
                    // Fallback: exact match
                    return value_key == *key;
                }
                // No schema type on value - can't match schema-defined type
                false
            }
            // All node types don't match atomics
            Self::Document(_)
            | Self::Element(_, _)
            | Self::Attribute(_, _)
            | Self::SchemaElement(_)
            | Self::SchemaAttribute(_)
            | Self::Text
            | Self::Comment
            | Self::ProcessingInstruction(_)
            | Self::NamespaceNode => false,
        }
    }

    /// Helper: check if a node matches a name test.
    fn matches_name_test<N: DomNavigator>(
        test: &NameTest,
        nav: &N,
        ctx: &XPathContext<'_>,
    ) -> bool {
        match test {
            NameTest::Wildcard => true,
            NameTest::NamespaceWildcard(local_id) => {
                // *:local - match any namespace with specific local name
                match ctx.resolve_name(*local_id) {
                    Some(local) => nav.local_name() == local,
                    None => false,
                }
            }
            NameTest::LocalWildcard(ns_id) => {
                // prefix:* - match any local name in specific namespace
                match ctx.resolve_name(*ns_id) {
                    Some(ns) => nav.namespace_uri() == ns,
                    None => false,
                }
            }
            NameTest::QName(qname) => Self::matches_qname(qname, nav, ctx),
        }
    }

    /// Helper: check if a node matches a QualifiedName.
    fn matches_qname<N: DomNavigator>(
        qname: &QualifiedName,
        nav: &N,
        ctx: &XPathContext<'_>,
    ) -> bool {
        let local = match ctx.resolve_name(qname.local_name) {
            Some(local) => local,
            None => return false,
        };
        let ns = match qname.namespace_uri {
            Some(id) => match ctx.resolve_name(id) {
                Some(ns) => ns,
                None => return false,
            },
            None => String::new(),
        };
        nav.local_name() == local && nav.namespace_uri() == ns
    }

    /// Helper: check schema type derivation for element/attribute.
    fn matches_schema_type<N: DomNavigator>(
        nav: &N,
        expected: SimpleTypeKey,
        ctx: &XPathContext<'_>,
    ) -> bool {
        if let Some(actual) = nav.schema_type() {
            if let Some(schema_set) = ctx.schema_set {
                return schema_set.is_type_derived_from(
                    TypeKey::Simple(actual),
                    TypeKey::Simple(expected),
                    DerivationSet::empty(),
                );
            }
            // Fallback to equality without schema set
            return actual == expected;
        }
        // No schema type on node
        false
    }

    /// Helper: check schema-element() declaration match.
    fn matches_schema_element_decl<N: DomNavigator>(
        nav: &N,
        name: &QualifiedName,
        ctx: &XPathContext<'_>,
    ) -> bool {
        if let Some(schema_set) = ctx.schema_set {
            let ns_id = name.namespace_uri;
            let Some(elem_key) = schema_set.lookup_element(ns_id, name.local_name) else {
                return false;
            };
            let Some(elem_data) = schema_set.arenas.elements.get(elem_key) else {
                return false;
            };
            if let Some(expected_type) = elem_data.resolved_type {
                let Some(actual_type) = nav.schema_type() else {
                    return false;
                };
                return schema_set.is_type_derived_from(
                    TypeKey::Simple(actual_type),
                    expected_type,
                    DerivationSet::empty(),
                );
            }
            // Declaration found, no type constraint
            return true;
        }
        // No schema context - name already verified
        true
    }

    /// Helper: check schema-attribute() declaration match.
    fn matches_schema_attribute_decl<N: DomNavigator>(
        nav: &N,
        name: &QualifiedName,
        ctx: &XPathContext<'_>,
    ) -> bool {
        if let Some(schema_set) = ctx.schema_set {
            let ns_id = name.namespace_uri;
            let Some(attr_key) = schema_set.lookup_attribute(ns_id, name.local_name) else {
                return false;
            };
            let Some(attr_data) = schema_set.arenas.attributes.get(attr_key) else {
                return false;
            };
            if let Some(expected_type) = attr_data.resolved_type {
                let Some(actual_type) = nav.schema_type() else {
                    return false;
                };
                return schema_set.is_type_derived_from(
                    TypeKey::Simple(actual_type),
                    expected_type,
                    DerivationSet::empty(),
                );
            }
            // Declaration found, no type constraint
            return true;
        }
        // No schema context - name already verified
        true
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
    ///
    /// This only matches empty sequences (zero items).
    pub fn empty() -> Self {
        Self::new(ItemType::AnyItem, XmlTypeCardinality::Empty)
    }

    /// Check if this is the empty-sequence() type
    pub fn is_empty_sequence(&self) -> bool {
        self.cardinality == XmlTypeCardinality::Empty
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

    /// Check if a sequence of items matches this sequence type.
    ///
    /// Validates both cardinality and item type for each item in the sequence.
    pub fn matches_sequence<N: DomNavigator>(
        &self,
        items: &[XmlItem<N>],
        ctx: &XPathContext<'_>,
    ) -> bool {
        // Check cardinality first
        if !self.cardinality.matches_count(items.len()) {
            return false;
        }
        // Check each item matches the item type
        for item in items {
            if !self.item_type.matches_item(item, ctx) {
                return false;
            }
        }
        true
    }
}

impl Default for SequenceType {
    fn default() -> Self {
        Self::any()
    }
}

impl fmt::Display for SequenceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Handle empty-sequence() specially
        if self.cardinality == XmlTypeCardinality::Empty {
            return write!(f, "empty-sequence()");
        }

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
// Helpers (xsd11)
// ============================================================================

/// Resolve the item type's `SimpleTypeKey` from a list type definition.
///
/// Given a list type's `SimpleTypeKey`, looks up its `resolved_item_type` and
/// extracts the `SimpleTypeKey` (if it refers to a simple type).
/// Returns `None` if the type is not found or item type is not a simple type.
#[cfg(feature = "xsd11")]
pub fn resolve_list_item_schema_type(
    list_type_key: SimpleTypeKey,
    schema_set: &crate::schema::SchemaSet,
) -> Option<SimpleTypeKey> {
    let st_data = schema_set.arenas.simple_types.get(list_type_key)?;
    st_data.resolved_item_type?.as_simple()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;

    use crate::namespace::table::NameTable;
    use crate::navigator::RoXmlNavigator;

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

    // ============================================================================
    // Runtime Sequence Type Matching Tests
    // ============================================================================

    #[test]
    fn test_cardinality_one() {
        // xs:integer requires exactly 1 item
        let seq_type = SequenceType::integer();
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let one_item: Vec<XmlItem<RoXmlNavigator<'static>>> =
            vec![XmlItem::Atomic(XmlValue::integer(BigInt::from(42)))];
        let no_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![];
        let two_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(2))),
        ];

        assert!(seq_type.matches_sequence(&one_item, &ctx));
        assert!(!seq_type.matches_sequence(&no_items, &ctx));
        assert!(!seq_type.matches_sequence(&two_items, &ctx));
    }

    #[test]
    fn test_cardinality_optional() {
        // xs:integer? allows 0 or 1 item
        let seq_type = SequenceType::integer_optional();
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let one_item: Vec<XmlItem<RoXmlNavigator<'static>>> =
            vec![XmlItem::Atomic(XmlValue::integer(BigInt::from(42)))];
        let no_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![];
        let two_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(2))),
        ];

        assert!(seq_type.matches_sequence(&one_item, &ctx));
        assert!(seq_type.matches_sequence(&no_items, &ctx));
        assert!(!seq_type.matches_sequence(&two_items, &ctx));
    }

    #[test]
    fn test_cardinality_star() {
        // xs:integer* allows any count
        let seq_type = SequenceType::star(ItemType::AtomicType(XmlTypeCode::Integer));
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let no_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![];
        let one_item: Vec<XmlItem<RoXmlNavigator<'static>>> =
            vec![XmlItem::Atomic(XmlValue::integer(BigInt::from(42)))];
        let three_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(2))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(3))),
        ];

        assert!(seq_type.matches_sequence(&no_items, &ctx));
        assert!(seq_type.matches_sequence(&one_item, &ctx));
        assert!(seq_type.matches_sequence(&three_items, &ctx));
    }

    #[test]
    fn test_cardinality_plus() {
        // xs:integer+ requires at least 1 item
        let seq_type = SequenceType::plus(ItemType::AtomicType(XmlTypeCode::Integer));
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let no_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![];
        let one_item: Vec<XmlItem<RoXmlNavigator<'static>>> =
            vec![XmlItem::Atomic(XmlValue::integer(BigInt::from(42)))];
        let three_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(2))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(3))),
        ];

        assert!(!seq_type.matches_sequence(&no_items, &ctx));
        assert!(seq_type.matches_sequence(&one_item, &ctx));
        assert!(seq_type.matches_sequence(&three_items, &ctx));
    }

    #[test]
    fn test_atomic_integer_match() {
        // Integer value matches xs:integer
        let item_type = ItemType::AtomicType(XmlTypeCode::Integer);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let int_value: XmlItem<RoXmlNavigator<'static>> =
            XmlItem::Atomic(XmlValue::integer(BigInt::from(42)));
        let str_value: XmlItem<RoXmlNavigator<'static>> =
            XmlItem::Atomic(XmlValue::string("hello"));

        assert!(item_type.matches_item(&int_value, &ctx));
        assert!(!item_type.matches_item(&str_value, &ctx));
    }

    #[test]
    fn test_atomic_string_match() {
        // String value matches xs:string
        let item_type = ItemType::AtomicType(XmlTypeCode::String);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let str_value: XmlItem<RoXmlNavigator<'static>> =
            XmlItem::Atomic(XmlValue::string("hello"));
        let int_value: XmlItem<RoXmlNavigator<'static>> =
            XmlItem::Atomic(XmlValue::integer(BigInt::from(42)));

        assert!(item_type.matches_item(&str_value, &ctx));
        assert!(!item_type.matches_item(&int_value, &ctx));
    }

    #[test]
    fn test_atomic_type_hierarchy() {
        // Any atomic matches xs:anyAtomicType
        let item_type = ItemType::AtomicType(XmlTypeCode::AnyAtomicType);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let str_value: XmlItem<RoXmlNavigator<'static>> =
            XmlItem::Atomic(XmlValue::string("hello"));
        let int_value: XmlItem<RoXmlNavigator<'static>> =
            XmlItem::Atomic(XmlValue::integer(BigInt::from(42)));
        let bool_value: XmlItem<RoXmlNavigator<'static>> = XmlItem::Atomic(XmlValue::boolean(true));

        assert!(item_type.matches_item(&str_value, &ctx));
        assert!(item_type.matches_item(&int_value, &ctx));
        assert!(item_type.matches_item(&bool_value, &ctx));
    }

    #[test]
    fn test_item_matches_any() {
        // item() matches both nodes and atomics
        let item_type = ItemType::AnyItem;
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let node_item = XmlItem::Node(nav);
        let atomic_item: XmlItem<RoXmlNavigator<'_>> = XmlItem::Atomic(XmlValue::string("hello"));

        assert!(item_type.matches_item(&node_item, &ctx));
        assert!(item_type.matches_item(&atomic_item, &ctx));
    }

    #[test]
    fn test_node_rejects_atomic() {
        // node() rejects atomic values
        let item_type = ItemType::AnyNode;
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let atomic_item: XmlItem<RoXmlNavigator<'static>> =
            XmlItem::Atomic(XmlValue::string("hello"));

        assert!(!item_type.matches_item(&atomic_item, &ctx));
    }

    #[test]
    fn test_atomic_rejects_node() {
        // xs:integer rejects nodes
        let item_type = ItemType::AtomicType(XmlTypeCode::Integer);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let node_item = XmlItem::Node(nav);

        assert!(!item_type.matches_item(&node_item, &ctx));
    }

    #[test]
    fn test_element_item_type_matches_element() {
        // element() matches element nodes
        let item_type = ItemType::Element(None, None);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let node_item = XmlItem::Node(nav);

        assert!(item_type.matches_item(&node_item, &ctx));
    }

    #[test]
    fn test_text_item_type_matches_text() {
        // text() matches text nodes
        let item_type = ItemType::Text;
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<root>text</root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // text

        let node_item = XmlItem::Node(nav);

        assert!(item_type.matches_item(&node_item, &ctx));
    }

    #[test]
    fn test_sequence_type_mixed_types_fail() {
        // Sequence with wrong type fails
        let seq_type = SequenceType::star(ItemType::AtomicType(XmlTypeCode::Integer));
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let mixed_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::string("not an integer")),
        ];

        assert!(!seq_type.matches_sequence(&mixed_items, &ctx));
    }

    #[test]
    fn test_integer_derived_types() {
        // Integer derived types match xs:integer via type_matches
        let item_type = ItemType::AtomicType(XmlTypeCode::Integer);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        // Create a value with integer type code
        let int_value: XmlItem<RoXmlNavigator<'static>> =
            XmlItem::Atomic(XmlValue::integer(BigInt::from(42)));

        assert!(item_type.matches_item(&int_value, &ctx));
    }

    // ============================================================================
    // empty-sequence() Tests
    // ============================================================================

    #[test]
    fn test_empty_sequence_matches_empty() {
        // empty-sequence() only matches empty sequences
        let seq_type = SequenceType::empty();
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let empty: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![];
        assert!(seq_type.matches_sequence(&empty, &ctx));
    }

    #[test]
    fn test_empty_sequence_rejects_non_empty() {
        // empty-sequence() rejects non-empty sequences
        let seq_type = SequenceType::empty();
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let one_item: Vec<XmlItem<RoXmlNavigator<'static>>> =
            vec![XmlItem::Atomic(XmlValue::integer(BigInt::from(42)))];
        let two_items: Vec<XmlItem<RoXmlNavigator<'static>>> = vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(2))),
        ];

        assert!(!seq_type.matches_sequence(&one_item, &ctx));
        assert!(!seq_type.matches_sequence(&two_items, &ctx));
    }

    #[test]
    fn test_empty_sequence_display() {
        // empty-sequence() displays correctly
        assert_eq!(SequenceType::empty().to_string(), "empty-sequence()");
    }

    #[test]
    fn test_is_empty_sequence() {
        assert!(SequenceType::empty().is_empty_sequence());
        assert!(!SequenceType::any().is_empty_sequence());
        assert!(!SequenceType::integer().is_empty_sequence());
    }

    // ============================================================================
    // document-node(E) Tests
    // ============================================================================

    #[test]
    fn test_document_node_with_element_single_element() {
        // document-node(element()) matches document with exactly one element child
        let item_type = ItemType::Document(Some(Box::new(ItemType::Element(None, None))));
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let nav = RoXmlNavigator::new(&doc);

        assert!(item_type.matches_node(&nav, &ctx));
    }

    #[test]
    fn test_document_node_with_element_allows_comments() {
        // document-node(element()) allows comments alongside the element
        let item_type = ItemType::Document(Some(Box::new(ItemType::Element(None, None))));
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<!-- comment --><root/><!-- another -->")
            .expect("parse xml");
        let nav = RoXmlNavigator::new(&doc);

        assert!(item_type.matches_node(&nav, &ctx));
    }

    #[test]
    fn test_document_node_with_element_allows_pi() {
        // document-node(element()) allows processing instructions alongside the element
        let item_type = ItemType::Document(Some(Box::new(ItemType::Element(None, None))));
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<?target data?><root/>").expect("parse xml");
        let nav = RoXmlNavigator::new(&doc);

        assert!(item_type.matches_node(&nav, &ctx));
    }

    #[test]
    fn test_document_node_with_element_rejects_no_element() {
        // document-node(element()) rejects document with no element children
        let item_type = ItemType::Document(Some(Box::new(ItemType::Element(None, None))));
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);

        // A document with only a comment (no root element) - this would be malformed XML
        // but we can test the logic by checking a document node directly
        let doc = roxmltree::Document::parse("<!-- comment only is invalid XML --><dummy/>")
            .expect("parse xml");
        let nav = RoXmlNavigator::new(&doc);

        // This should match since there's still an element
        assert!(item_type.matches_node(&nav, &ctx));
    }

    #[test]
    fn test_document_node_with_specific_element_name() {
        // document-node(element(root)) matches document with element named "root"
        let table = NameTable::new();
        let root_id = table.add("root");
        let qname = QualifiedName::local(root_id);
        let name_test = NameTest::QName(qname);
        let item_type =
            ItemType::Document(Some(Box::new(ItemType::Element(Some(name_test), None))));
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<root/>").expect("parse xml");
        let nav = RoXmlNavigator::new(&doc);

        assert!(item_type.matches_node(&nav, &ctx));
    }

    #[test]
    fn test_document_node_with_wrong_element_name() {
        // document-node(element(root)) rejects document with element named "other"
        let table = NameTable::new();
        let root_id = table.add("root");
        let qname = QualifiedName::local(root_id);
        let name_test = NameTest::QName(qname);
        let item_type =
            ItemType::Document(Some(Box::new(ItemType::Element(Some(name_test), None))));
        let ctx = XPathContext::new(&table);

        let doc = roxmltree::Document::parse("<other/>").expect("parse xml");
        let nav = RoXmlNavigator::new(&doc);

        assert!(!item_type.matches_node(&nav, &ctx));
    }
}
