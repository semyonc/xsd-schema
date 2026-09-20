//! XPath node test matching helpers.
//!
//! Provides a unified node test type that can be used by axis iterators
//! and type-based filters, aligning with `XPATH_ITERATOR_PORT_PLAN.md`.

use crate::ids::TypeKey;
use crate::namespace::qname::QualifiedName;
use crate::schema::model::DerivationSet;
use crate::types::value::XmlValue;
use crate::types::{ItemType, NameTest, SequenceType};
use crate::xpath::ast::{Axis, ItemTypeNode, KindTest};
use crate::xpath::cast::type_matches;
use crate::xpath::iterator::XmlItem;

use super::context::XPathContext;
use super::{DomNavigator, DomNodeType};

/// The principal node kind of `axis` (XPath 2.0 §3.2.1.1).
///
/// "If an axis can contain elements, then the principal node kind is element;
/// otherwise, it is the kind of nodes that the axis can contain." Thus it is
/// attribute for the `attribute::` axis, namespace for `namespace::`, and
/// element for every other axis.
pub fn principal_node_kind(axis: Axis) -> DomNodeType {
    match axis {
        Axis::Attribute => DomNodeType::Attribute,
        Axis::Namespace => DomNodeType::Namespace,
        _ => DomNodeType::Element,
    }
}

/// Build the runtime node test for a **name test** used on an axis whose
/// principal node kind is `principal`.
///
/// XPath 2.0 §3.2.1.2: "A name test is true if and only if the kind of the
/// node is the principal node kind for the step axis and the expanded QName
/// of the node is equal (as defined by the `eq` operator) to the expanded
/// QName specified by the name test." Without this restriction `self::*` and
/// `ancestor-or-self::*` would select attribute nodes, which are never of the
/// principal node kind of those axes.
pub fn name_test_for_principal_kind(test: NameTest, principal: DomNodeType) -> NodeTest {
    match principal {
        DomNodeType::Attribute => {
            NodeTest::Type(SequenceType::one(ItemType::Attribute(Some(test), None)))
        }
        // A namespace node's name is its prefix, and it is the only kind the
        // `namespace::` axis yields; `matches_name_test` applies that rule.
        DomNodeType::Namespace => NodeTest::Name(test),
        _ => NodeTest::Type(SequenceType::one(ItemType::Element(Some(test), None))),
    }
}

/// Unified node test for axis iterators.
#[derive(Debug, Clone)]
pub enum NodeTest {
    /// Name test (`*`, `*:local`, `prefix:*`, or QName).
    Name(NameTest),
    /// Sequence type test (`node()`, `element(...)`, etc.).
    Type(SequenceType),
}

impl NodeTest {
    pub fn matches<N: DomNavigator>(&self, nav: &N, ctx: &XPathContext<'_>) -> bool {
        match self {
            NodeTest::Name(test) => matches_name_test(test, nav, ctx),
            NodeTest::Type(seq) => matches_sequence_type(seq, nav, ctx),
        }
    }
}

pub fn matches_name_test<N: DomNavigator>(
    test: &NameTest,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    // A namespace node has a name too: its prefix, in no namespace (XDM §6.4).
    // A name test on the `namespace::` axis therefore selects by prefix, and
    // `*` selects every namespace node — the principal node kind of that axis
    // is namespace, not element.
    if nav.node_type() == DomNodeType::Namespace {
        return matches_namespace_name_test(test, nav, ctx);
    }
    if nav.node_type() != DomNodeType::Element && nav.node_type() != DomNodeType::Attribute {
        return false;
    }

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
        NameTest::QName(qname) => qname_matches(qname, nav, ctx),
    }
}

/// A name test applied to a namespace node, whose name is its prefix.
fn matches_namespace_name_test<N: DomNavigator>(
    test: &NameTest,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    match test {
        NameTest::Wildcard => true,
        // `*:local` — a namespace node is in no namespace, so this is just the
        // prefix test.
        NameTest::NamespaceWildcard(local_id) => match ctx.resolve_name(*local_id) {
            Some(local) => nav.local_name() == local,
            None => false,
        },
        // `prefix:*` — no namespace node is in a namespace.
        NameTest::LocalWildcard(_) => false,
        // A QName selects the namespace node whose prefix it is; a prefixed
        // name never matches, because the name is in no namespace.
        NameTest::QName(qname) => match ctx.resolve_name(qname.local_name) {
            Some(local) => qname.namespace_uri.is_none() && nav.local_name() == local,
            None => false,
        },
    }
}

pub fn matches_sequence_type<N: DomNavigator>(
    sequence: &SequenceType,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    matches_item_type(&sequence.item_type, nav, ctx)
}

fn matches_item_type<N: DomNavigator>(
    item_type: &ItemType,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    match item_type {
        ItemType::AnyItem | ItemType::AnyNode => true,
        ItemType::Document(None) => nav.node_type() == DomNodeType::Root,
        ItemType::Document(Some(inner)) => match_document_with_inner(inner, nav, ctx),
        ItemType::Element(name_test, schema_type) => {
            if nav.node_type() != DomNodeType::Element {
                return false;
            }
            if let Some(test) = name_test {
                if !matches_name_test(test, nav, ctx) {
                    return false;
                }
            }
            if let Some(expected) = schema_type {
                // Use derivation checking if schema_set is available
                if let Some(actual) = nav.schema_type() {
                    if let Some(schema_set) = ctx.schema_set {
                        // Check if actual type is derived from expected type
                        // Using empty DerivationSet means any derivation method is allowed
                        if !schema_set.is_type_derived_from(
                            TypeKey::Simple(actual),
                            TypeKey::Simple(*expected),
                            DerivationSet::empty(),
                        ) {
                            return false;
                        }
                    } else {
                        // Fallback to equality without schema set
                        if actual != *expected {
                            return false;
                        }
                    }
                } else {
                    // No schema type on node, fail the type match
                    return false;
                }
            }
            true
        }
        ItemType::Attribute(name_test, schema_type) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            if let Some(test) = name_test {
                if !matches_name_test(test, nav, ctx) {
                    return false;
                }
            }
            if let Some(expected) = schema_type {
                // Use derivation checking if schema_set is available
                if let Some(actual) = nav.schema_type() {
                    if let Some(schema_set) = ctx.schema_set {
                        // Check if actual type is derived from expected type
                        if !schema_set.is_type_derived_from(
                            TypeKey::Simple(actual),
                            TypeKey::Simple(*expected),
                            DerivationSet::empty(),
                        ) {
                            return false;
                        }
                    } else {
                        // Fallback to equality without schema set
                        if actual != *expected {
                            return false;
                        }
                    }
                } else {
                    // No schema type on node, fail the type match
                    return false;
                }
            }
            true
        }
        ItemType::SchemaElement(name) => {
            if nav.node_type() != DomNodeType::Element {
                return false;
            }
            // Check element name matches
            if !qname_matches(name, nav, ctx) {
                return false;
            }
            // If schema_set available, validate declaration exists and type derivation
            if let Some(schema_set) = ctx.schema_set {
                // Lookup element declaration - must exist for schema-element() to match
                let ns_id = name.namespace_uri;
                let Some(elem_key) = schema_set.lookup_element(ns_id, name.local_name) else {
                    // Declaration not found in schema - no match
                    return false;
                };
                let Some(elem_data) = schema_set.arenas.elements.get(elem_key) else {
                    return false;
                };
                // Check type derivation if declaration has resolved_type
                if let Some(expected_type) = elem_data.resolved_type {
                    let Some(actual_type) = nav.schema_type() else {
                        // Node has no type annotation but declaration expects one
                        return false;
                    };
                    // Node type must derive from declaration type
                    return schema_set.is_type_derived_from(
                        TypeKey::Simple(actual_type),
                        expected_type,
                        DerivationSet::empty(),
                    );
                }
                // Declaration found, no type constraint - match
                return true;
            }
            // No schema context - fall back to name-only match
            true
        }
        ItemType::SchemaAttribute(name) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            // Check attribute name matches
            if !qname_matches(name, nav, ctx) {
                return false;
            }
            // If schema_set available, validate declaration exists and type derivation
            if let Some(schema_set) = ctx.schema_set {
                // Lookup attribute declaration - must exist for schema-attribute() to match
                let ns_id = name.namespace_uri;
                let Some(attr_key) = schema_set.lookup_attribute(ns_id, name.local_name) else {
                    // Declaration not found in schema - no match
                    return false;
                };
                let Some(attr_data) = schema_set.arenas.attributes.get(attr_key) else {
                    return false;
                };
                // Check type derivation if declaration has resolved_type
                if let Some(expected_type) = attr_data.resolved_type {
                    let Some(actual_type) = nav.schema_type() else {
                        // Node has no type annotation but declaration expects one
                        return false;
                    };
                    // Node type must derive from declaration type
                    return schema_set.is_type_derived_from(
                        TypeKey::Simple(actual_type),
                        expected_type,
                        DerivationSet::empty(),
                    );
                }
                // Declaration found, no type constraint - match
                return true;
            }
            // No schema context - fall back to name-only match
            true
        }
        ItemType::Text => nav.node_type().is_text_like(),
        ItemType::Comment => nav.node_type() == DomNodeType::Comment,
        ItemType::ProcessingInstruction(target) => {
            nav.node_type() == DomNodeType::ProcessingInstruction
                && target.as_ref().is_none_or(|name| nav.local_name() == name)
        }
        ItemType::NamespaceNode => nav.node_type() == DomNodeType::Namespace,
        ItemType::AtomicType(_) | ItemType::SchemaAtomicType(_) => false,
    }
}

fn match_document_with_inner<N: DomNavigator>(
    inner: &ItemType,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    if nav.node_type() != DomNodeType::Root {
        return false;
    }

    let mut cursor = nav.clone();
    if !cursor.move_to_first_child() {
        return false;
    }

    loop {
        if matches_item_type(inner, &cursor, ctx) {
            return true;
        }
        if !cursor.move_to_next_sibling() {
            break;
        }
    }

    false
}

fn qname_matches<N: DomNavigator>(qname: &QualifiedName, nav: &N, ctx: &XPathContext<'_>) -> bool {
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

// ============================================================================
// AST KindTest and ItemTypeNode Matching
// ============================================================================

/// Check if an XmlItem matches an AST ItemTypeNode.
///
/// This is used for `instance of` and `treat as` expressions to check
/// if a value matches the target type specification.
///
/// # Arguments
///
/// * `item` - The item to check (node or atomic value)
/// * `item_type` - The AST item type node to match against
/// * `resolved_atomic_type` - The resolved QualifiedName for atomic types (from binding)
/// * `ctx` - The XPath context for name resolution
///
/// # Returns
///
/// `true` if the item matches the item type, `false` otherwise.
pub fn matches_item_type_node<N: DomNavigator>(
    item: &XmlItem<N>,
    item_type: &ItemTypeNode,
    resolved_atomic_type: Option<&QualifiedName>,
    ctx: &XPathContext<'_>,
) -> bool {
    match item_type {
        ItemTypeNode::Item => {
            // item() matches any item (node or atomic)
            true
        }
        ItemTypeNode::Atomic(_) => {
            // Atomic type - item must be an atomic value matching the type
            match item {
                XmlItem::Node(_) => false,
                XmlItem::Atomic(value) => {
                    // Use the resolved atomic type from binding
                    if let Some(qname) = resolved_atomic_type {
                        matches_atomic_type(value, qname, ctx)
                    } else {
                        // No resolved type - this shouldn't happen after binding
                        false
                    }
                }
            }
        }
        ItemTypeNode::Kind(kind_test) => {
            // Kind test - item must be a node matching the kind test
            match item {
                XmlItem::Node(nav) => matches_kind_test(nav, kind_test, ctx),
                XmlItem::Atomic(_) => false,
            }
        }
    }
}

/// Check if an atomic value matches a resolved atomic type QualifiedName.
fn matches_atomic_type(value: &XmlValue, qname: &QualifiedName, ctx: &XPathContext<'_>) -> bool {
    use crate::namespace::table::well_known;
    use crate::xpath::cast::resolved_type_to_type_code;

    // Verify it's in XS namespace
    match qname.namespace_uri {
        Some(ns_id) if ns_id == well_known::XS_NAMESPACE => {}
        _ => return false,
    }

    // Get the target type code
    let target_type = match resolved_type_to_type_code(qname, ctx.names) {
        Ok(tc) => tc,
        Err(_) => return false,
    };

    // Check if the value's type matches
    type_matches(value.type_code, target_type)
}

/// Check if a DOM node matches an AST KindTest.
///
/// This converts the AST KindTest to runtime type checks.
pub fn matches_kind_test<N: DomNavigator>(
    nav: &N,
    kind_test: &KindTest,
    ctx: &XPathContext<'_>,
) -> bool {
    match kind_test {
        KindTest::AnyKind => {
            // node() matches any node
            true
        }
        KindTest::Text => nav.node_type().is_text_like(),
        KindTest::Comment => nav.node_type() == DomNodeType::Comment,
        KindTest::ProcessingInstruction(target) => {
            if nav.node_type() != DomNodeType::ProcessingInstruction {
                return false;
            }
            match target {
                None => true,
                Some(name) => nav.local_name() == *name,
            }
        }
        KindTest::Document(inner) => {
            if nav.node_type() != DomNodeType::Root {
                return false;
            }
            match inner {
                None => true,
                Some(inner_kind) => {
                    // document-node(element(...)) - check if document has matching element
                    let mut cursor = nav.clone();
                    if !cursor.move_to_first_child() {
                        return false;
                    }
                    loop {
                        if matches_kind_test(&cursor, inner_kind, ctx) {
                            return true;
                        }
                        if !cursor.move_to_next_sibling() {
                            break;
                        }
                    }
                    false
                }
            }
        }
        KindTest::Element(elem_test) => {
            if nav.node_type() != DomNodeType::Element {
                return false;
            }
            // Check element name if specified
            if let Some(ref qname) = elem_test.name {
                if !ast_qname_matches(qname, nav, ctx) {
                    return false;
                }
            }
            if let Some(ref type_name) = elem_test.type_name {
                if !matches_type_annotation(nav, type_name, true, ctx) {
                    return false;
                }
                // §2.5.4.3: `element(N, T)` requires "the nilled property of
                // the node is false"; only `element(N, T?)` accepts a nilled
                // element.
                if !elem_test.nillable
                    && matches!(nav.typed_value(), crate::xpath::TypedValue::Nilled)
                {
                    return false;
                }
            }
            true
        }
        KindTest::Attribute(attr_test) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            // Check attribute name if specified
            if let Some(ref qname) = attr_test.name {
                if !ast_qname_matches(qname, nav, ctx) {
                    return false;
                }
            }
            if let Some(ref type_name) = attr_test.type_name {
                if !matches_type_annotation(nav, type_name, false, ctx) {
                    return false;
                }
            }
            true
        }
        KindTest::SchemaElement(name) => {
            if nav.node_type() != DomNodeType::Element {
                return false;
            }
            // Parse the QName string to extract prefix and local name
            use crate::xpath::functions::qname::parse_lexical_qname;
            let Ok((prefix_opt, local_name)) = parse_lexical_qname(name) else {
                return false; // Invalid QName syntax
            };
            // Check local name matches
            if nav.local_name() != local_name {
                return false;
            }
            // Resolve namespace: use prefix if provided, otherwise default element namespace
            let expected_ns = if let Some(prefix) = &prefix_opt {
                ctx.resolve_prefix(prefix).unwrap_or_default()
            } else {
                ctx.default_element_ns
                    .and_then(|id| ctx.names.try_resolve(id))
                    .unwrap_or_default()
            };
            // Verify node's namespace matches expected
            if nav.namespace_uri() != expected_ns {
                return false;
            }
            // If schema_set available, validate declaration exists and type
            if let Some(schema_set) = ctx.schema_set {
                // Get local name as NameId - if not found, declaration doesn't exist
                let Some(local_id) = ctx.names.get(&local_name) else {
                    return false;
                };
                // Get namespace as NameId
                let ns_id = if expected_ns.is_empty() {
                    None
                } else {
                    ctx.names.get(&expected_ns)
                };
                // Lookup element declaration - must exist for schema-element() to match
                let Some(elem_key) = schema_set.lookup_element(ns_id, local_id) else {
                    return false;
                };
                let Some(elem_data) = schema_set.arenas.elements.get(elem_key) else {
                    return false;
                };
                // Check type derivation if declaration has resolved_type
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
                // Declaration found, no type constraint - match
                return true;
            }
            // No schema context - name and namespace already verified
            true
        }
        KindTest::SchemaAttribute(name) => {
            if nav.node_type() != DomNodeType::Attribute {
                return false;
            }
            // Parse the QName string to extract prefix and local name
            use crate::xpath::functions::qname::parse_lexical_qname;
            let Ok((prefix_opt, local_name)) = parse_lexical_qname(name) else {
                return false; // Invalid QName syntax
            };
            // Check local name matches
            if nav.local_name() != local_name {
                return false;
            }
            // Resolve namespace: use prefix if provided, otherwise empty (attributes default to no namespace)
            let expected_ns = if let Some(prefix) = &prefix_opt {
                ctx.resolve_prefix(prefix).unwrap_or_default()
            } else {
                String::new() // Unprefixed attributes have no namespace
            };
            // Verify node's namespace matches expected
            if nav.namespace_uri() != expected_ns {
                return false;
            }
            // If schema_set available, validate declaration exists and type
            if let Some(schema_set) = ctx.schema_set {
                // Get local name as NameId - if not found, declaration doesn't exist
                let Some(local_id) = ctx.names.get(&local_name) else {
                    return false;
                };
                // Get namespace as NameId
                let ns_id = if expected_ns.is_empty() {
                    None
                } else {
                    ctx.names.get(&expected_ns)
                };
                // Lookup attribute declaration - must exist for schema-attribute() to match
                let Some(attr_key) = schema_set.lookup_attribute(ns_id, local_id) else {
                    return false;
                };
                let Some(attr_data) = schema_set.arenas.attributes.get(attr_key) else {
                    return false;
                };
                // Check type derivation if declaration has resolved_type
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
                // Declaration found, no type constraint - match
                return true;
            }
            // No schema context - name and namespace already verified
            true
        }
    }
}

/// Does the node's type annotation derive from the `TypeName` of an
/// `element(N, T)` or `attribute(N, T)` test?
///
/// XPath 2.0 §2.5.4.3: "`element(ElementName, TypeName)` matches an element node
/// whose name is `ElementName` if `derives-from(AT, TypeName)` is `true`, where
/// `AT` is the type annotation of the element node". §2.5.4.5 states the same
/// rule for `attribute(AttributeName, TypeName)`.
///
/// A node that carries no type annotation — any node of a document that was
/// never validated, or one a lax assessment skipped — is annotated `xs:untyped`
/// if it is an element and `xs:untypedAtomic` if it is an attribute. Those two
/// types derive from none of the ordinary schema types, which is why
/// `element(*, xs:integer)` must not match an untyped element; they do derive
/// from `xs:anyType`, and `xs:untypedAtomic` additionally from
/// `xs:anySimpleType` and `xs:anyAtomicType`. (That hierarchy is XDM, which is
/// not available locally, so it is stated **from memory**; it is corroborated by
/// this crate's own `types::builtin` base-type table, which gives
/// `xs:untypedAtomic` the base `xs:anyAtomicType`, and by the W3C tests
/// `saxonData/CTA/cta0018` and `cta0019`, which use `element(*, xs:untyped)` and
/// `attribute(*, xs:untypedAtomic)` precisely to detect the untyped case.)
fn matches_type_annotation<N: DomNavigator>(
    nav: &N,
    type_name: &crate::xpath::ast::QName,
    is_element: bool,
    ctx: &XPathContext<'_>,
) -> bool {
    use crate::namespace::table::well_known;

    // An unprefixed TypeName is in the default element/type namespace.
    let ns_id = if type_name.prefix.is_empty() {
        ctx.default_element_ns
    } else {
        match ctx.resolve_prefix(&type_name.prefix) {
            Some(uri) => Some(ctx.names.add(&uri)),
            None => return false,
        }
    };
    let is_xs = ns_id == Some(well_known::XS_NAMESPACE);

    match nav.type_annotation() {
        None => {
            // The node is untyped.
            if !is_xs {
                return false;
            }
            if is_element {
                matches!(type_name.local.as_str(), "untyped" | "anyType")
            } else {
                matches!(
                    type_name.local.as_str(),
                    "untypedAtomic" | "anyAtomicType" | "anySimpleType" | "anyType"
                )
            }
        }
        Some(actual) => {
            // The node is annotated, so the TypeName has to name a type of the
            // schema the annotation came from.
            let Some(schema_set) = ctx.schema_set else {
                return false;
            };
            let local_id = ctx.names.add(&type_name.local);
            let target = if is_xs {
                schema_set.get_built_in_type_by_qname(ns_id, local_id)
            } else {
                schema_set.lookup_type(ns_id, local_id)
            };
            match target {
                Some(target) => {
                    schema_set.is_type_derived_from(actual, target, DerivationSet::empty())
                }
                // `xs:untyped` has no schema type of its own, and an annotated
                // node is not untyped in any case.
                None => false,
            }
        }
    }
}

/// Check if a node matches an AST QName (from paths.rs).
fn ast_qname_matches<N: DomNavigator>(
    qname: &crate::xpath::ast::QName,
    nav: &N,
    ctx: &XPathContext<'_>,
) -> bool {
    // For AST QName, prefix is stored directly as a string
    // Local name must match
    if nav.local_name() != qname.local {
        return false;
    }

    // Resolve prefix to namespace URI
    if qname.prefix.is_empty() {
        // No prefix - match empty namespace
        nav.namespace_uri().is_empty()
    } else {
        // Resolve the prefix to namespace URI
        match ctx.resolve_prefix(&qname.prefix) {
            Some(ns_uri) => nav.namespace_uri() == ns_uri,
            None => false,
        }
    }
}

#[cfg(test)]
#[path = "node_test_tests.rs"]
mod tests;
