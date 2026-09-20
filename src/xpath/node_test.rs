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
        ItemType::SchemaElement(name) => matches_schema_element(nav, name, ctx),
        ItemType::SchemaAttribute(name) => matches_schema_attribute(nav, name, ctx),
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
// schema-element(N) / schema-attribute(N)
// ============================================================================

/// `schema-element(N)` matching — XPath 2.0 §2.5.4.4.
///
/// This is the **single** implementation of the rule. Every spelling of the
/// test routes here: `ItemType::SchemaElement` (a step node test, and the
/// `SequenceType` form after resolution), `KindTest::SchemaElement` (the AST
/// form used by `instance of` / `treat as`) and `ItemType::matches_node`.
///
/// > A `SchemaElementTest` matches a candidate element node if all three of the
/// > following conditions are satisfied:
/// >
/// > 1. The name of the candidate node matches the specified `ElementName` or
/// >    matches the name of an element in a substitution group headed by an
/// >    element named `ElementName`.
/// > 2. `derives-from(AT, ET)` is true, where `AT` is the type annotation of the
/// >    candidate node and `ET` is the schema type declared for element
/// >    `ElementName` in the in-scope element declarations.
/// > 3. If the element declaration for `ElementName` in the in-scope element
/// >    declarations is not nillable, then the nilled property of the candidate
/// >    node is false.
///
/// Without a schema set in the static context there are no in-scope element
/// declarations to consult, so the test degrades to a name-only match, which is
/// what it did before clauses 1 and 3 existed.
///
/// §2.5.4.4 also makes an `ElementName` that is *not* in the in-scope element
/// declarations a static error (`XPST0008`). This crate does not raise it —
/// neither here nor from the binder — and such a test simply matches nothing.
pub(crate) fn matches_schema_element<N: DomNavigator>(
    nav: &N,
    name: &QualifiedName,
    ctx: &XPathContext<'_>,
) -> bool {
    if nav.node_type() != DomNodeType::Element {
        return false;
    }
    let Some(schema_set) = ctx.schema_set else {
        // No in-scope element declarations: name-only match.
        return qname_matches(name, nav, ctx);
    };
    let Some(head_key) = schema_set.lookup_element(name.namespace_uri, name.local_name) else {
        // `ElementName` is not declared; nothing matches it.
        return false;
    };
    let Some(head) = schema_set.arenas.elements.get(head_key) else {
        return false;
    };

    // Clause 1: the candidate's own name, or a member of the group `name` heads.
    if !qname_matches(name, nav, ctx) {
        let Some(candidate_key) = lookup_node_element_decl(nav, schema_set) else {
            return false;
        };
        if !crate::compiler::substitution::is_substitution_group_member(
            schema_set,
            head_key,
            candidate_key,
        ) {
            return false;
        }
    }

    // Clause 3: only a nillable declaration accepts a nilled candidate.
    if !head.nillable && matches!(nav.typed_value(), crate::xpath::TypedValue::Nilled) {
        return false;
    }

    // Clause 2: derives-from(AT, ET).
    derives_from_declared_type(nav, head.resolved_type, schema_set)
}

/// `schema-attribute(N)` matching — XPath 2.0 §2.5.4.6.
///
/// > A `SchemaAttributeTest` matches a candidate attribute node if both of the
/// > following conditions are satisfied:
/// >
/// > 1. The name of the candidate node matches the specified `AttributeName`.
/// > 2. `derives-from(AT, ET)` is true, where `AT` is the type annotation of the
/// >    candidate node and `ET` is the schema type declared for attribute
/// >    `AttributeName` in the in-scope attribute declarations.
///
/// There is no substitution group for attributes, and no nilled property, so
/// the rule is the element one without clauses 1's second half and 3. The
/// `XPST0008` and no-schema-set remarks of [`matches_schema_element`] apply
/// unchanged.
pub(crate) fn matches_schema_attribute<N: DomNavigator>(
    nav: &N,
    name: &QualifiedName,
    ctx: &XPathContext<'_>,
) -> bool {
    if nav.node_type() != DomNodeType::Attribute {
        return false;
    }
    if !qname_matches(name, nav, ctx) {
        return false;
    }
    let Some(schema_set) = ctx.schema_set else {
        return true;
    };
    let Some(attr_key) = schema_set.lookup_attribute(name.namespace_uri, name.local_name) else {
        return false;
    };
    let Some(attr) = schema_set.arenas.attributes.get(attr_key) else {
        return false;
    };
    derives_from_declared_type(nav, attr.resolved_type, schema_set)
}

/// Expand the lexical `ElementName` / `AttributeName` of a
/// `schema-element(N)` / `schema-attribute(N)` test into the interned
/// [`QualifiedName`] the matcher takes.
///
/// The namespace rule is the one §3.2.1.2 gives a name test on an axis of that
/// principal node kind: an unprefixed `ElementName` picks up the default
/// element namespace, while an unprefixed `AttributeName` is in no namespace.
/// A prefix that the static context cannot expand cannot occur — the binder
/// rejects it with `XPST0081` — but the name is re-resolved here rather than
/// carried, so an unexpandable one yields `None` and matches nothing.
///
/// Returns `None` for a name that is not a lexical QName at all.
pub(crate) fn resolve_schema_test_name(
    name: &str,
    ctx: &XPathContext<'_>,
    principal: DomNodeType,
) -> Option<QualifiedName> {
    let (prefix, local) = crate::xpath::functions::qname::parse_lexical_qname(name).ok()?;
    let ns_id = match &prefix {
        Some(prefix) => Some(ctx.names.add(&ctx.resolve_prefix(prefix)?)),
        // An unprefixed attribute name is always in no namespace.
        None if principal == DomNodeType::Attribute => None,
        None => ctx.default_element_ns,
    };
    Some(QualifiedName::new(ns_id, ctx.names.add(&local), None))
}

/// The global element declaration whose expanded name is the one `nav` carries,
/// if the schema set has one.
fn lookup_node_element_decl<N: DomNavigator>(
    nav: &N,
    schema_set: &crate::schema::SchemaSet,
) -> Option<crate::ids::ElementKey> {
    let local_id = schema_set.name_table.get(nav.local_name())?;
    let ns = nav.namespace_uri();
    let ns_id = if ns.is_empty() {
        None
    } else {
        Some(schema_set.name_table.get(ns)?)
    };
    schema_set.lookup_element(ns_id, local_id)
}

/// `derives-from(AT, ET)` for a candidate node and the type a declaration
/// declares, where `None` for `ET` means the declaration constrains no type.
///
/// `AT` is read from [`DomNavigator::type_annotation`], not from
/// [`DomNavigator::schema_type`], so a node annotated with a **complex** type
/// participates: the declared type lives in the schema set here, not in a
/// `SimpleTypeKey` slot of the [`ItemType`], so nothing restricts it to the
/// simple types. A node with no annotation at all cannot derive from a declared
/// type and never matches.
fn derives_from_declared_type<N: DomNavigator>(
    nav: &N,
    declared: Option<TypeKey>,
    schema_set: &crate::schema::SchemaSet,
) -> bool {
    let Some(expected) = declared else {
        // The declaration constrains no type: the name match is the whole test.
        return true;
    };
    let Some(actual) = nav.type_annotation() else {
        return false;
    };
    schema_set.is_type_derived_from(actual, expected, DerivationSet::empty())
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
        // The two schema tests keep their name in lexical form until here;
        // resolving it produces the very same `QualifiedName` the
        // `ItemType::SchemaElement` / `ItemType::SchemaAttribute` spelling
        // carries, so both spellings then run the one matcher.
        KindTest::SchemaElement(name) => {
            match resolve_schema_test_name(name, ctx, DomNodeType::Element) {
                Some(qname) => matches_schema_element(nav, &qname, ctx),
                None => false,
            }
        }
        KindTest::SchemaAttribute(name) => {
            match resolve_schema_test_name(name, ctx, DomNodeType::Attribute) {
                Some(qname) => matches_schema_attribute(nav, &qname, ctx),
                None => false,
            }
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
