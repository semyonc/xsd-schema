//! XSD 1.1 XPath tests compiled once per schema set.
//!
//! An `xs:assert` (Structures §3.13.4.1), a simple type's `assertion` facet
//! (Datatypes §4.3.13) and an `xs:alternative` test (Structures §3.12.4) are
//! evaluated per element or per value, but their expression text and the
//! static context it is compiled in depend only on the schema: the namespace
//! snapshot of the schema element, the resolved `xpathDefaultNamespace`, the
//! schema document's base URI (alternatives) and the declared `$value`
//! (assertions). [`CompiledXPathCache`] compiles each test on first use and
//! keeps it, together with those context inputs, so an evaluation only
//! rebuilds the context from them, creates the evaluator and runs it.
//!
//! The schema-time check (`schema::validate_assertion_xpath`) compiles the
//! same tests with only the built-in components in scope; evaluation needs the
//! schema set in its context, so that compiled result is not the one kept.
//!
//! Two things feed a test's static context. The components in the arenas
//! (the test text, its namespace snapshot, the `xpathDefaultNamespace` of the
//! test and of its type) change only through `ArenasGuard::entries_mut`, which
//! wipes this cache. The schema documents (`SchemaSet::documents`, public: a
//! document's `base_uri`, `xpath_default_namespace` and `target_namespace`)
//! can change without it, so every entry remembers what it read from them
//! ([`DocumentStamp`]) and a lookup rebuilds the entry when a document no
//! longer matches. The result is what compiling per evaluation gave: the
//! context always reflects the documents as they are now.

use std::sync::{Arc, RwLock};

use slotmap::{Key, SecondaryMap};

use crate::ids::{ComplexTypeKey, DocumentId, ElementKey, NameId, SimpleTypeKey, TypeKey};
use crate::namespace::context::NamespaceContextSnapshot;
use crate::parser::location::{SourceLocation, SourceRef};
use crate::schema::{SchemaDocument, SchemaSet};
use crate::types::facets::FacetSet;
use crate::xpath::api::XPathExpr;
use crate::xpath::XPathContext;

use super::alternatives::resolve_alternative_default_ns;
use super::assertions::{collect_inherited_assertions, resolve_ct_assertion_default_ns};
use super::simple::resolve_assertion_default_ns;

/// The fields of one schema document that a test's static context is built
/// from: `base_uri` is the static base URI of an alternative test (Structures
/// §3.12.4), `xpath_default_namespace` and `target_namespace` feed the
/// `xpathDefaultNamespace` cascade of every test.
#[derive(Debug, PartialEq, Eq)]
struct DocumentFields {
    base_uri: String,
    xpath_default_namespace: Option<NameId>,
    target_namespace: Option<NameId>,
}

impl DocumentFields {
    fn of(doc: &SchemaDocument) -> Self {
        Self {
            base_uri: doc.base_uri.clone(),
            xpath_default_namespace: doc.xpath_default_namespace,
            target_namespace: doc.target_namespace,
        }
    }

    fn matches(&self, doc: &SchemaDocument) -> bool {
        self.base_uri == doc.base_uri
            && self.xpath_default_namespace == doc.xpath_default_namespace
            && self.target_namespace == doc.target_namespace
    }
}

/// What an entry read from one schema document when it was built: the
/// document's [`DocumentFields`], or `None` when `SchemaSet::documents` had no
/// document under that id (the cascade then reads nothing). `is_current` is
/// the test a lookup applies before reusing the entry.
#[derive(Debug)]
struct DocumentStamp {
    doc_id: DocumentId,
    seen: Option<DocumentFields>,
}

impl DocumentStamp {
    fn take(doc_id: DocumentId, schema_set: &SchemaSet) -> Self {
        Self {
            doc_id,
            seen: schema_set
                .documents
                .get(doc_id as usize)
                .map(DocumentFields::of),
        }
    }

    fn is_current(&self, schema_set: &SchemaSet) -> bool {
        match (schema_set.documents.get(self.doc_id as usize), &self.seen) {
            (Some(doc), Some(seen)) => seen.matches(doc),
            (None, None) => true,
            _ => false,
        }
    }
}

/// The stamps of the documents `sources` name, one per distinct document.
fn document_stamps(sources: &[Option<&SourceRef>], schema_set: &SchemaSet) -> Vec<DocumentStamp> {
    let mut stamps: Vec<DocumentStamp> = Vec::new();
    for source in sources.iter().flatten() {
        if stamps.iter().all(|s| s.doc_id != source.doc_id) {
            stamps.push(DocumentStamp::take(source.doc_id, schema_set));
        }
    }
    stamps
}

/// One XPath test compiled in its static context, with the inputs that context
/// is built from, so the evaluation context is the compile-time one.
///
/// A test that does not compile keeps its error text: an assertion then fails
/// with `cvc-assertion` at every evaluation and an alternative never matches,
/// as when the test was compiled per evaluation.
#[derive(Debug)]
pub(crate) struct CompiledXPathTest {
    /// The expression text, as written in the schema.
    pub(crate) test: String,
    /// The compiled expression, or the compile error's message.
    pub(crate) expr: Result<XPathExpr, String>,
    namespaces: NamespaceContextSnapshot,
    default_element_ns: Option<NameId>,
    base_uri: Option<String>,
    schema_casts: bool,
    /// Where an `assertion` facet is defined, for its error reports; `None`
    /// for the other tests, whose errors carry no schema location. Read from
    /// `SchemaSet::source_maps` once; it only names the facet in a message.
    pub(crate) location: Option<SourceLocation>,
    /// The schema documents `default_element_ns` and `base_uri` were resolved
    /// from, as they were then; see [`DocumentStamp`].
    documents: Vec<DocumentStamp>,
}

impl CompiledXPathTest {
    /// An assertion test (`xs:assert` or the `assertion` facet): `$value` is
    /// declared (Structures §3.13.4.1 clause 2.2) and the schema-defined
    /// constructor functions stay out of scope (`without_schema_casts`, a
    /// second guard behind the schema-time check).
    fn assertion(
        test: &str,
        namespaces: &NamespaceContextSnapshot,
        default_element_ns: Option<NameId>,
        location: Option<SourceLocation>,
        documents: Vec<DocumentStamp>,
        schema_set: &SchemaSet,
    ) -> Self {
        let mut compiled = Self {
            test: test.to_string(),
            expr: Err(String::new()),
            namespaces: namespaces.clone(),
            default_element_ns,
            base_uri: None,
            schema_casts: false,
            location,
            documents,
        };
        compiled.expr =
            XPathExpr::compile_with_vars(test, &compiled.context(schema_set), &["value"])
                .map_err(|e| e.to_string());
        compiled
    }

    /// A type alternative test (Structures §3.12.4): no variables, and the
    /// static base URI is the schema document's.
    fn alternative(
        test: &str,
        namespaces: &NamespaceContextSnapshot,
        default_element_ns: Option<NameId>,
        base_uri: Option<String>,
        documents: Vec<DocumentStamp>,
        schema_set: &SchemaSet,
    ) -> Self {
        let mut compiled = Self {
            test: test.to_string(),
            expr: Err(String::new()),
            namespaces: namespaces.clone(),
            default_element_ns,
            base_uri,
            schema_casts: true,
            location: None,
            documents,
        };
        compiled.expr =
            XPathExpr::compile(test, &compiled.context(schema_set)).map_err(|e| e.to_string());
        compiled
    }

    /// Whether the schema documents this test's context was resolved from are
    /// unchanged, so the entry still describes the test as it would be
    /// compiled now.
    fn is_current(&self, schema_set: &SchemaSet) -> bool {
        self.documents.iter().all(|stamp| stamp.is_current(schema_set))
    }

    /// The static context the test was compiled in, rebuilt for an evaluation.
    pub(crate) fn context<'a>(&self, schema_set: &'a SchemaSet) -> XPathContext<'a> {
        let ctx = XPathContext::new(&schema_set.name_table)
            .with_namespaces(self.namespaces.clone())
            .with_schema_set(schema_set);
        let ctx = if self.schema_casts {
            ctx
        } else {
            ctx.without_schema_casts()
        };
        let ctx = match &self.base_uri {
            Some(uri) => ctx.with_base_uri(uri.clone()),
            None => ctx,
        };
        match self.default_element_ns {
            Some(ns) => ctx.with_default_element_ns(ns),
            None => ctx,
        }
    }
}

/// One `xs:alternative` of an element declaration, in declaration order.
#[derive(Debug)]
pub(crate) struct CompiledAlternative {
    /// The compiled test; `None` for the test-less default alternative.
    pub(crate) test: Option<CompiledXPathTest>,
    /// The type the alternative selects.
    pub(crate) resolved_type: Option<TypeKey>,
}

/// Where a set of `assertion` facets comes from: a simple type's effective
/// facets, or the local facets of a complex type's simple-content restriction
/// (Structures §3.4.2.2 clause 1.2).
#[derive(Debug, Clone, Copy)]
pub(crate) enum AssertionFacetsOwner {
    Simple(SimpleTypeKey),
    SimpleContent(ComplexTypeKey),
}

/// The compiled XPath tests of a schema set, built per component on first use.
///
/// Lives on [`ArenasGuard`](crate::arenas::ArenasGuard) next to the
/// effective-facet cache and, like it, is wiped by `entries_mut`, so it is
/// always consistent with the arenas; a component added later is a miss that
/// is compiled then. Dense `SecondaryMap`s keyed by the owning component.
#[derive(Debug, Default)]
pub(crate) struct CompiledXPathCache {
    /// Per complex type: its own and inherited `xs:assert` tests, base first.
    complex_type_assertions: RwLock<SecondaryMap<ComplexTypeKey, Arc<[CompiledXPathTest]>>>,
    /// Per simple type: the `assertion` facets of its effective facets.
    simple_type_assertions: RwLock<SecondaryMap<SimpleTypeKey, Arc<[CompiledXPathTest]>>>,
    /// Per complex type: the `assertion` facets of its simple-content restriction.
    simple_content_assertions: RwLock<SecondaryMap<ComplexTypeKey, Arc<[CompiledXPathTest]>>>,
    /// Per element declaration: its type alternatives.
    alternatives: RwLock<SecondaryMap<ElementKey, Arc<[CompiledAlternative]>>>,
}

/// Memo lookup: a hit that `current` accepts is returned; a miss, or a hit a
/// changed schema document has outdated, is rebuilt by `build` and replaces
/// the entry. No lock is held while building.
fn all_current(tests: &[CompiledXPathTest], schema_set: &SchemaSet) -> bool {
    tests.iter().all(|test| test.is_current(schema_set))
}

fn memo<K: Key, V: ?Sized>(
    map: &RwLock<SecondaryMap<K, Arc<V>>>,
    key: K,
    current: impl Fn(&V) -> bool,
    build: impl FnOnce() -> Arc<V>,
) -> Arc<V> {
    if let Some(value) = map.read().unwrap().get(key) {
        if current(value) {
            return Arc::clone(value);
        }
    }
    let built = build();
    map.write().unwrap().insert(key, Arc::clone(&built));
    built
}

impl CompiledXPathCache {
    /// Drop every entry (called by `ArenasGuard::entries_mut`).
    pub(crate) fn clear(&mut self) {
        self.complex_type_assertions.get_mut().unwrap().clear();
        self.simple_type_assertions.get_mut().unwrap().clear();
        self.simple_content_assertions.get_mut().unwrap().clear();
        self.alternatives.get_mut().unwrap().clear();
    }

    /// The `xs:assert` tests a complex type's instances are checked against
    /// (Structures §3.13.4.1): its own and its base types', base first, each
    /// with the `xpathDefaultNamespace` of the type that declared it. Tests
    /// with empty text are left out.
    pub(crate) fn complex_type_assertions(
        &self,
        ct_key: ComplexTypeKey,
        schema_set: &SchemaSet,
    ) -> Arc<[CompiledXPathTest]> {
        let current = |tests: &[CompiledXPathTest]| all_current(tests, schema_set);
        memo(&self.complex_type_assertions, ct_key, current, || {
            collect_inherited_assertions(ct_key, &schema_set.arenas)
                .into_iter()
                .filter(|(assertion, _)| !assertion.test.is_empty())
                .map(|(assertion, owner)| {
                    // The cascade reads the document of the type that owns
                    // the assertion (`resolve_ct_assertion_default_ns`).
                    let owner_source = schema_set.arenas.complex_types[owner].source.as_ref();
                    CompiledXPathTest::assertion(
                        &assertion.test,
                        &assertion.ns_snapshot,
                        resolve_ct_assertion_default_ns(assertion, owner, schema_set),
                        None,
                        document_stamps(&[owner_source], schema_set),
                        schema_set,
                    )
                })
                .collect()
        })
    }

    /// The `assertion` facets of `facets`, which are the facets `owner`
    /// stands for. Tests with empty text are left out.
    pub(crate) fn assertion_facets(
        &self,
        owner: AssertionFacetsOwner,
        facets: &FacetSet,
        schema_set: &SchemaSet,
    ) -> Arc<[CompiledXPathTest]> {
        let build = || {
            facets
                .assertions
                .iter()
                .filter(|assertion| !assertion.test.is_empty())
                .map(|assertion| {
                    CompiledXPathTest::assertion(
                        &assertion.test,
                        &assertion.ns_snapshot,
                        resolve_assertion_default_ns(
                            assertion.xpath_default_namespace.as_deref(),
                            assertion.source.as_ref(),
                            &assertion.ns_snapshot,
                            schema_set,
                        ),
                        assertion
                            .source
                            .as_ref()
                            .and_then(|s| schema_set.source_maps.locate(s)),
                        // The cascade reads the facet's own document
                        // (`resolve_assertion_default_ns`).
                        document_stamps(&[assertion.source.as_ref()], schema_set),
                        schema_set,
                    )
                })
                .collect()
        };
        let current = |tests: &[CompiledXPathTest]| all_current(tests, schema_set);
        match owner {
            AssertionFacetsOwner::Simple(sk) => {
                memo(&self.simple_type_assertions, sk, current, build)
            }
            AssertionFacetsOwner::SimpleContent(ck) => {
                memo(&self.simple_content_assertions, ck, current, build)
            }
        }
    }

    /// The type alternatives of an element declaration (Structures §3.12.4),
    /// in declaration order. The static base URI of each test is the URI of
    /// the schema document containing the `xs:alternative`, so
    /// `fn:static-base-uri()` names the schema document, not the instance.
    pub(crate) fn alternatives(
        &self,
        elem_key: ElementKey,
        schema_set: &SchemaSet,
    ) -> Arc<[CompiledAlternative]> {
        let current = |alts: &[CompiledAlternative]| {
            alts.iter()
                .all(|alt| alt.test.as_ref().is_none_or(|t| t.is_current(schema_set)))
        };
        memo(&self.alternatives, elem_key, current, || {
            let elem = &schema_set.arenas.elements[elem_key];
            elem.alternatives
                .iter()
                .map(|alt| CompiledAlternative {
                    test: alt.test.as_ref().map(|test| {
                        let base_uri = alt
                            .source
                            .as_ref()
                            .and_then(|s| schema_set.documents.get(s.doc_id as usize))
                            .map(|d| d.base_uri.as_str())
                            .filter(|u| !u.is_empty())
                            .map(str::to_string);
                        // The base URI comes from the alternative's document,
                        // the cascade from the element declaration's
                        // (`resolve_alternative_default_ns`).
                        let documents = document_stamps(
                            &[alt.source.as_ref(), elem.source.as_ref()],
                            schema_set,
                        );
                        CompiledXPathTest::alternative(
                            test,
                            &alt.ns_snapshot,
                            resolve_alternative_default_ns(alt, elem_key, schema_set),
                            base_uri,
                            documents,
                            schema_set,
                        )
                    }),
                    resolved_type: alt.resolved_type,
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::load_and_process_schema;
    use crate::validation::simple::validate_simple_type;

    const XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
                           xmlns:my="urn:my">
        <xs:complexType name="range">
            <xs:attribute name="min" type="xs:int"/>
            <xs:attribute name="max" type="xs:int"/>
            <xs:assert test="@min le @max"/>
        </xs:complexType>
        <xs:simpleType name="pct">
            <xs:restriction base="xs:integer">
                <xs:assertion test="$value le 100"/>
            </xs:restriction>
        </xs:simpleType>
        <xs:simpleType name="unknownFn">
            <xs:restriction base="xs:integer">
                <xs:assertion test="my:nope($value)"/>
            </xs:restriction>
        </xs:simpleType>
        <xs:element name="item" type="xs:anyType">
            <xs:alternative test="@kind = 'a'" type="xs:string"/>
            <xs:alternative/>
        </xs:element>
        <xs:element name="doc" type="xs:anyType">
            <xs:alternative test="ends-with(static-base-uri(), 'test.xsd')" type="xs:string"/>
            <xs:alternative type="xs:int"/>
        </xs:element>
    </xs:schema>"#;

    fn load() -> SchemaSet {
        let mut schema_set = SchemaSet::xsd11();
        load_and_process_schema(XSD.as_bytes(), "test.xsd", &mut schema_set, None)
            .expect("failed to load schema");
        schema_set
    }

    fn type_key(schema_set: &SchemaSet, name: &str) -> TypeKey {
        schema_set
            .lookup_type(None, schema_set.name_table.add(name))
            .expect("type not found")
    }

    fn expr_ptr(test: &CompiledXPathTest) -> *const XPathExpr {
        test.expr.as_ref().expect("compiles")
    }

    #[test]
    fn schema_set_stays_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SchemaSet>();
    }

    #[test]
    fn second_lookup_returns_the_same_compiled_expressions() {
        let schema_set = load();
        let cache = schema_set.arenas.compiled_xpath();

        let TypeKey::Complex(ct) = type_key(&schema_set, "range") else {
            panic!("complex type expected");
        };
        let first = cache.complex_type_assertions(ct, &schema_set);
        let second = cache.complex_type_assertions(ct, &schema_set);
        assert_eq!(first.len(), 1);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(expr_ptr(&first[0]), expr_ptr(&second[0]));

        let TypeKey::Simple(st) = type_key(&schema_set, "pct") else {
            panic!("simple type expected");
        };
        let facets = schema_set.effective_facets(st);
        let owner = AssertionFacetsOwner::Simple(st);
        let first = cache.assertion_facets(owner, &facets, &schema_set);
        let second = cache.assertion_facets(owner, &facets, &schema_set);
        assert!(Arc::ptr_eq(&first, &second));

        let elem = schema_set
            .lookup_element(None, schema_set.name_table.add("item"))
            .expect("element");
        let first = cache.alternatives(elem, &schema_set);
        let second = cache.alternatives(elem, &schema_set);
        assert_eq!(first.len(), 2);
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first[1].test.is_none());

        // Values are validated through the cache and still checked.
        let pct = type_key(&schema_set, "pct");
        assert!(validate_simple_type("50", pct, &schema_set).is_ok());
        let err = validate_simple_type("150", pct, &schema_set).unwrap_err();
        assert_eq!(err.constraint, "cvc-assertion");
    }

    #[test]
    fn a_test_that_fails_to_compile_fails_every_evaluation() {
        let schema_set = load();
        let key = type_key(&schema_set, "unknownFn");
        for _ in 0..2 {
            let err = validate_simple_type("1", key, &schema_set).unwrap_err();
            assert_eq!(err.constraint, "cvc-assertion");
            assert!(
                err.message
                    .starts_with("Failed to compile assertion test 'my:nope($value)': "),
                "{}",
                err.message
            );
        }
    }

    /// `SchemaSet::documents` is public: a document's base URI can change
    /// after the alternative was compiled, and `static-base-uri()` must see
    /// the change as it did when the test was compiled per evaluation.
    #[test]
    fn a_changed_document_base_uri_is_seen() {
        use crate::validation::alternatives::evaluate_type_alternatives;
        let mut schema_set = load();
        let elem = schema_set
            .lookup_element(None, schema_set.name_table.add("doc"))
            .expect("element");
        let local = schema_set.name_table.add("doc");
        // The two alternatives' types (`xs:string`, then `xs:int`).
        let (string, int) = {
            let alts = schema_set.arenas.compiled_xpath().alternatives(elem, &schema_set);
            (alts[0].resolved_type, alts[1].resolved_type)
        };
        assert!(string.is_some() && int.is_some() && string != int);
        let select = |schema_set: &SchemaSet| {
            evaluate_type_alternatives(elem, local, None, &[], None, None, schema_set)
        };
        assert_eq!(select(&schema_set), string);
        assert_eq!(select(&schema_set), string);

        schema_set.documents[0].base_uri = "elsewhere/other.xsd".to_string();
        assert_eq!(select(&schema_set), int);
    }

    /// A changed document-level `xpathDefaultNamespace` rebuilds the entry;
    /// an unchanged document keeps it.
    #[test]
    fn a_changed_document_default_namespace_rebuilds_the_entry() {
        let mut schema_set = load();
        let TypeKey::Complex(ct) = type_key(&schema_set, "range") else {
            panic!("complex type expected");
        };
        let first = schema_set
            .arenas
            .compiled_xpath()
            .complex_type_assertions(ct, &schema_set);
        let again = schema_set
            .arenas
            .compiled_xpath()
            .complex_type_assertions(ct, &schema_set);
        assert!(Arc::ptr_eq(&first, &again));

        let urn = schema_set.name_table.add("urn:elsewhere");
        schema_set.documents[0].xpath_default_namespace = Some(urn);
        let rebuilt = schema_set
            .arenas
            .compiled_xpath()
            .complex_type_assertions(ct, &schema_set);
        assert!(!Arc::ptr_eq(&first, &rebuilt));
        assert_eq!(rebuilt.len(), 1);
        assert!(rebuilt[0].expr.is_ok());
    }

    #[test]
    fn entries_mut_drops_the_compiled_tests() {
        let mut schema_set = load();
        let pct = type_key(&schema_set, "pct");
        let TypeKey::Simple(st) = pct else {
            panic!("simple type expected");
        };
        assert!(validate_simple_type("150", pct, &schema_set).is_err());

        schema_set.arenas.entries_mut().simple_types[st]
            .facets
            .assertions[0]
            .test = "$value le 200".to_string();
        assert!(validate_simple_type("150", pct, &schema_set).is_ok());
    }
}
