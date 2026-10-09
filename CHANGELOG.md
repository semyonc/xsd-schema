# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Under XSD 1.0, `derivation-ok-restriction` rejected an element restricting
  an optional choice through a branch that is itself optional — for example
  `XAdESTimeStampType` in the XAdES 1.3.2 schema used by UBL 2.1, which
  restricts `choice{0,1}(Include{0,unbounded}, ReferenceInfo{1,unbounded})`
  to `Include{0,unbounded}`. A base choice was checked by folding its
  occurrence range into each branch, never by the §3.9.6 rule, and a guard
  that keeps this shortcut from accepting an optional element against a
  *required* branch (W3C `particlesHa161`, invalid for 1.0) tested the derived
  element's `minOccurs` rather than the branch it maps to. Particle Derivation
  OK (Elt:All/Choice/Sequence -- RecurseAsIfGroup) is now applied first: the
  element, wrapped in an implicit `choice{1,1}`, is checked with RecurseLax,
  and the guard and the folding fallback apply only when that fails. XSD 1.1
  already accepted the schema. Both W3C suites are unchanged (identical
  failure sets), and so is the GAEB DA XML 3.3 corpus.

### Changed

- Under XSD 1.0, an element declared directly in a restriction no longer
  restricts a base choice whose `minOccurs` is 2 or more: `a{2,2}` against
  `choice{2,2}(a, b)` is now rejected. RecurseAsIfGroup fixes the implicit
  group's occurrence range at 1..1, which fails Occurrence Range OK against
  the base choice. That is the letter of the 1.0 Recommendation, unchanged by
  any erratum, and what Xerces-J and .NET implement; processors that accept it
  (XSV, Saxon) deliberately depart from 1.0 towards the 1.1 rule. A repeated
  group that particle normalization folds into the same range, such as
  `sequence{2,2}(a)`, is still checked as a group and accepted. Against a
  repeated choice with `minOccurs` 0 or 1 the previous, laxer result is kept,
  as W3C `particlesZ001`'s schema test expects. XSD 1.1 restriction is
  language subsumption (§3.4.6.4) and accepts all of these.

## [0.2.1] - 2026-10-03

### Behaviour changes

This release removes no public item and changes no signature, but it fixes a
large number of XPath 2.0 conformance defects, and some expressions now give a
different, correct answer or raise an error where they used to return one.
This list names them by theme; each point is detailed under *Fixed*, or under
*Added* where it says so.

- **Validation outcome.** An element's PSVI `[validity]`, and so
  `DriveOutcome::root_validity` and the validity an end-element event reports,
  now reflects its children, attributes and identity constraints and, for the
  validation root, the ID/IDREF table. An instance with an invalid descendant
  — a bad value, a failing assertion, a duplicate key, a dangling IDREF and so
  on — used to report the error but leave the root `Valid`; the root is now
  `Invalid`. Errors inside a laxly assessed subtree still leave it as it was.
  Which errors are reported is unchanged.
- **Paths and axes.** `/` evaluates its right-hand operand once per node, so
  results come in document order without duplicates and `//t[1]` selects the
  first `t` child of each parent rather than the first `t` in the document. An
  abbreviated `attribute(…)` step uses the attribute axis; `following::` no
  longer skips subtrees; `preceding::` is delivered in reverse document order,
  and the root has none; a name test selects only nodes of its axis's
  principal node kind, and works on the `namespace::` axis; in a step, the
  name of `element(N)` / `attribute(N)` and the type of `element(N, T)` /
  `attribute(N, T)` are honoured and `schema-element(N)` /
  `schema-attribute(N)` are declaration-aware; `xmlns=""` is no longer exposed
  as a namespace node. A leading `/` or `//` raises `XPDY0050` when the root of
  the context node's tree is not a document node.
- **New static errors** — the expression no longer compiles: a QName used as
  an `AtomicType` that is not an atomic type (`XPST0051`); `xs:NOTATION` or
  `xs:anyAtomicType` as the target of `cast as` or `castable as` (`XPST0080`);
  a `TypeName` in `element(N, T)` / `attribute(N, T)` that is neither a
  built-in type nor a type of the static context's schema set (`XPST0008`),
  which without a schema set is any name outside the XML Schema namespace; an
  unbound prefix inside a kind test (`XPST0081`); a numeric literal whose
  exponent has no digits, such as `1e` (`XPST0003`).
- **New dynamic and type errors.** An operator applied to operand types XPath
  2.0 does not define raises `XPTY0004`, and a general comparison no longer
  reports `false` for such a pair; unary `+` on a non-numeric operand, which
  returned it unchanged, raises `XPTY0004` too. An operand or built-in
  function argument of more than one item where at most one is allowed, and an
  operand of `is`, `<<` or `>>` holding more than one node, raise `XPTY0004`
  instead of `XPDY0050`; `fn:name`, `fn:root` and the other functions with a
  `node()?` parameter raise `XPTY0004` for several nodes or an atomic value
  instead of using the first node or answering as for the empty sequence.
  `treat as` raises `XPDY0050` on every failure. `inf`, `Infinity` and `nan`
  are no longer numbers (`FORG0001` in a cast, `NaN` from `fn:number`).
  `fn:sum` and `fn:avg` of a number and a duration, or of the two duration
  types, raise `FORG0006`; `idiv` raises `FOAR0002` for a NaN operand or an
  infinite dividend. A `$collation` the host does not supply raises `FOCH0002`
  instead of being ignored; `fn:tokenize` and `fn:replace` report an invalid
  pattern, flags or replacement whatever the input; `fn:matches`,
  `fn:replace` and `fn:tokenize` reject `(?` and the `q` flag;
  `fn:resolve-uri` raises `FORG0002` for more unusable base URIs. In the other
  direction, an `xs:untypedAtomic` operand of `to` is cast to `xs:integer`
  instead of raising `XPTY0004`, and `() treat as empty-sequence()` succeeds.
- **Atomization.** A list-typed node atomizes to one item per member, through
  `compose::Value` as well; a namespace node to `xs:string`; an element of
  mixed complex content (including `xs:anyType`) to `xs:untypedAtomic`, and
  one of empty content to the empty sequence; a nilled element makes a value
  comparison or arithmetic return the empty sequence; built-in functions
  atomize their arguments instead of reading the string value.
- **Functions.** `fn:index-of` and `fn:distinct-values` compare with `eq`
  (`index-of(xs:token('a'), 'a')` is now `1`); `fn:tokenize` keeps
  zero-length tokens; `fn:sum` keeps `xs:integer`, and `sum((), ())` is the
  empty sequence; `fn:min` and `fn:max` accept every ordered type and return
  NaN in the numbers' common type; `fn:round-half-to-even` decides a tie on
  the exact value and no longer returns NaN for a large precision;
  `fn:namespace-uri-for-prefix` returns the empty sequence for an unbound
  prefix; `fn:id` follows F&O §15.5.2 (normalized values, XML-whitespace
  tokens, `xs:ID`-typed attributes, `FODC0001` outside a document);
  `fn:deep-equal` follows F&O §15.3.1; `fn:nilled` reads the PSVI property;
  `fn:base-uri` of namespace and parentless attribute nodes, the error codes
  of `fn:resolve-QName` and `fn:subsequence` with infinite or NaN arguments
  are corrected.
- **SequenceType matching.** `xs:untypedAtomic` is no longer an `xs:string`,
  and `xs:dayTimeDuration` and `xs:yearMonthDuration` are `xs:duration`s; the
  `TypeName` of `element(N, T)` / `attribute(N, T)` is matched in
  `instance of` and `treat as`, so an untyped node no longer matches an
  ordinary schema type.
- **Lexer.** String literals are no longer entity-decoded a second time;
  `for`, `some` and `every` are ordinary names where the grammar allows one;
  `p:*` and `5.` are accepted; `NCName` follows the XML 1.0 name productions.
- **XSD 1.1 validation.** In an assertion the data model instance is cut at
  the asserted element: `parent::`, `ancestor::`, `following::` and the
  sibling axes no longer reach outside it, and `fn:id` finds the elements
  inside it, where it used to find nothing. In the test of a type alternative,
  whose data model instance is rooted at the element itself, a leading `/` or
  `//` and `fn:id` now raise an error, so the test is false. Each can change
  which type is selected or whether an instance is valid.
- **Schema loading.** A relative schema location that starts with `..` keeps
  it, so a different file may be read.
- **XPath 1.0 compatibility.** `XPathMode::XPath10` applies XPath 1.0's
  first-item rule to arithmetic: `1 + /doc/a` over two `a` elements adds the
  first instead of raising `XPDY0050`.
- **Documents and ids.** `xml:id` is indexed in a document built through the
  push API and in a `Fragment` buffer too; `BufferDocument::get_element_by_id`
  normalizes its argument, ignores values that are not NCNames and also finds
  `xs:ID`-typed attributes; text parsing reports `DuplicateId` for values that
  are equal after normalization (`"a"` and `" a"`) and no longer for repeated
  values that are not NCNames; `BufferDocumentBuilder::register_xml_id`
  accepts a repeat for the same element, works in a `Fragment` buffer and
  panics on a node reference outside the document; `copy_attribute` keeps the
  annotation, or lack of one, of the attribute that replaces another.
- **Public helpers.** `xpath::functions::atomize_to_single_opt` and the
  `atomize_to_*` helpers built on it raise `XPTY0004` instead of `XPDY0050`
  for more than one item; `xpath::atomize::to_number` no longer accepts `inf`,
  `Infinity` or `nan`; `BufferDocNavigator::typed_value` returns
  `TypedValue::Untyped` for mixed and an empty value for empty complex content
  instead of `TypedValue::Absent`; `RoXmlNavigator::name` returns the
  qualified name; `XPathValue::as_slice` returns a one-element slice for a
  single item.

### Added

- Collations. The module `xpath::collation` with the trait `Collation`, the
  trait `CollationResolver` and the constant `CODEPOINT_COLLATION_URI`, plus
  `XPathContext::with_collation_resolver`,
  `XPathContext::with_default_collation` and
  `XPathContext::default_collation`. The Unicode codepoint collation is
  implemented here and is still the default; every other collation is supplied
  by the host through the resolver callback, so the crate takes no dependency
  for collation data. A `Collation` need only implement `compare`; an optional
  `equals` (by default `compare(a, b).is_eq()`) decides equality where that is
  cheaper than ordering, an optional `sort_key` lets the engine keep answering
  a general comparison with a hash index instead of comparing every pair, and
  an optional `find` (with the provided `starts_with` and `ends_with`)
  supplies the collation units that `fn:contains`, `fn:starts-with`,
  `fn:ends-with`, `fn:substring-before` and `fn:substring-after` are defined
  over — without it they raise `FOCH0004`.
  The `$collation` argument of `fn:compare`, `fn:contains`, `fn:starts-with`,
  `fn:ends-with`, `fn:substring-before`, `fn:substring-after`, `fn:index-of`,
  `fn:distinct-values`, `fn:deep-equal`, `fn:min` and `fn:max`, and the static
  context's default collation for the value and general comparisons (`eq`,
  `lt`, `=`, `<`, …, including `=` and `!=` between strings in XPath 1.0
  compatibility mode, which XPath 2.0 §3.5.2 evaluates with `eq` and `ne`),
  now go through it; `fn:default-collation()` returns the
  property rather than a constant. A relative collation URI is resolved
  against the static base URI (F&O §7.3.1). Nothing changes under the
  codepoint collation: it is answered without consulting the resolver, on the
  same code path and at the same speed as before. `XPathError::error_code`
  reports `FOCH0004` for the new error, which travels as an error QName;
  `XPathError::collation_no_units` builds it. A comparison operator asks the
  resolver for the default collation only when it actually compares two
  strings — never for an evaluation that compares only numbers, dates or other
  non-string values — and a function that takes a `$collation` argument asks
  when it is called; within one evaluation run the answer is memoised. The
  public `operators::general_eq_iter` and its siblings, which see only the
  static context, ask at most once per call.
- `XPathContext::with_xpath10_compatibility(bool)` and
  `XPathContext::xpath10_compatibility()`. This is the static-context property
  XPath 2.0 calls *XPath 1.0 compatibility mode*, and it is distinct from
  `XPathMode::XPath10`, which is a *language* mode whose lexer and parser
  reject XPath 2.0 syntax. The flag keeps the full 2.0 syntax and switches
  only the semantics 2.0 itself defines differently when the property is
  true: the 1.0 conversions in arithmetic (§3.4); the 1.0 rules in general
  comparisons — an operand compared with a single boolean is converted, as a
  whole, to its effective boolean value (§3.5.2), `<`, `<=`, `>` and `>=`
  compare numbers, and `=` and `!=` compare numbers when either side is a
  number and strings otherwise; and the first-item, `fn:string` and
  `fn:number` conversions of a function argument whose declared type is a
  single item (§3.1.5). The effective boolean value itself is not affected: a
  sequence of two or more atomic values is `FORG0006` in `and`, `or`, a
  predicate, `fn:boolean` or `fn:not` with the flag as without it (§2.4.3).
  Hosts embedding the XPath engine need this when they run expressions
  written for a 1.0-era host language while still accepting 2.0 syntax in the
  same document. `XPathMode::XPath10` implies the property.
- `BufferDocNavigator::new_orphan(doc, node)` — a navigator under which `node`
  has **no parent**: `move_to_parent` returns `false` there, `move_to_root`
  and `move_to_visible_root` land on it, and it has no siblings; its
  descendants are unaffected. A `BufferDocument` always has a document node at
  the root of its tree, but the XDM allows a parentless element, attribute,
  comment, processing instruction or text node, and a host that constructs
  such nodes has to build them somewhere. Under it `parent::`, `ancestor::`,
  the sibling axes, `following::` and `preceding::` stop at `node`,
  `fn:root()` lands on it, a leading `/` or `//` raises `XPDY0050` because the
  root is not a document node, and `find_element_by_id` searches the subtree
  of `node` only (`fn:id` itself raises `FODC0001` there, for the same
  reason).
  `new_assertion` cuts the same links at the asserted element, but keeps
  `fn:root()` and the absolute paths on the hidden document node. The cut
  applies to the node itself only: its attribute and namespace nodes keep it
  as their parent, and the parentless view travels with `move_to`. On the
  document node `new_orphan` is the ordinary navigator; it panics when `node`
  is not a node of `doc`.
- `BufferDocument::set_document_base_uri(uri)` and
  `BufferDocument::document_base_uri()` — the **document-level base URI**, the
  base URI a node reports once the walk up its `xml:base` ancestors reaches
  the document node without finding one. A parser is handed bytes and cannot
  know the URI a document was retrieved from, so a document built by
  `from_reader` or by a `BufferDocumentBuilder` still starts with none and
  `fn:base-uri` still falls back to the static base URI of the expression; a
  host that does know records it here, and `fn:base-uri` and
  `fn:document-uri` then report it, with any `xml:base` attribute on the node
  or an ancestor still taking precedence and being resolved against it.
- `BufferDocument::source_span(node_ref)` and
  `BufferDocument::has_source_spans()`. The per-node byte ranges a document
  records under `BufferDocumentOptions::track_source_locations` were only
  reachable from inside the crate; a host that parses a document and wants to
  report an error at a line and column of its own copy of the source text can
  now read them.
- `xpath::functions::special::error` — an implementation of `fn:error` for
  arities 0 to 3, which raises a dynamic error identified by the supplied
  QName, defaulting to `err:FOER0000`, and accepts and ignores
  `$error-object`. The four declared signatures are enforced by the
  implementation itself, because the engine does not check a registered
  function's declared parameter types at evaluation time and a host may
  register the function loosely: `$error` is one `xs:QName` in the
  one-argument form and `xs:QName?` in the other two, and `$description` is
  one `xs:string` — so `error(())`, `error((), 42)` and `error((), ())` are
  `XPTY0004`, while `error((), 'why')` raises `FOER0000`. It is **opt-in**: the
  built-in catalog does not contain it, so `error()` in an expression is still
  `XPST0017` until a host registers the function with `FunctionSet::register`,
  for which its documentation carries a worked example that declares one
  signature per arity.
- `XPathError::raised`, `XPathError::raised_error` and the `RaisedError` type,
  which carry a dynamic error identified by an arbitrary error QName — the one
  `fn:error` raises, and the spec-defined codes that have no variant of their
  own. `XPathError::error_code` resolves the codes listed in
  `QNAMED_ERROR_CODES` — `FOER0000`, `FOCH0004`, `FODC0001`, `FONS0004`,
  `FORG0002` and `XPST0080` — back to their `'static` string, and
  `XPathError::no_namespace_for_prefix` builds `FONS0004`. The namespace and
  the default local name are exposed as `XQT_ERRORS_NAMESPACE` and
  `DEFAULT_RAISED_ERROR`. `RaisedError` is `#[non_exhaustive]`: a host reads
  its fields but does not build one.
- `BufferDocument::has_type_annotations` — a constant-time, exact answer to
  "does any element or attribute of this document carry a schema type
  annotation?", maintained where bindings are attached instead of computed by
  walking the tree. It is `true` exactly when some node would report
  `DomNavigator::type_annotation() == Some(_)`, and follows the annotation
  mode of a copy.
- `BufferDocument::get_element_by_id_in_tree(node, id)`, the id lookup scoped
  to the tree containing `node`, answering with the first such element in
  document order. A `DocumentKind::Full` document is one tree — every
  top-level node, a comment or processing instruction before the document
  element included, sits under its document node — so there it answers as
  `get_element_by_id` does; in a `DocumentKind::Fragment` buffer each child of
  the document node is a tree of its own. Passing the document node searches
  the whole buffer, and a `node` that is not a node of the document answers
  `None`.
- An attribute whose typed value is a single `xs:ID` is now an is-id node,
  found by `fn:id` and `BufferDocument::get_element_by_id`, not only `xml:id`
  (XDM §6.3.4): its type is `xs:ID` or
  derived from it, or a union whose value is of such a member, or a list of
  `xs:ID` of length one — and the value is valid. That holds wherever the
  attribute is annotated: in a document built by `build_typed_document`
  (including a type chosen by XSD 1.1 conditional type assignment), in a
  copy made with `Annotations::Preserve`, and wherever
  `BufferDocumentBuilder::set_node_binding` binds an attribute. Binding it
  again replaces what the earlier binding filed.
- Five small public helpers that come with the path, node-test and URI fixes
  below:
  - `xpath::ast::default_forward_axis` — the axis an abbreviated forward step
    runs on: `attribute` for an `attribute(…)` or `schema-attribute(…)` test,
    `child` otherwise (XPath 2.0 §3.2.4);
  - `xpath::ast::PathStepNode::abbrev_forward` — builds an abbreviated
    forward step on that axis;
  - `xpath::node_test::principal_node_kind` — the principal node kind of an
    axis: attribute for `attribute::`, namespace for `namespace::`, element
    for every other axis (§3.2.1.1);
  - `xpath::node_test::name_test_for_principal_kind` — the runtime node test
    for a name test that selects only nodes of that principal kind
    (§3.2.1.2);
  - `XPathError::invalid_uri_argument` — builds `FORG0002` for an argument
    that is not a valid URI reference, carried as an error QName.

### Fixed

- An element's `[validity]` now reflects its children, its attributes, its
  identity constraints and — for the validation root — the ID/IDREF table, as
  Structures §3.3.5.1 requires ("Neither its [children] nor its [attributes]
  contains an information item … whose [validity] is invalid"). Before, an
  error anywhere below the root was reported but `DriveOutcome::root_validity`
  and every ancestor's end-element validity stayed `Valid`: an invalid child
  value or attribute, a failing `xs:assert` or assertion facet on a nested
  element, a type selected by a type alternative, a duplicate key or an
  unmatched keyref (even one declared on the root), a duplicate ID or an
  unresolved IDREF, text in element-only or empty content, content in a nilled
  element, an abstract element declaration, an invalid `xsi:` attribute value,
  an undeclared ENTITY value. Each now makes the element it judges invalid, and
  an invalid element makes its parent invalid. An element that is only laxly
  assessed (no declaration and no type, e.g. matched by a `lax` wildcard)
  stays `notKnown` and does not pass invalidity on, so errors inside such a
  subtree are still reported while the root can remain `Valid`. Which errors
  are reported is unchanged.
- Relative schema locations keep a leading `..`. Path normalization let every
  `..` remove the component before it, and on a relative path with nothing
  left to remove the `..` was silently dropped, so
  `SchemaSetBuilder::add("", "../xsd/a.xsd")` read `xsd/a.xsd` below the
  current directory, and an `include`, `import`, `redefine` or `override` with
  `schemaLocation="../../common.xsd"` resolved against the relative base URI
  `schemas/a.xsd` read `common.xsd` instead of `../common.xsd`. A `..` now
  cancels only a preceding directory name; it is kept at the start of a
  relative path and, as before, dropped at the root of an absolute one.
- `fn:contains`, `fn:starts-with`, `fn:ends-with`, `fn:substring-before` and
  `fn:substring-after` no longer ignore their `$collation` argument. They
  atomized it and dropped it, so a call naming any collation at all was
  answered under the codepoint collation and a collation the implementation
  does not support went unreported. They now use the collation, and a URI the
  implementation does not support raises `FOCH0002` as F&O §7.3.1 requires.
  `fn:index-of`, `fn:distinct-values`, `fn:min` and `fn:max` ignored theirs in
  the same way and likewise use it now — `fn:min` and `fn:max` only for
  `xs:string` items, since F&O §15.4.3 ignores the collation for every other
  type.
- A name test on the `namespace::` axis now selects namespace nodes. The
  principal node kind of that axis is namespace, and a namespace node's name is
  its prefix, in no namespace — but the name-test matcher rejected every node
  that was not an element or an attribute, so `namespace::*` and
  `namespace::p` always returned the empty sequence.
- The lexer now accepts `NCName ":" "*"`, the second alternative of the
  Wildcard production. `*:NCName` already lexed and the grammar already had the
  production, but `p:*` failed with a lexer error in every position.
- `TimSort::merge_hi` no longer panics in a debug build. The algorithm walks
  its cursors backwards and deliberately lets two of them step one position
  past the front of the array, which is the reference implementation's `-1`
  sentinel; with `usize` indices that is wrapping arithmetic, and a plain
  subtraction panicked under `cargo test` without `--release` while the
  release build was already correct. Sorting a node sequence in a debug build
  could hit it.
- `for`, `some` and `every` are no longer treated as reserved words. They are
  keywords only when a variable follows, so `@for`, `some:a` and an element
  named `every` now lex as ordinary names instead of failing to parse.
- The operands of the `to` operator now follow the function conversion rules
  (XPath 2.0 §3.3.1): an `xs:untypedAtomic` operand is cast to `xs:integer`
  instead of raising `XPTY0004`, which matters for a range whose bound comes
  from an untyped node.
- An abbreviated forward step whose node test is an `attribute(...)` or
  `schema-attribute(...)` test now runs on the **attribute** axis. XPath 2.0
  §3.2.4: "If the axis name is omitted from an axis step, the default axis is
  `child` unless the axis step contains an AttributeTest or
  SchemaAttributeTest; in that case, the default axis is `attribute`." Such a
  step was given the child axis, so `/doc/attribute()` and `//attribute()`
  always selected nothing.
- The name of an `element(N)` or `attribute(N)` test used as a step's node
  test is now matched instead of ignored, so `child::element(p)` selects only
  `p` children. Previously the name was parsed and dropped and the test
  behaved as bare `element()` / `attribute()`. (Reachable for attribute names
  only since the previous entry.)
- A name test now selects only nodes of the **principal node kind** of its
  axis (XPath 2.0 §3.2.1.1, §3.2.1.2: "A name test is true if and only if the
  kind of the node is the principal node kind for the step axis and the
  expanded QName of the node is equal ... to the expanded QName specified by
  the name test"). `self::*` and `ancestor-or-self::*` used to select
  attribute nodes.
- An unprefixed QName in a step's name test picks up the default
  element/type namespace only on an axis whose principal node kind is element
  (§3.2.1.2); on the `attribute::` and `namespace::` axes it is in no
  namespace. Binding already applied this rule, but the unbound fallback path
  did not.
- The `following::` axis no longer skips the subtrees of the nodes it
  reaches. Only the *context node's* descendants are off the axis
  (§3.2.1.1), so the subtree escape now happens once, at the start, and the
  walk continues in plain document order. From an attribute or namespace
  node the axis now starts at the owner element's first child, which is the
  first node after the context node in document order.
- The `preceding::` axis is now delivered in **reverse document order**, the
  order a positional predicate's focus counts in on a reverse axis, so
  `preceding::x[1]` selects the nearest preceding `x` rather than the
  furthest. The sequence returned by a path expression is still in document
  order.
- The `preceding::` axis of the root of a tree is now the empty sequence
  instead of the whole tree: every node of the tree is a descendant of the
  root and therefore follows it in document order.
- A namespace **undeclaration** (`xmlns=""`, or `xmlns:p=""` under Namespaces
  1.1) is no longer exposed as a namespace node with a zero-length URI. It is
  the absence of a binding, so it disappears from the `namespace::` axis and
  from `fn:in-scope-prefixes` while still shadowing the outer binding for
  that prefix. The declaration remains visible to serialization, to shallow
  copy, and to the namespace context that resolves QNames during validation,
  which all model declarations rather than namespace nodes.
- The `/` operator now evaluates its right-hand operand once per node of its
  left-hand operand, with the inner focus XPath 2.0 §3.2 and §2.1.2 prescribe.
  The whole operation was applied to the concatenated left-hand sequence, with
  three consequences, all fixed: a step's predicates saw that concatenation
  instead of the sequence the step produced for one context node, so
  `a/b[last()]` returned only the last `b` of the last `a` and `a/b[1]` only
  the first `b` of the first `a`; `fn:position()` and `fn:last()` in a
  non-predicate right-hand operand reported 1 instead of the item's position in
  the left-hand sequence and that sequence's size; and the per-operation
  "returned in document order" / "duplicate nodes are eliminated" rules were
  applied at most once, at the end of the whole path, so
  `(item[5],item[3])/@val` came back in the order written and
  `(a,a)/b` returned every `b` twice. A right-hand operand whose evaluations
  return both a node and an atomic value now raises `XPTY0018`, and a
  non-node left-hand operand raises `XPTY0019` for every kind of right-hand
  operand rather than only for axis steps. Note that `//x[1]` is, as the
  specification defines it, the first `x` child of *each* node — not
  `(//x)[1]`.
- A leading `/` or `//` raises `XPDY0050` when the root of the tree that
  contains the context node is not a document node, as XPath 2.0 §3.2
  requires: "At evaluation time, if the root node above the context node is
  not a document node, a dynamic error is raised [err:XPDY0050]". That is the
  case under `BufferDocNavigator::new_orphan` for any node but the document
  node, and in the test of an XSD 1.1 type alternative, whose data model
  instance is rooted at the element itself (Structures §3.12.4). There `/`
  used to select the element, so `/@kind = 'a'` could select a type; a test
  that uses a leading `/` or `//` is now false, which is how §3.12.4 treats a
  dynamic error. Assertion navigators are unaffected: their root is the
  document node.
- An operator applied to an operand combination that XPath 2.0 Appendix B.2
  does not define now reports the specification's type error code. §3.4 and
  §3.5.1 both say "a type error is raised [err:XPTY0004]", but
  `XPathError::error_code()` returned `None` for
  `BinaryOperatorNotDefined`/`UnaryOperatorNotDefined`, and the arithmetic path
  raised an internal error instead of either, so `3 + '2'`,
  `xs:gYear("2000") gt xs:gYear("2001")` and `xs:date("2000-01-01") * 2`
  surfaced without a code. The two error variants and their `Display` text now
  carry `XPTY0004`.
- Unary `+` is `op:numeric-unary-plus`: an `xs:untypedAtomic` operand is cast
  to `xs:double` (`FORG0001` when it cannot be), a non-numeric operand is
  `XPTY0004`, and the result is an `xs:integer`, `xs:decimal`, `xs:float` or
  `xs:double` (XPath 2.0 §3.4, F&O §6.2.7). It returned its operand unchanged,
  so `+'3'` was the string `'3'`.
- A general comparison no longer reports `false` (or, for `!=`, `true`) when a
  pair of operands cannot be compared. `'001' = 1` and `'001' != 1` both raise
  `XPTY0004`: §3.5.2 defers each pair to the corresponding value comparison and
  §3.5.1 makes an operand combination outside B.2 a type error. The
  specification's freedom to "return true as soon as it finds an item in the
  first operand and an item in the second operand that have the required
  magnitude relationship" is kept — the type error is only reported when no pair
  compares true, so `(1,'a') = 1` is still `true`.
- An operand of an arithmetic operator, a value comparison, `to` or `cast as`
  that atomizes to more than one item now raises `XPTY0004` instead of the
  generic "more than one item" dynamic error. All four sections state the same
  rule: "If the atomized operand is a sequence of length greater than one, a
  type error is raised [err:XPTY0004]" (§3.4, §3.5.1, §3.3.1 via the function
  conversion rules, §3.10.2). `(2,3,4) eq (2,3)` reported `XPDY0050`.
- A built-in function argument of more than one item where the parameter
  allows at most one is `XPTY0004` (XPath 2.0 §3.1.5), and so is an operand of
  `is`, `<<` or `>>` holding more than one node (§3.5.3): "Each operand must
  be either a single node or an empty sequence; otherwise a type error is
  raised [err:XPTY0004]". Both reported `XPDY0050`, which the evaluator now
  raises only for `treat as` and a leading `/` or `//`: `string(('a','b'))`
  and `upper-case(('a','b'))` are `XPTY0004`. `fn:name`, `fn:local-name`,
  `fn:namespace-uri`, `fn:node-name`, `fn:nilled`, `fn:base-uri`,
  `fn:document-uri` and `fn:root` raise `XPTY0004` for more than one node, for
  an atomic argument and, called without an argument, for an atomic context
  item, where they used the first node or answered as for the empty sequence:
  `name(/doc/a)` over two `a` elements returned `a`, and `local-name(1)` the
  empty string. The public helper `xpath::functions::atomize_to_single_opt`,
  and the `atomize_to_*` helpers built on it, report `XPTY0004` for more than
  one item as well.
- `treat as` now raises `XPDY0050` on every failure path, not `XPTY0004`.
  §3.10.5: "If `expr1` matches `type1`, using the rules for SequenceType
  matching, the `treat` expression returns the value of `expr1`; otherwise, it
  raises a dynamic error [err:XPDY0050]" — which covers the wrong cardinality
  as much as the wrong item type. `() treat as empty-sequence()` also stopped
  failing: `empty-sequence()` carries no occurrence indicator of its own, so it
  is no longer run through the cardinality check first.
- A QName used as an `AtomicType` that does not name an atomic type in the
  in-scope schema types is now the static error `XPST0051` (§2.5.4.2, §3.10.2,
  §3.10.3) instead of silently yielding `false` from `instance of` and
  `castable as`, or a run-time type error from `cast as`. This covers an unknown
  name (`instance of nosuchtype`), an unprefixed name with no default
  element/type namespace in scope (`castable as double`), the non-atomic
  built-ins the specification's own note calls out (`xs:IDREFS`, `xs:NMTOKENS`,
  `xs:ENTITIES`, `xs:anyType`, `xs:anySimpleType`, `xs:error`), and a name that
  a schema set in the static context does not define as a simple type.
  `xs:anyAtomicType` and `xs:NOTATION` are atomic types and remain accepted in
  `instance of` and `treat as`; as the target of `cast as` or `castable as`
  they are the static error `XPST0080` (§3.10.2, §3.10.3), which
  `XPathError::error_code` reports for an error carried as an error QName —
  `'a' castable as xs:NOTATION` was `true`, and `1 cast as xs:NOTATION` failed
  at run time with `FORG0001`. A user-defined simple type of the schema set
  passes the `XPST0051` check but is not yet supported as an `AtomicType`: as
  in 0.2.0, `instance of` and `castable as` answer `false` for it, `treat as`
  fails and `cast as` raises `XPST0051` when it is evaluated, and a list or
  union type is not rejected at compile time.
- The `TypeName` of an `element(N, T)` or `attribute(N, T)` test is now matched
  instead of ignored — in `instance of` and `treat as`, and as the node test of
  a path step, inside `document-node(…)` too — so
  `/doc instance of element(*, xs:integer)` is no longer `true` for a document
  that was never validated, and `/doc/element(*, xs:integer)` no longer selects
  every child. §2.5.4.3 and §2.5.4.5 require
  `derives-from(AT, TypeName)`, `AT` being the node's type annotation; an
  untyped element's annotation is `xs:untyped` and an untyped attribute's is
  `xs:untypedAtomic`, which derive from none of the ordinary schema types (but
  do derive from `xs:anyType`, and the latter also from `xs:anySimpleType` and
  `xs:anyAtomicType`). For an annotated node the comparison uses the schema
  set's derivation relation, so a user-defined type name works as well.
  `element(N, T)` now also requires the node's nilled property to be false,
  which only `element(N, T?)` relaxes. A step applies the whole test before its
  predicates, so `element(*, T)[1]` is the first child that matches it. A
  `TypeName` that is not in the in-scope schema types — the built-in types of
  the XML Schema namespace, `xs:untyped` included, and the named types of the
  static context's schema set — is the static error `XPST0008` (§2.5.4.3,
  §2.5.4.5); it used to be accepted and ignored, so without a schema set a type
  name outside the XML Schema namespace now fails to compile.
- In XPath 1.0 compatibility mode — `XPathContext::with_xpath10_compatibility`
  (see *Added*), or `XPathMode::XPath10`, which implies it — the conversions
  XPath 2.0 adds in that mode are applied. §3.1.5: if an
  argument **is not of the expected type**, and that type is a single or optional
  single item, the argument is replaced by its first item, an argument whose
  expected type is `xs:string`/`xs:string?` by `fn:string` of it, and one whose
  expected type is `xs:double`/`xs:double?` (the declared type of every `numeric`
  parameter) by `fn:number` of it — so `round(concat('20','.7'))` is `21` instead
  of a type error, while `compare((), '')` and `round(())`, whose argument
  already is of the expected type, keep returning the empty sequence. The
  condition is about the argument's *static* type, which §2.2.3.1 leaves
  implementation-dependent without the Static Typing Feature: an argument that
  evaluates to the empty sequence counts as being of an optional expected type
  only when the argument expression is the literal `()`, the one expression
  §2.3.4 allows the static type `empty-sequence()`, so a path expression that
  selects no nodes is still converted and `round(doc/none)` is `NaN`. §3.4: an
  arithmetic
  operand is likewise reduced to its first item and converted with `fn:number`,
  and an empty operand makes the whole expression `NaN` rather than the empty
  sequence — so `1 + (6 to 10)` is `7` and `1 + ()` is `NaN`. The same first-item
  rule applies to the operands of `to` (§3.3.1), whose expected type is
  `xs:integer?`. The date, time and duration types are outside §3.4's conversion
  list, so arithmetic over them keeps the XPath 2.0 operator mapping in
  compatibility mode, and `idiv`, which XPath 1.0 does not have, keeps its
  integer result. None of this changes anything when the flag is off and the
  language mode is XPath 2.0. In `XPathMode::XPath10`, `1 + /doc/a` over two
  `a` elements therefore adds the first one, where it raised `XPDY0050`.
- `fn:matches`, `fn:replace` and `fn:tokenize` now reject regular-expression
  syntax that XPath 2.0 does not define. The backing engine implements a later
  dialect, which added the `(?…)` group forms and the `q` flag; an XPath 2.0
  `(` always opens a capturing group and can never be followed by `?`, and the
  only defined flags are `s`, `m`, `i` and `x`. `(?:a)` now raises `FORX0002`
  and `q` raises `FORX0001`. An escaped `\(?` and a `?` inside a character
  class, including a subtracted one such as `[a-z-[(?]]`, are unaffected, and
  so are `xs:pattern` facets, which compile through a separate path. With the
  `x` flag the check reads the pattern after whitespace has been removed, so
  `\ (?` is the escaped `\(?` and is accepted, as F&O §7.6.1.1 requires.
- `fn:tokenize` no longer drops zero-length tokens. Every gap between two
  matches of the separator is a token, so a leading separator produces an empty
  first token, a trailing separator an empty last token, and two adjacent
  separators an empty token between them: `tokenize("a,,b", ",")` now returns
  three tokens and `tokenize(",a,", ",")` returns `("", "a", "")`.
- String literals are no longer re-decoded by the lexer. XPath 2.0 §3.1.1 makes
  a literal's value the characters between the delimiters, a doubled delimiter
  standing for one; XML un-escaping belongs to the host language and has already
  happened by the time the expression text arrives. The lexer used to expand XML
  entity and character references a second time and fold CR/CRLF to LF, so
  `concat('a&b','!')` failed to compile, `string-length('&#13;')` was 1 instead
  of 5, and a literal carriage return became a line feed.
- A `DecimalLiteral` may now end with its period. XPath 2.0 §A.2.1 gives
  `DecimalLiteral ::= ("." Digits) | (Digits "." [0-9]*)`, so `5.` is a valid
  literal; it previously failed to lex.
- A numeric literal whose exponent has no digits — `1e`, `5.0e`, `1e+` — is a
  syntax error (`XPST0003`), as the `DoubleLiteral` production of XPath 2.0
  §A.2.1 requires; it lexed as a double literal and evaluated to NaN.
- The lexer's `NCName` predicates now implement the XML 1.0 §2.3
  `NameStartChar` / `NameChar` productions (minus `":"`) instead of Rust's
  Unicode `Alphabetic` / `Alphanumeric` properties, which admitted characters
  such as U+00B5 MICRO SIGN that the productions exclude.
- Only the XML Schema lexical forms convert to `xs:double` and `xs:float`. A
  string or `xs:untypedAtomic` value was converted with Rust's own float
  parser — in `cast as`, `castable as` and the constructor functions, in
  arithmetic and comparisons, in `fn:number`, `fn:sum`, `fn:avg`, `fn:min` and
  `fn:max` — which also accepts `inf`, `infinity` and `nan` in any case and
  trims Unicode whitespace. Those spellings are now not numbers (XPath 2.0
  Appendix I.1): `xs:double('inf')` raises `FORG0001`, as does an
  `xs:untypedAtomic` `inf` in arithmetic, a comparison or an aggregate,
  `'inf' castable as xs:double` is `false`, and `number('Infinity')` is `NaN`;
  and only XML whitespace around a number is ignored. `INF`, `+INF`, `-INF`
  and `NaN` are unchanged. `xpath::atomize::to_number`, the public helper
  behind `fn:number`, follows the same rule.
- SequenceType matching now follows the whole built-in atomic type hierarchy
  instead of a hand-written table covering only the string and numeric
  branches. Two results change: `xs:untypedAtomic` is no longer an `xs:string`
  (it derives from `xs:anyAtomicType`), and `xs:dayTimeDuration` and
  `xs:yearMonthDuration` now are `xs:duration`s. The other branches —
  `xs:dateTimeStamp` under `xs:dateTime`, the `xs:ID`/`xs:IDREF`/`xs:ENTITY`
  leaves of the string branch — are now modelled as well.
- `fn:min` and `fn:max` accept every type that has an ordering, not only the
  numeric and string ones: `xs:date`, `xs:time`, `xs:dateTime`, `xs:boolean`
  and the two ordered duration types used to raise `FORG0006`. A sequence that
  mixes primitive types still raises `FORG0006`, which it did not always do
  before. NaN is returned as a value of the numbers' least common type, so
  `max((xs:float('NaN'), 1))` is an `xs:float`; it was always an `xs:double`.
- `fn:index-of` and `fn:distinct-values` compare with the `eq` operator, as
  F&O §15.1.5 and §15.1.6 require. They compared the type as well as the
  value, so an `xs:token`, an `xs:ID` or an `xs:NMTOKEN` never equalled the
  `xs:string` it spells: `index-of(xs:token('a'), 'a')` was the empty
  sequence, and so was F&O's own example, `index-of(@a, "blue")` over an
  `xs:NMTOKENS` attribute `red green blue`. Dates and times now also compare
  on the timeline, durations by value across their subtypes (`P12M` and `P1Y`
  are one distinct value) and QNames by expanded name; a pair `eq` cannot
  compare is still distinct, and `fn:distinct-values` still treats NaN as
  equal to NaN.
- `fn:sum` no longer widens integers: it accumulates with `op:numeric-add`, so
  `sum((1, 2, 3))` is an `xs:integer` rather than an `xs:decimal`. `fn:avg`
  keeps returning an `xs:decimal` for an integer input, because dividing two
  integers yields one.
- `fn:sum` and `fn:avg` raise `FORG0006` for a mixture of individually
  summable types — a number and a duration, or an `xs:yearMonthDuration` and
  an `xs:dayTimeDuration` — as F&O §15.4.5 and §15.4.2 require. Such a sum
  failed with an error that carried no code.
- `fn:sum`'s `$zero` argument is honoured when it is itself the empty sequence:
  `sum((), ())` is now the empty sequence instead of the integer 0. Omitting
  the argument still gives 0.
- `fn:round-half-to-even` decides a tie on the exact value of its argument
  rather than on a scaled binary product, and a zero result keeps the sign of
  the argument. `round-half-to-even(250.0250e0, 2)` is now `250.03`,
  `round-half-to-even(xs:float(150.0150e0), 2)` is `150.01`, and
  `round-half-to-even(-3.0e0, -2)` is `-0`. A large precision no longer gives
  NaN: `round-half-to-even(1234.5678e0, 400)` and
  `round-half-to-even(5e-324, 400)` return their argument.
- `fn:subsequence` now evaluates its two position comparisons in `xs:double`
  arithmetic as the specification defines them, so infinite and NaN arguments
  behave: `subsequence(1 to 20, -INF, INF)` is the empty sequence, because the
  two bounds sum to NaN.
- `idiv` on `xs:float` and `xs:double` operands raises `FOAR0002`, not
  `FOAR0001`, when an operand is NaN or the dividend is infinite (F&O
  §6.2.5); `FOAR0001` remains the code for a zero divisor. A quotient beyond
  the 64-bit range is now exact — `1e20 idiv 1` is `100000000000000000000` —
  where it saturated at `9223372036854775807`.
- `fn:nilled` reports the post-schema-validation property instead of looking
  for an `xsi:nil` attribute, so an element that was never validated is not
  nilled.
- `fn:resolve-QName` reports the specified error codes: `FOCA0002` for a value
  that is not a lexical QName (it raised `FORG0001`, the code for a failed
  `cast as xs:QName`) and `FONS0004` for a prefix the element does not bind (it
  raised the static error `XPST0081`).
- `fn:base-uri` follows the XDM accessor rules for the remaining node kinds: a
  namespace node has no base URI, and an attribute node has its parent
  element's or none at all when it has no parent. A namespace node used to
  report its owner element's base URI, and a parentless attribute the
  document's.
- `fn:resolve-uri` validates both arguments and reports `FORG0002` for one that
  is not a valid URI reference — including a base URI that is not absolute —
  keeping `FORG0009` for a resolution that fails. The validation also rejects
  more than one `#` and the characters no URI production allows, which it let
  through before. A base URI that has a fragment identifier or is not
  hierarchic (`mailto:…`, `urn:…`) is `FORG0002` as well, and so is a
  zero-length base when `$relative` is relative, which raised `FORG0009`
  (F&O §8.1).
- `RoXmlNavigator::name` returns the qualified name as the document writes it,
  for elements and for attributes. It returned the local name, dropping the
  prefix, which also affected `fn:name` and serialization through that
  navigator. The name is read back out of the document's source text, and the
  scan ends at XML's whitespace — the four characters of XML 1.0 §2.3 production
  `S` — and not at Unicode's, so a name containing U+1680 OGHAM SPACE MARK or
  another character that is a legal XML `NameChar` in `[#x37F-#x1FFF]` is
  returned whole.
- `fn:deep-equal` compares namespace nodes. A namespace node has no children,
  so the generic child-sequence comparison reported every pair of them as
  equal; two namespace nodes are now deep-equal only when their names — the
  prefix, absent for a default-namespace binding — and their string values, the
  bound namespace URI, both match.
- `schema-element(N)` and `schema-attribute(N)` used as **step** node tests are
  the declaration-aware tests of XPath 2.0 §2.5.4.4 and §2.5.4.6. As a step
  they matched every node on the axis. Both tests, in every spelling — a step,
  `instance of`, `treat as`, a function signature — are now decided by one
  implementation, which also gained the substitution-group clause and the
  nilled clause and accepts a complex-typed declaration. A name that is not
  declared matches nothing — §2.5.4.4 and §2.5.4.6 make it the static error
  `XPST0008`, which this release does not raise — and without a schema set in
  the static context the tests remain name-only, as before.
- Atomizing a namespace node yields `xs:string`, not `xs:untypedAtomic` — the
  rule that already applied to comments and processing instructions.
- `fn:namespace-uri-for-prefix` returns the empty sequence, not
  `xs:anyURI("")`, when the prefix has no binding — including the empty prefix
  on an element with no default namespace.
- A namespace prefix with no binding inside a kind test — `element(p:x)`,
  `attribute(p:a)`, `element(*, p:T)`, `schema-element(p:x)`, the same inside
  `document-node(...)` or a SequenceType — is the static error `XPST0081`. It
  silently resolved to no namespace.
- Under XSD 1.1 assertion scope the data model instance is cut at the asserted
  element, as §3.13.4.1 clause 1.3 constructs it: `parent::`, `ancestor::`,
  `following::`, `following-sibling::` and `preceding-sibling::` no longer
  reach outside it (`preceding::` and the downward axes never did). As the
  specification's note says, such a reference is not an error, it selects
  nothing. `/` and `fn:root()` are unchanged: they still answer a document
  node whose children are hidden, so `//x` inside an assertion is empty, which
  is what the W3C test suite expects.
- Inside an XSD 1.1 assertion `fn:id` finds the elements inside the asserted
  element, and only those, as §3.13.4.1 clause 1.3 constructs the data model
  instance. It found nothing at all in an assertion, because the buffer the
  validator builds to evaluate assertions indexed no ids: now
  `exists(id('y'))` holds on an element with a descendant `xml:id="y"`, while
  an element outside the asserted one — a sibling, say — is never found, even
  when it claims the value first.
- `fn:deep-equal` no longer looks at comment and processing-instruction
  **children**. F&O §15.3.1 compares a document or element node's
  `$i/(*|text())`, a sequence that holds neither kind, so
  `deep-equal(<a>x<!--c--></a>, <a>x</a>)` is now true where it was false.
  Such a child still *splits* the text around it and text nodes are never
  merged, so `<a>x<!--c-->y</a>` has two text children and remains different
  from `<a>xy</a>`; and a comment or PI that is itself an item of the two
  compared sequences is still compared, by string value and, for a PI, by
  target. `TreeComparer` is unchanged: its `deep_equal` and `deep_equal_iter`
  keep comparing every child, which is what a serialization round-trip check
  needs.
- `fn:deep-equal` compares two elements according to their **content**, as
  F&O §15.3.1 clauses (2) and (4) prescribe, instead of always comparing their
  children. Two elements annotated as having simple content — a simple type,
  or a complex type whose content is text-only — are compared by their typed
  values, so a validated `<a>1</a>` and `<a>01</a>` of type `xs:integer` are
  now deep-equal. With element-only content only the child elements are
  compared, so whitespace between them no longer counts; with mixed content
  the `(*|text())` sequences are compared, as before. An element annotated as
  having simple content is never deep-equal to one with complex content.
  Without a schema set in the static context the content kind of a complex
  type is not read: the element's typed value decides between simple and
  complex content, complex content is compared as mixed, and a nilled element
  of a complex type is compared as having the other element's content kind.
- `fn:deep-equal` compares two attributes' typed values with `eq` semantics,
  the same rule it applies to a free-standing atomic item, instead of plain
  value equality. The two answers could differ for schema-validated
  attributes: an `xs:integer` `1` and an `xs:decimal` `1.0` were reported
  different as attributes and equal as items. Untyped attributes are
  `xs:untypedAtomic` and compare as strings either way, so nothing changes for
  an unvalidated document.
- `fn:tokenize` reports an invalid `$pattern` (FORX0002), invalid `$flags`
  (FORX0001) and a `$pattern` that matches the zero-length string (FORX0003)
  when `$input` is the empty sequence or a zero-length string. It returned the
  empty sequence for such an input before looking at the pattern at all, so
  `tokenize("", "[")` and `tokenize((), "a*")` silently succeeded. F&O §7.6.4
  states the three errors with no exemption for any input, and states the
  empty-sequence result separately; that result is unchanged for a pattern
  that raises nothing, so `tokenize("", "\s+")` is still the empty sequence.
- `fn:replace` reports a malformed `$replacement` (FORX0004) even when nothing
  is replaced — when `$input` is empty, or when `$pattern` matches nowhere in
  it. The replacement string was only validated while the matches were walked,
  so `replace("", "a", "$")` and `replace("abc", "z", "$")` returned a string
  instead of raising. The two rules are unchanged: a `\` must be followed by a
  `\` or a `$`, and a `$` that is not part of such a pair must be followed by
  a digit.
- `fn:id` found an element by its `xml:id` only in a `DocumentKind::Full`
  document read from text (`from_reader` or `build_typed_document`), and only
  when the attribute value had no surrounding whitespace. The id index is now
  filled from `BufferDocumentBuilder::attribute` for an attribute of an
  element, the one place an attribute node is created, so a document built
  through the push API, a `Fragment` buffer and a copied subtree carry their
  ids too; and the index key is the value with XML whitespace stripped and
  collapsed, so `xml:id="d "` answers to `id(' d')`. Only the key is
  normalized — the attribute keeps its own value.
  `BufferDocument::get_element_by_id` reads the same index: it normalizes its
  argument the same way, never answers for a value that is not an NCName
  (`xml:id="a b"` and `xml:id="1x"` used to be found by their literal value),
  and finds `xs:ID`-typed attributes too (see *Added*).
- `fn:id` tokenized its argument on Unicode whitespace and looked up
  every token. It now follows F&O §15.5.2: the candidate IDREFs are
  `tokenize(normalize-space($s), ' ')` — XML whitespace only — and a
  token that is not a lexical NCName is ignored instead of looked up.
  An `xml:id` whose value is not an NCName is never selected.
- `fn:id` returned an empty sequence where the specification requires an
  error: it now raises `FODC0001` when the node it is given is in a tree
  whose root is not a document node. In the test of an XSD 1.1 type
  alternative, whose data model instance is rooted at the element itself
  (Structures §3.12.4), `fn:id` therefore makes the test false, as any
  dynamic error there does; it used to see the empty sequence.
- A `Fragment` buffer — whose document node holds parentless trees — now
  indexes the ids of each of its trees, and
  `BufferDocumentBuilder::register_xml_id`, which did nothing at all for a
  `Fragment` buffer, files them there too. When two elements of one tree share
  an id, the first in document order is the one selected, as F&O §15.5.2
  requires, and a tree built through the push API is not refused for it.
  Reading a `Full` document from text — with `from_reader` or
  `build_typed_document` — still reports `BufferDocumentError::DuplicateId`,
  now comparing the normalized values, so `xml:id="a"` and `xml:id=" a"`
  collide where they used to be accepted, and ignoring values that are not
  NCNames, which are never ids, so two `xml:id="1x"` are no longer reported.
  `register_xml_id` normalizes its value the same way, refuses one that
  another element of the same tree already has (in a `Full` document, any
  other element), accepts a repeat for the same element, which used to be
  `DuplicateId`, ignores a node that is not an element, and panics on a node
  reference that is not a node of the document being built, where it used to
  accept any reference unchecked.
- `XPathValue::as_slice` returns a one-element slice for a single item. It
  returned an empty slice for the `Item` variant, so a caller that borrowed a
  one-item value as a slice saw no items while `len()` said 1. The slice now
  always has `len()` elements, in sequence order, and is borrowed without
  cloning; the signature is unchanged, and a caller that matched the `Item`
  variant to work around it keeps working.
- `BufferDocumentBuilder::copy_attribute`, replacing an attribute of the
  same expanded name with one that carries no type annotation — a copy with
  `Annotations::Strip`, or of an untyped attribute — left the earlier
  attribute's annotation on the node, so the new value was reported with the
  old type. The later attribute now wins with its annotation or the lack of
  one, and `BufferDocument::has_type_annotations` stays exact.
- Atomizing a node whose type is a list type now yields one atomic value per
  list member, each typed with the list's item type (XDM §3.3.1.2, XPath 2.0
  §2.5.2). It yielded the whole list as a single value, so `data(@refs)` of an
  `xs:IDREFS` attribute holding `a b` had one item, and every operator and
  function saw that one value. This changes answers that a schema-aware host
  can observe: `@refs = 'b'` is now true where it was false (or raised
  `XPTY0004` for a built-in list type such as `xs:NMTOKENS` or `xs:IDREFS`),
  and `@refs = 'a b'` is now false where it was true; `@refs eq 'b'`,
  arithmetic on a list of several members and `xs:string(@refs)` now raise
  `XPTY0004` instead of answering (or failing with an error that had no code);
  `sum()`, `avg()`, `min()` and `max()` of a list of numbers add and compare
  the members instead of returning the list's text; `count(data(@refs))`,
  `distinct-values()`, `index-of()`, `string-join()` and `deep-equal()` see
  the members; a list with no members atomizes to the empty sequence. XPath
  1.0 compatibility mode applies its conversions to each member. The string
  value of such a node, and `fn:string`, are unchanged: still the whole text.
  `atomize::atomize_node` still returns the typed value as it is stored, one
  value of kind `XmlValueKind::List`. For a list whose item type is a union,
  the member type the validator actually chose is not recorded per member:
  each member is typed with the list's recorded item type, or `xs:string`
  when it holds a lexical form that type cannot hold.
  `compose::Value::atomics` likewise yields one value per member, and
  `Value::number` and `Value::key` raise `XPTY0004` for a single node whose
  list-typed value has several members and treat one with no members as the
  empty sequence (`NaN`, `None`); they used to see the whole list as one
  value.
- A nilled element, whose typed value is the empty sequence, now makes a value
  comparison, an arithmetic expression or a unary `+`/`-` return the empty
  sequence, as XPath 2.0 §3.4 and §3.5.1 require, instead of raising
  `XPTY0004`; `castable as T?` of it is true and `castable as T` false.
- The arguments of the built-in functions are now atomized as the function
  conversion rules require (XPath 2.0 §3.1.5): a node contributes its typed
  value, not its string value. A schema-aware host sees the difference:
  `fn:number(@flag)` of an `xs:boolean` attribute `true` is now `1` instead of
  `NaN`, `concat(@n, '')` of an `xs:integer` attribute ` 5 ` is `5`, a string
  argument whose typed value has several members raises `XPTY0004`, an empty
  typed value is the empty sequence (so `compare(@empty, 'a')` is the empty
  sequence), and an element with element-only content passed where a string or
  number is expected raises `FOTY0012`, because it has no typed value.
  `fn:string`, the zero-argument string functions (`string-length()`,
  `normalize-space()`), and the `fn:string(V)` step of XPath 1.0
  compatibility mode keep reading the string value. Untyped nodes are
  unaffected: their typed value is their string value.
- An element whose complex type has mixed content — `xs:anyType` included — now
  has its string value as an `xs:untypedAtomic` typed value, and one whose
  complex type has empty content has the empty sequence, as XDM §6.2.4
  requires. `BufferDocNavigator::typed_value` reported both as having no typed
  value (`TypedValue::Absent`), so atomizing such an element — in a
  comparison, in `fn:data`, in an XSD 1.1 assertion — raised `FOTY0012`. It now
  returns `TypedValue::Untyped` for mixed content and an empty list value for
  empty content; element-only content is still `Absent`.
- `fn:deep-equal` compares two typed values item by item, as F&O §15.3.1
  requires of the typed values of two simple-content elements or two
  attributes: same length, and each pair equal under `eq`, with `NaN` equal to
  `NaN`. A list-typed value was compared as one value, so two lists whose
  members include `NaN`, or an `xs:IDREFS` value and a list-of-`xs:IDREF`
  value with the same members, were reported different. `TreeComparer` is
  unchanged.

### Performance

- The general comparison operators `=` and `!=` no longer walk the whole
  Cartesian product of their operands. XPath 2.0 §3.5.2 makes `A = B` an
  existential test over every pair of atomized items, which the evaluator
  answered with a nested loop — `O(|A| · |B|)`, and unusable once both
  operands hold more than a few thousand items. The comparison now walks a
  bounded prefix of the product, so that a small comparison or an early match
  still costs exactly what it did, and then decides the rest with a hash index
  over the comparison classes of §3.5.2 in `O(|A| + |B|)`: numeric (keyed at
  the type the pair promotes to), string-like (including `xs:untypedAtomic`
  compared as a string, and `xs:untypedAtomic` cast to `xs:double` against a
  numeric), boolean, `xs:dateTime`/`xs:date`/`xs:time` keyed at the instant the
  comparison normalizes them to, the durations keyed at their (months, seconds)
  pair, `xs:QName`, and the types whose equality is structural. Every candidate
  the index finds is confirmed with the ordinary value comparison, and any
  input the index does not model — a conversion that fails, a union-typed
  value, `xs:untypedAtomic` against a type that is neither numeric nor
  string-like — falls back to the original loop, so results and errors,
  including which error a comparison raises and when, are unchanged. Two
  disjoint sequences of 100,000 items now compare in ~25 ms; before, 10,000
  items already took ~16 s.
- A `=` or `!=` comparison that is evaluated over and over — the predicate of
  `$big[. = $c]`, the body of a `for` or a quantified expression — now reuses
  the index of the operand that does not change during the evaluation, instead
  of rebuilding it every time. An operand counts as unchanging only if a
  conservative static analysis says so: it must read no part of the focus, call
  no function, and reference no variable bound by a `for`, `some` or `every`;
  on top of that, every reuse re-checks that the variables it was built from
  have not been rebound. The index lives in the dynamic context of the one
  evaluation run and is dropped with it, nothing is built before the second
  evaluation of the same comparison, and the index only answers a comparison
  when it can prove both the answer and the absence of an error for that pair
  of operands, so results and errors are unchanged. An index is keyed by a
  serial number of the expression's syntax tree, not by its address, so a host
  function that compiles and evaluates expressions of its own on the caller's
  `DynamicContext` is never answered from another expression's index, and
  replacing `DynamicContext::variables` with another store is detected as a
  rebinding. A host function registered
  through `FunctionSet` receives `&mut DynamicContext` and may rebind a
  variable while the other operand of the same comparison is being evaluated;
  such a write is detected, and the comparison then answers exactly what it
  would have answered without an index, with both operands evaluated in source
  order. Filtering 1,000,000 items
  against a 100,000-item sequence goes from days to ~0.4 s; the same filter
  against a one-item sequence is about twice as fast as before.
- `fn:matches`, `fn:replace` and `fn:tokenize` compile each regular expression
  once per evaluation run. They recompiled their `$pattern` argument on every
  call, so a constant pattern inside a predicate —
  `$items[matches(., '\p{Ll}')]` — paid for a full compile, including the
  expansion of any Unicode category it names, once per item. The compiled
  programs of one run are kept in its dynamic context, keyed by
  `(pattern, flags)`, bounded at 32 entries with least-recently-used eviction,
  and dropped when the run ends. A pattern that fails to compile is remembered
  too, and raises the same error every time. Answers, error codes and error
  messages are unchanged. On a 100,000-item filter `matches(., '\p{Ll}')` goes
  from 6.6 s to 67 ms; `replace()` in a loop is 23× faster and `tokenize()`
  39×; a single call, and a pattern computed afresh for every item, cost what
  they did.

## [0.2.0] - 2026-09-13

A breaking release, in three parts:

- **Exact occurrence bounds.** Every finite `maxOccurs` is enforced exactly.
  Execution limits keep pathological counted models finite, and hitting one
  is an operational failure, never a validity verdict. The release also adds
  a content-model inspector and precomputed successor closures on the
  validation hot path.
- **Hosts working with more than one document.** Additive extensions for
  hosts that embed the XPath engine: node identity that includes the
  document, `DomNavigator::type_annotation`, `XPathExpr` dependency
  metadata, the `DynamicContext` extension slot and `set_function_evaluator`,
  an owned default function namespace, and `BufferDocument::serial()`.
- **"XQuery without XQuery".** An XML serializer for any `DomNavigator`
  (`document::serialize`, compact or indented), a subtree copy with
  constructor semantics (`document::copy`), and a `compose` feature with
  `xpath!` and `form!` for building XML from query results in Rust — proven
  against the eighteen queries of the XQuery test suite's relational use
  case.

XPath built-in functions now apply the function conversion rules to
`xs:untypedAtomic` arguments, and `fn:round`, `fn:subsequence` and
`fn:codepoints-to-string` follow F&O to the letter.

Both W3C XSD suites fail exactly the tests 0.1.5 failed (XSD 1.0
39458/39510, XSD 1.1 2313/2319), the XQTS XPath 2.0 selection passes
8047/8047, and the GAEB DA XML 3.3 corpus loads as it did in 0.1.5 (all 32
schemas under XSD 1.1, 23 under XSD 1.0). The crate still builds and tests on
rustc 1.85.0.

### Upgrading from 0.1.x

Source changes, only where you use the item:

- Exhaustive `match`es on `compiler::ContentModelMatcher`,
  `validation::CompiledContentModel` or `validation::ContentValidatorState`
  lose their `AllGroupExtension` arm, and `validation::content::AllGroupExtPhase`
  is gone. No compile path ever constructed them (see *Removed*).
- `compiler::NfaTable` has a private field: build one with `NfaTable::new` or
  `NfaTable::with_counters` instead of a struct literal, and after a table has
  been executed mutate its states through `get_state_mut`.
- `XPathEvaluator::run_with` and `run_with_node_and_setup` require `N: 'ctx`;
  existing call sites satisfy it without change.
- `MaxOccurs::is_effectively_unbounded` is deprecated in favour of
  `is_unbounded`.

Behaviour you may observe without changing code:

- A finite `maxOccurs` above 10 000 is no longer treated as `unbounded`, so an
  instance with more occurrences than the bound is now invalid.
- The drivers (`drive_quick_xml`, `drive_navigator`, `drive_buffer_document`)
  return `Err` when validation cannot complete — an execution limit, or a
  complex type whose content model failed to prepare, which used to be
  validated silently as empty content.
- PSVI `[validity]` is `Invalid` on a parent whose child the content model
  rejected, and on the start event of an element with `xsi:nil` that is not
  nillable. Sink diagnostics are unchanged.
- Schema error locations point at the element's `<`, so line and column
  numbers in messages move.
- XPath: `round(-2.5)` is `-2`; `codepoints-to-string` raises `XPTY0004` for
  an `xs:decimal`, `xs:float` or `xs:double` argument, even a whole-valued
  one; an untyped argument now casts to the expected type, and one whose value
  is not in that type's lexical space raises `FORG0001` where it used to raise
  `XPTY0004`.
- Processing-instruction data is kept verbatim instead of trimmed, and
  `RoXmlNavigator` returns a PI's data as its string value.

### Added

- **Execution limits for counted content models** —
  `compiler::MAX_ACTIVE_CONFIGS` (live counter configurations per frontier)
  and `compiler::MAX_CLOSURE_WORK` (work per epsilon closure). Removing the
  10 000 cutoff means a pathological nested-nullable model such as
  `((a?){0,1000000}){0,1000000}` would otherwise grow its configuration set
  with the bound; the limits keep memory and per-child work finite. Exceeding
  one is an **operational failure**, never an acceptance decision:
  `compiler::ContentModelLimitExceeded` (with `ContentModelLimit`) at the
  automaton level, and a terminal `validation-resource-limit` diagnostic in
  the runtime. Every shape the old cutoff admitted still fits.
- **Dominance pruning for scalar counter configurations.** With exact bounds,
  nested non-nullable counted loops such as the W3C `particlesZ036` models —
  `choice{1,100000}(sequence{1,100000000}(a+), b)` — keep one configuration
  per way of partitioning the input into iterations, O(k²) after k children;
  the 16 660-child instance hit the new execution limit after several seconds
  where the old star approximation finished in a millisecond. A configuration
  whose counters are all equal to, or below-but-past-the-minimum of, another
  configuration at the same state accepts a superset of inputs through the
  same states, so the dominated one is dropped after every closure. Exact by
  construction (a differential test checks every string up to length 8
  against the unrolled automaton), and the frontier for those models is now
  constant; the three affected W3C tests pass again.
- `ValidationRuntime::operational_failure()` — the terminal failure recorded
  for a run. Once set, every push call is inert and `end_validation` returns
  the failure, so `drive_quick_xml` / `drive_navigator` /
  `drive_buffer_document` return `Err` instead of a `DriveOutcome`. A validity
  field without a successful completion is not a result.
- Fallible automaton entry points: `ActiveStates::{try_from_nfa,
  try_epsilon_closure, try_advance, try_advance_with_priority}`,
  `CompiledContentModel::try_from_matcher`,
  `ContentValidatorState::{try_advance_element, try_is_complete}`. The
  existing infallible methods keep their signatures and now panic with a
  clear message if a limit is exceeded (they are no longer used by the
  runtime).
- Regression tests: `tests/occurrence_bounds.rs` (10 001/10 002, a boundary
  matrix around 16 and 10 000, huge literals, both failure contracts on both
  drivers) and `compiler::nfa::exact_bounds_tests` (counter arithmetic at
  `u32::MAX`, limit behaviour).
- **Content-model inspector** (`compiler::inspect`): a readable,
  source-attributed description of what the validator compiled for a complex
  type, in three views — *Source* (type, document, location, content type,
  open content), *Authored particles* (the resolved particle tree with
  occurrence ranges, whether each range is unrolled or counted, declaration
  and type bindings, source locations) and *Compiled* (matcher kind, states,
  transitions including counter operations, counters, the initial frontier
  variant, an exposure line for models that can hit the execution limits, and
  substitution-group expansions). `inspect_content_model(&SchemaSet,
  ComplexTypeKey)` compiles through the validator's own path so the report
  is exactly what validation executes; `SchemaValidator::describe_content_model`
  reuses the prepared model and names a preparation failure;
  `find_complex_type` looks a type up by expanded name. Example:
  `cargo run --example inspect_content_model -- schema.xsd TypeName [ns]`.
- **`DomNavigator::type_annotation() -> Option<TypeKey>`**, with a default body
  returning `None`. The node's XDM type-name property: complex or simple for
  element and attribute nodes, `None` for untyped nodes and for every other
  node kind. `schema_type()` is unchanged and remains its simple-type
  projection. This is the half of `element(*, T)` / `schema-element(x)`
  matching with a complex `T` that can be added without breaking the public
  `ItemType`; `BufferDocNavigator` overrides it, `RoXmlNavigator` keeps the
  default.
- **`BufferDocument::serial()`** — the document's creation ordinal from a
  process-wide counter: unique within the process, strictly increasing in
  creation order. Intended for reproducible cross-document ordering and
  `generate-id()`-style identifiers.
- **`XPathExpr` dependency metadata**, computed once in a post-bind walk
  (nothing on the evaluation path changes):
  `referenced_external_vars()` — the declared externals the expression
  actually references, a subset of the unchanged `external_vars()`, which
  still lists every name supplied to `compile_with_vars`;
  `references_external_var(slot)`, O(1) and bitset-backed;
  `uses_focus()`, `uses_position()`, `uses_last()` — whether evaluation reads
  the *initial* focus (context item, `fn:position()`, `fn:last()`). Foci
  created inside the expression do not count: "Certain language constructs,
  notably the path expression `E1/E2` and the predicate `E1[E2]`, create a new
  focus for the evaluation of a sub-expression" (XPath 2.0 §2.1.2), so
  `$x[position() = 1]` uses neither the focus nor the position, while
  `a[position() = 1]` uses the focus (its first step is relative) but not the
  position; `for`, quantified and `if` expressions keep the enclosing focus.
  Functions that default to the context item (`string()`, `number()`,
  `name()`, `local-name()`, `namespace-uri()`, `base-uri()`, `root()`,
  `string-length()`, `normalize-space()`, `lang($l)`, `id($x)`) count as
  focus uses at the arity that reads it; calls this crate cannot inspect are
  conservatively counted.
  `function_calls() -> &[FunctionCallRef]` — the distinct (namespace, local
  name, arity) triples of every function called anywhere in the expression,
  nested foci included, so a host language can detect its impure calls
  (`doc`, `document`, `key`, `unparsed-text`, `collection`) itself. The
  namespace is the one the call *bound* to, so `fn:` names are reported
  identically in XPath 1.0 and 2.0 mode and the `FN_2010_NAMESPACE` alias is
  normalized. `FunctionCallRef` is re-exported at the crate root and from
  `xpath` under the `xsd11` feature.
- **`DynamicContext::with_extension` / `set_extension` / `extension::<T>()`** — a
  `&dyn Any` slot for host or engine state that extension functions can read
  during evaluation without changing the `FunctionEvaluator::eval(&self, …)`
  contract. One `DynamicContext` serves a whole expression (sub-expressions
  only save and restore the focus in place), so the slot is visible from every
  function call: predicates, path steps, `for` and quantified bodies, nested
  calls.
- **`DynamicContext::set_function_evaluator`** — the mutable counterpart of
  `with_function_evaluator`, so a custom `FunctionEvaluator` can be installed
  from an `XPathEvaluator::run_with` setup callback. Custom functions were
  reachable only through `parse` / `bind_node` / `eval_node` before; they now
  work through the high-level API.
- **`XPathContext::with_default_function_ns_owned(impl Into<String>)`** — the
  default function namespace from a runtime-built `String`, with no
  `&'static str` to leak per host context. `default_function_namespace()`
  prefers it over the public `default_function_ns` field; XPath 1.0 mode still
  yields `""`.
- **`document::serialize` — a `BufferDocument` can be written back out.** Three
  entry points, `serialize_document`, `serialize_node` and `to_string`, plus
  `SerializeOptions` (an optional XML declaration with `standalone`, and
  `indent`) and `SerializeError`. Generic over `DomNavigator` rather than over
  `BufferDocument`, so the same code writes a `BufferDocNavigator`, a
  `RoXmlNavigator` and whatever navigator a host embedding the engine brings,
  using only navigation, names and `value_ref()`. Escaping follows Canonical
  XML 1.0 §2.3 — the rules under which a re-parse cannot change the value back,
  since XML 1.0's end-of-line handling (§2.11) and attribute-value
  normalization (§3.3.3) would otherwise rewrite literal carriage returns and
  attribute whitespace. Namespace declarations are written where they are
  introduced, diffed against an in-scope stack: redundant re-declarations are
  dropped, an element leaving an inherited default namespace gets `xmlns=""`,
  the `xml` prefix and prefixed undeclarations are never written, and a
  subtree written on its own gains the declarations it inherits. Content XML
  cannot express is refused, never dropped: a character outside the XML 1.0
  `Char` production, a comment containing `--`, a PI target `xml` or data
  containing `?>`, and a name whose prefix is not bound to its namespace URI
  in the output scope. Output is compact by default — no layout whitespace
  added and none removed, which is what makes it an exact text round trip —
  while `indent: Some(n)` opts into formatted output (a line break plus `n`
  spaces per level, `Some(0)` the breaks alone) from the same traversal with
  no second tree and no pretty-print pass. Layout is added conservatively
  because it becomes text nodes when parsed again: every existing text
  character survives, any text child of a container suppresses added layout
  throughout that container's subtree (so mixed content such as
  `<p>Hello <b>world</b>!</p>` stays on one line and existing indentation is
  never reindented), a break lands only at a child boundary next to an
  element, `xml:space` is honoured per XML 1.0 §2.10 including for a subtree
  that reads its ancestors for the inherited value, and no leading blank line
  or final newline is invented. All 26,307 comparable instance documents of
  the W3C XSD test suite round-trip (`tests/serialize_roundtrip.rs`); the
  XQTS driver's private comparison serializer is gone in favour of this one,
  with its pass count unchanged. Other encodings, CDATA sections and a
  doctype are out of scope.
- **`document::copy` — copying existing nodes into a document under
  construction, with constructor semantics.** Four additive methods on
  `BufferDocumentBuilder`: `copy_subtree` (element, text, comment, PI,
  document node, attribute — new node identities, nothing shared with the
  source), `copy_attribute`, `append_content` and `append_atomic`; plus
  `CopyOptions` (`CopyNamespaces::{Preserve, NoPreserve}`, `inherit`,
  `Annotations::{Strip, Preserve}`), `CopyError`, the `CopySource` trait
  (every `DomNavigator` is one with nothing to preserve; `BufferDocNavigator`
  reports its schema bindings) and `NamespaceFixup`. Content sequences follow
  the constructor content rules (XQuery 1.0 §3.7.1.3): consecutive atomic
  values joined with a single space, document nodes flattened, zero-length
  text dropped, an attribute after a child refused (XQTY0024) — a duplicate
  attribute name keeps the later value. A copied element carries the
  namespace declarations its names need (XQuery 1.0 §3.7.4), with generated
  `ns0`, `ns1`, … prefixes on a conflict and `xmlns=""` where an inherited
  default namespace would otherwise apply, which is the invariant that lets
  `document::serialize` refuse an unbound name. Type annotations are stripped
  by default and can be preserved within one `SchemaSet`.
- **XQuery-style composition from Rust** (`compose` feature, implies `xsd11`):
  a `compose` module for hosts that build XML from query results without a
  query processor. `Composer` owns one arena, one name table, one namespace
  context and a cache of compiled expressions keyed by expression text plus
  external variable names, so an expression inside a loop is parsed once;
  every method takes `&self`, so it works inside iterator closures. `Doc` is a
  `Copy` handle on a document in that arena — loaded with
  `load_str`/`load_reader`/`load_file` or built with `build`/`build_sequence`
  — and every navigator, `Value` and `Form` shares its lifetime, so a node of
  one document splices into another with no lifetime work. `Value` exposes a
  result as the XDM sequence it is, with `nodes`/`atomics`/`boolean`/`string`/
  `number`/`key` on top, each refusing rather than guessing.
  `Form`/`Content`/`AttrValue`/`Name` describe a result element as an owned,
  closure-free tree; the emitter resolves prefixes, computes the namespace
  declarations each element must carry, atomizes and space-joins attribute
  sequences, and splices nodes with the constructor content rules, so an
  empty sequence leaves an empty element. `compose::order` implements
  `order by` with XDM comparison, stable sorting and one key evaluation per
  row; `compose::pipe` turns the FLWOR clauses into fallible iterator
  adapters (`try_filter`, `try_map`, `try_flat_map`, `order_by`) that keep the
  `Result` and fuse after the first error. `IntoXPathValue` and `IntoContent`
  convert Rust values, navigators, documents and sequences into bindings and
  content. The module uses only the crate's public API and adds no
  dependencies.
- **`xpath!` and `form!`** (`compose` feature): the composition written as
  macros. `xpath!(c, "expr", node, name = value)` evaluates a cached
  expression with an optional context node and named bindings;
  `form! { (result :id "b1" ^{ value } ..?^{ rows }) }` describes a result
  element — attributes, namespace declarations, literal and `Display` text,
  copied content, spliced sequences, and fallible splices whose first failure
  propagates with `?`. Both are exported at the crate root
  (`use xsd_schema::{form, xpath};`). A prefixed element name is written
  `p::local`: one colon after the element name is always the attribute
  marker, so nothing is ever reinterpreted silently. Element and attribute
  names are checked when the document is built (`ComposeError::InvalidName`).
  Braces are the documented spelling of a form invocation because a form's
  tokens also parse as a Rust expression, which rustfmt would reflow; all
  three delimiters expand identically. `Composer::new` predeclares the `xs`,
  `xsi` and `fn` prefixes, as every XQuery processor does, so `xs:date(…)` in
  an expression and `:xsi:type "…"` in a form need no declaration of their
  own; `with_namespace` rebinds any of them, the last declaration winning, and
  a predeclared prefix a form does not use is not written to the output.
  The eighteen queries of the XQTS "relational" use case, rewritten in this
  style, reproduce their expected results (`tests/compose_usecase_r.rs`);
  `examples/xquery_without_xquery.rs` runs three of them end to end, and
  `doc/COMPOSE.md` is the guide.

### Changed

- Counter increments use checked arithmetic on every counted path.
- Structural refactors, all behaviour-neutral (both W3C suites byte-identical,
  no public signature changed):
  - `src/schema/derivation.rs` (10,817 lines) is now the `schema::derivation`
    module directory: `simple`, `complex`, `normalize`, `particle`,
    `wildcard`, `attributes`, `constraints`, `type_table`, `redefine`,
    `tests`, with the entry point and shared helpers in `mod.rs`. Every
    previously reachable path is re-exported unchanged.
  - The four oversized functions in `validation/runtime.rs` —
    `start_element_by_id` (700 → 136 lines), `end_of_attributes_inner`
    (305 → 110), `validate_attribute_by_id` (291 → 130) and
    `validate_attribute_against_type` (293 → 45) — are entry-point sequences
    over 25 single-responsibility helpers, with no new allocation on the
    per-element path.
  - `compile_content_model_matcher_impl` (267 → 46 lines) dispatches to
    `compile_all_group_matcher`, `compile_all_group_extension_matcher`
    (xsd11), `compile_nfa_matcher` and `attach_own_open_content`.
- **Cross-document document order is reproducible.**
  `BufferDocNavigator::compare_position` orders nodes of distinct trees by
  `BufferDocument::serial()` instead of the document's heap address, which
  XPath 2.0 §2.4.1 permits and constrains: "The relative order of nodes in
  distinct trees is stable but implementation-dependent, subject to the
  following constraint: If any node in a given tree T1 is before any node in
  a different tree T2, then all nodes in tree T1 are before all nodes in tree
  T2." `RoXmlNavigator` keeps address ordering (there is no serial to attach
  to a foreign `roxmltree::Document`), which stays stable for the duration of
  an expression.
- **`XPathEvaluator::run_with` / `run_with_node_and_setup` accept callbacks over
  caller-frame state.** The callback bound is now
  `for<'d> FnOnce(&mut TypedEvaluator<'expr, 'ctx, 'd, N>)`: only the borrow
  of the evaluator stays higher-ranked, and the expression and static-context
  lifetimes are the evaluator's own. A setup callback can therefore install a
  reference to a local (`set_extension`, `set_function_evaluator`) instead of
  `'static` data only. The bound is strictly wider than before; the one new
  requirement is `N: 'ctx` (the navigator outlives the static-context borrow),
  which every existing call site satisfies through variance without change.

### Deprecated

- `MaxOccurs::is_effectively_unbounded` is deprecated; it now equals
  `is_unbounded` because no finite bound is approximated any more.

### Removed

- **The never-constructed all-group-extension composite matcher.**
  `ContentModelMatcher::AllGroupExtension`, `CompiledContentModel::AllGroupExtension`,
  `ContentValidatorState::AllGroupExtension` and
  `validation::content::AllGroupExtPhase` modelled an all-group base followed
  by an NFA extension. No compile path ever built one:
  Structures §3.4.2.3.3 clause 4.2.3 gives an extension of an all-group base a
  `{particle}` that is the base particle itself (4.2.3.1), one merged all group
  — "a model group whose {compositor} is all and whose {particles} are the
  {particles} of the {term} of the ·base particle· followed by the {particles}
  of the {term} of the ·effective content·" (4.2.3.2) — or, in the "otherwise"
  case 4.2.3.3, a sequence containing the base's all group, which All Group
  Limited (§3.8.6.2) clause 1 forbids. The compiler already produced the merged
  all group and rejected the third case, so the composite was unreachable in
  every configuration. These are public enum variants, so this is a breaking
  change for exhaustive `match`es on them. No validation verdict, diagnostic
  or W3C conformance outcome changes. With the arm gone,
  `ContentValidatorState::try_is_complete` always returns `Ok` and
  `is_complete` never panics: the one execution limit completion could hit
  lived there.

### Fixed

- **`fn:round` takes a half towards positive infinity, not away from zero.**
  F&O §6.4.4: "Returns the number with no fractional part that is closest to the
  argument. If there are two such numbers, then the one that is closest to
  positive infinity is returned", with the example "round(-2.5) returns -2 (not
  the possible alternative, -3)". Halves of a negative argument were rounded
  away from zero, so `round(-2.5)` answered -3 and `round(-3.5)` answered -4.
  All four numeric types now round the same way, and the floating-point special
  cases of §6.4.4 are explicit: NaN, both infinities and both zeroes come back
  unchanged, and an `xs:double` or `xs:float` argument "less than zero, but
  greater than or equal to -0.5" returns negative zero (`round(-0.3)` is `-0`,
  while the `xs:decimal` `-0.3` rounds to plain `0`). The rounding is computed
  from `floor(x)` and an exact fractional part instead of `(x + 0.5).floor()`,
  which for the largest `xs:double` below a half would have answered 1.
  `fn:round-half-to-even` is a different function and is unchanged; so are all
  W3C XSD and XQTS results.
  `fn:subsequence` rounds `$startingLoc` and `$length` through the same helper:
  F&O §15.1.10 defines its result as the items whose position `p` satisfies
  `p >= fn:round($startingLoc)` and `p < fn:round($startingLoc) +
  fn:round($length)`, and it kept a second, half-away-from-zero copy of the
  rounding (whose comment claimed `fn:round-half-to-even`), so
  `subsequence((1,2,3,4,5), -1.5, 4.5)` started at -2 and yielded `(1, 2)`
  instead of the specified `(1, 2, 3)`. Integral positions, NaN and the
  infinities are unaffected.
- **`fn:codepoints-to-string` rejects a non-integer numeric argument.** Its
  declared parameter type is `xs:integer*`, and the function conversion rules of
  XPath 2.0 §3.1.5 cast only an `xs:untypedAtomic` item to it: numeric promotion
  goes the other way, promoting `xs:decimal` and `xs:float` *to* `xs:double`
  (§B.1), never a numeric item down to `xs:integer`. A whole-valued
  `xs:decimal`, `xs:float` or `xs:double` therefore reaches the closing rule —
  "If, after the above conversions, the resulting value does not match the
  expected type according to the rules for SequenceType Matching, a type error
  is raised [err:XPTY0004]" — where it used to be accepted as a codepoint. So
  `codepoints-to-string(65.0)` and `codepoints-to-string(xs:double(65))` are now
  `XPTY0004`, while `xs:integer`, every type derived from it and an untyped node
  still work.
- **Built-in functions apply the function conversion rules to `xs:untypedAtomic`
  arguments.** XPath 2.0 §3.1.5: "Each item in the atomic sequence that is of
  type xs:untypedAtomic is cast to the expected atomic type. For built-in
  functions where the expected type is specified as numeric, arguments of type
  xs:untypedAtomic are cast to xs:double." Functions whose parameter is a
  specific atomic type checked the exact variant instead, so
  `month-from-date(end_date)` over a document with no schema raised `XPTY0004`
  where a general comparison on the same node cast and compared. The whole
  `*-from-date` / `*-from-dateTime` / `*-from-time` / `*-from-duration` /
  `timezone-from-*` / `adjust-*-to-timezone` family, `fn:dateTime`, the
  `xs:integer` parameters of `fn:remove`, `fn:insert-before` and
  `fn:round-half-to-even`, `fn:codepoints-to-string` (which also now atomizes
  its argument) and the `numeric` parameters of `fn:abs`, `fn:ceiling`,
  `fn:floor` and `fn:round` now cast through one shared helper
  (`functions::convert`). An untyped value whose lexical form is invalid for
  the expected type is the dynamic error `FORG0001`; a type that
  `xs:untypedAtomic` cannot be cast to at all still raises `XPTY0004`, so
  `fn:prefix-from-QName` keeps rejecting an untyped node (casting to
  `xs:QName` requires a string literal, §2.3.4). No central signature-driven
  conversion was introduced. W3C XSD and XQTS results are unchanged.
- **A schema element's reported location is now its `<`, not the end of the
  markup before it.** `SourceRef.span.start` was taken from quick-xml's
  `buffer_position()` before the read, which is where the *previous* event
  ended — the same read also consumes the whitespace in front of the tag. A
  declaration was therefore reported on the line above itself and at the
  column just past the preceding `>` (`examples/books.xsd`'s `BookForm` came
  out as 14:21 instead of 16:3). The markup start is now recovered from the
  tag's own length in `parser::reader::TrackedReader::read_event`, so every
  span, every error location and every inspector row points at the `<`. Spans
  stay stable per element, so `compiler::upa`'s `same_particle_origin`, which
  compares `(doc_id, span)`, is unaffected.
- **Epsilon states now carry a source location.** About half the rows of the
  inspector's state table read `(no origin)`: branch and merge states are
  invented by composition and `FragmentBuilder` left them without one. The new
  `compiler::NfaFragment::fill_missing_origin` gives every origin-less epsilon
  state the location of the construct that created it — applied after each
  model group's composition and after each particle's occurrence wrapper, so
  the innermost known construct wins. Term-bearing states are never touched,
  keeping `same_particle_origin` exact. Diagnostics-only; no verdict moves.
- **UPA compilation no longer compiles the base type uncapped.**
  `compile_base_all_group` called the public, non-UPA
  `compile_content_model_matcher`, so when an XSD 1.1 extension type was
  compiled for schema-time UPA checking (`compile_content_model_for_upa`) the
  base type's content model was built with exact occurrence bounds while the
  rest of the same compilation was capped by `cap_for_upa`. The mode is now
  threaded through, so both halves are capped (Sperberg-McQueen 2005: for
  determinism testing `F{n,m}` can be replaced by `F{min(n,1), min(m,2)}`) and
  the counted construction is not run for a model the UPA check discards. No
  UPA verdict changes: an all-group model carries its member bounds in either
  mode and `check_all_group_upa` does not read them.
- **`xsi:nil` on a non-nillable element is now invalid at the start event.**
  The pushed element state was `Invalid` and `cvc-elt.3.1` was reported, but
  the `SchemaInfo` returned by `validate_element` / `validate_element_by_id`
  for the *start* event still said `Valid`, so a streaming consumer reading
  per-element `[validity]` saw the violation only at end-of-element. Element
  Locally Valid (Element) (§3.3.4.2) clause 3.1 — "D . {nillable} = false, and
  E has no xsi:nil attribute" — is a verdict on the element itself, so both now
  report `Invalid`. Diagnostics, their order and every driver outcome are
  unchanged.
- **An unresolved `<xs:group ref="…"/>` now names the group.** The error read
  `unresolved group reference: NameId(42):17` — the raw interned ids — instead
  of the QName. Both group-reference resolution sites (`compile_group_ref` and
  the XSD 1.1 `flatten_all_group_ref_into`) now format the name with
  `schema::resolver::format_resolved_qname`, the helper every sibling error
  site already uses, giving `unresolved group reference:
  {http://example.com/tns}missing`.
- **Finite `maxOccurs` above 10 000 is now enforced exactly.** The compiler
  treated any finite maximum larger than `MAX_COUNTED_OCCURS = 10_000` as
  `unbounded`, so `a{0,10001}` accepted 10 002 children. Structures §3.9.4.3
  clause 2.2 requires the sequence length to be "less than or equal to the
  {max occurs}" whenever it is a number. Every finite bound now compiles to an
  exact counted loop up to the representable maximum `u32::MAX`. Occurrence
  literals beyond `u32` (schema-valid — `nonNegativeInteger` has no bound)
  saturate to `u32::MAX` and stay finite; this is documented on
  `parse_occurs` and only observable past 4 294 967 295 sibling occurrences.
- **A child rejected by the content model now makes its parent invalid.**
  `cvc-complex-type.2.4` was reported to the sink, but the parent's PSVI
  `[validity]` — and therefore `DriveOutcome::root_validity` — stayed
  `Valid`. Clause 2.4 of Element Locally Valid (Complex Type) (§3.4.4) is a
  constraint on the parent, so it is now `Invalid`. Sink diagnostics are
  unchanged; only the validity field moves.
- **A content model that cannot be prepared is no longer treated as empty
  content.** `SchemaValidator::new` silently dropped any complex type whose
  content model failed to compile, and the first element it governed was then
  validated as if the type were empty. The failure is now recorded
  (`SchemaValidator::content_model_failures`) and raised as an operational
  failure (`validation-preparation-failed`) when such a type is first used.
- **Node identity now includes the document.**
  `BufferDocNavigator::is_same_position` compared `current`, `virtual_parent`,
  `current_ns` and `attr_index` — each an index into *one* `BufferDocument` —
  but not the document itself, and `RoXmlNavigator::is_same_position` compared
  roxmltree `NodeId`s, which are per `Document`. Two navigators on the same
  node index of two different documents therefore compared equal. XPath 2.0
  §3.5.3: "A comparison with the `is` operator is true if the two operand
  nodes have the same identity, and are thus the same node; otherwise it is
  `false`." `is`, `<<` and `>>` were unaffected (they go through
  `compare_position`, which already compared the document); the reachable
  consequence was in `union`, `intersect` and `except`, which "eliminate
  duplicate nodes from their result sequences based on node identity"
  (§3.3.3) — `$a/root/x | $b/root/x` over two same-shaped documents yielded 2
  nodes instead of 4 — and in the adjacent-duplicate check of
  `DocumentOrderNodeIterator` at a tree boundary. Both navigators' `move_to`
  also copied the cursor but not the document, so moving onto a node of
  another tree landed on this tree's node of the same index; `RoXmlNavigator`
  now carries the document's base URI over as well. New integration test
  `tests/multi_document.rs` pins identity, `is`, the three set operators and
  block order across two documents for both navigators.
- **Processing-instruction data is kept verbatim.** `PI ::= '<?' PITarget (S
  (Char* - (Char* '?>')))? '?>'` (XML 1.0 §2.6) makes only the `S` between
  target and data a separator, but both quick-xml adapters — the
  `BufferDocument` builder's and the streaming validator's — trimmed the raw
  content first, so a PI ending in whitespace (Microsoft InfoPath writes
  `<?mso-application progid="…" ?>`) reached the tree, and any host hook, a
  character short.
- **A processing instruction's string value through `RoXmlNavigator` is its
  data.** `value()` / `value_ref()` asked roxmltree's `Node::text()`, which
  answers only for text and comments — a PI's data lives in `Node::pi()` — so
  they returned an empty string where `BufferDocNavigator` returned the data,
  and XDM asks for the data.

### Performance

- **Precomputed successor closures for counter-free content models**
  (`NfaTable::successor_closure`, `StateSet::union_with`). Each NFA state's
  epsilon closure of its consuming successors is computed once per table
  (lazily, shared through the `Arc`), so one child step is the OR of a few
  4-word bitsets over the matching frontier states instead of a depth-first
  epsilon search per child, exact because the epsilon closure distributes
  over unions. Paired A/B on a 47 MiB synthetic catalog against the preceding
  revision: validate-only 67.4 → 72.4 MiB/s (roxmltree),
  64.1 → 68.6 (BufferDoc), streaming 47.2 → 50.0, streaming without PSVI
  52.4 → 55.5; libxml2 control flat. A differential test compares the step
  with the previous algorithm on every string up to length 5–7 over unrolled,
  nullable epsilon-cycle, wildcard-priority and substitution-group models in
  both XSD versions. Counted models are unchanged. `NfaTable` gained a private
  cache field: it can no longer be built with a struct literal (use `new` /
  `with_counters`), and code that mutates `states` directly after a table has
  been executed must go through `get_state_mut`, which invalidates the cache.
- Two per-element allocation gates in the validation runtime: the element
  path and location are no longer cloned at every element end, and the
  attribute typed value is no longer cloned at every attribute, unless an
  identity constraint is actually active. Both W3C suites are unchanged.

## [0.1.5] - 2026-08-31

Composition and complex-type restriction fixes, prompted by the official GAEB
DA XML 3.3 schema corpus (<https://www.gaeb.de>) — 32 schemas built on chained,
chameleon `xs:redefine`. All 32 now load with every check enabled when the
schema set is XSD 1.1 — no opt-out, and none was added. Under XSD 1.0 nine
still fail, all on the "intensional restriction" class that the W3C suite
itself accepts only for 1.1. Both W3C suites are unchanged by this work (XSD 1.0 39458/39510, XSD 1.1
2313/2319, byte-identical failure sets).

### Fixed

- `xs:redefine` now resolves the component it redefines through the *effective
  view* of the redefined document — its transitive `include` and `redefine`
  edges — instead of only that document's own component index. A schema whose
  original is declared one hop below the redefine target failed to load with
  `src-redefine: Original ... not found`, which rejected valid chained and
  chameleon redefine graphs (six files of the GAEB corpus).
- Eight defects in `derivation-ok-restriction` (§3.4.6.3 / §3.9.6), each of
  which rejected valid restrictions or accepted invalid ones:
  - `Choice:Choice` (RecurseLax) folded the parent choice's occurrence range
    into every branch. An optional choice therefore made each derived branch
    optional, so it could no longer map onto a base branch whose first child is
    required — and, in the other direction, an optional choice was accepted as a
    restriction of a required one whenever every branch happened to be optional.
    The two choices' own ranges are now compared directly and the mapping runs
    over their raw `{particles}`.
  - Particles whose term is an empty model group were not removed as pointless,
    so `<xs:choice minOccurs="0"/>` still had to map onto something in the base.
    A required empty `choice` is still kept: it accepts no sequence at all.
  - Attribute uses with `use="prohibited"` were run through the attribute-type,
    `fixed`-value and (XSD 1.1) `{inheritable}` checks. Per §3.4.2.4 such an
    `<attribute>` corresponds to no component — it only suppresses the base's
    use — so its declared type no longer takes part in the derivation.
    Prohibiting a *required* base attribute remains an error.
  - Restricting a type derived by extension was checked against the extension's
    own particle alone. §3.4.2.3 makes the content type
    `sequence(inherited-particle, own-particle)`; without the inherited half,
    every element the base contributed looked like one the restriction invented.
  - The base side of a restriction reported only the attribute uses the type
    declares itself, never the ones it inherits, with the same consequence for
    inherited attributes.
  - Under XSD 1.1, a single-child group folded away by particle normalization
    (`<C minOccurs="m" maxOccurs="n">X</C>` → `X{m,n}`) is now also offered to
    the base in its original shape. XSD 1.1 restriction is language subsumption
    (§3.4.6.4), so the folded and unfolded spellings must be treated alike.
    XSD 1.0 is deliberately unchanged here: there the fold is what makes a
    single-branch `<choice>` restricting a multi-branch base choice invalid
    (W3C `msData` `groupH021v`, `particlesZ024`, both marked invalid for 1.0 and
    valid for 1.1).

- Three `cargo doc` intra-doc-link warnings: the Datatypes 1.1 production
  numbers in `is_valid_xsd_decimal_lexical` were read as item links, and
  `SubstitutionGroupMap` linked to a `pub(crate)` item absent from the
  rendered docs. `cargo doc --no-deps` is now warning-free with and without
  `--features xsd11`.

### Changed

- Replaced the two uses of `usize::is_multiple_of`, stabilized in Rust 1.87,
  with the equivalent modulo, so the crate builds on older toolchains. Verified
  against rustc 1.85.0 with `--all-features`. No `rust-version` is declared: the
  crate does not commit to a minimum supported version.

## [0.1.4] - 2026-08-22

Security release. Upgrades `quick-xml` past two denial-of-service advisories
and adopts the XML attribute-value and line-end normalization that the newer
parser performs. No public API change. W3C XSD 1.0 suite failures 19 → 18;
XSD 1.1 suite and the XQTS XPath suite are unchanged, as is instance-validation
throughput (within run-to-run measurement noise).

### Security

- Upgrade `quick-xml` from 0.31 to 0.41, which fixes two denial-of-service
  advisories that affected every parse of untrusted XML through this crate
  (the schema parser, the streaming validation driver and `BufferDocument`
  all read through `quick-xml`):
  - [RUSTSEC-2026-0194] — quadratic run time when checking a start tag for
    duplicate attribute names.
  - [RUSTSEC-2026-0195] — unbounded namespace-declaration allocation in
    `NsReader`.

### Fixed

- Attribute values are now normalized as XML 1.0 §3.3.3 requires before they
  reach the validator: a literal tab, carriage return or line feed inside an
  attribute value becomes a space. Previously the raw character was validated,
  so patterns and facets saw content no conforming XML processor would produce
  (W3C XSD 1.0 suite: `RegexTest_63.i`; suite failures 19 → 18).

### Changed

- Character data containing general references (`&amp;`, `&#65;`) is reported
  by `quick-xml` 0.38+ as separate `Text` and `GeneralRef` events. The schema
  parser, the streaming driver and the `BufferDocument` builder rejoin the run
  before validating it, so a reference no longer splits one text run into
  several validator events. Only the five predefined entities and character
  references are resolved; any other entity is a parse error, as before.
- Line endings in character data are normalized (`\r\n` and `\r` → `\n`)
  per XML 1.0 §2.11, which `quick-xml` 0.31 did not do for text events.

## [0.1.3] - 2026-07-21

### Fixed

- Defer the `cvc-type.2` abstract-type check for XSD 1.1 element
  declarations carrying `xs:alternative` (conditional type assignment).
  The governing type is not known until after attribute processing, so
  the check now runs against the final CTA-selected type in
  `end_of_attributes_inner` rather than the declared type at element
  start. This removes false-positive errors on elements whose *declared*
  type is abstract but whose CTA always resolves to a concrete
  alternative (e.g. OpenDRIVE 1.8 `<junction>`). A genuinely abstract
  governing type still errors.

## [0.1.2] - 2026-07-04

Conformance sweep: W3C XSD 1.0 suite failures reduced 47 → 19 (99.95%);
every remaining failure in both suites is a documented W3C dispute or an
intra-suite contradiction. Per-element allocation cleanup on the validation
hot path (+6–8% pure-validation throughput on the synthetic corpus; both
W3C suites byte-identical). API-additive only.

### Performance

- Build the element path without a per-element `String` (zero-alloc
  interned-name lookup in `push_element`).
- Skip XSD 1.1 inherited-attribute propagation and default recording
  entirely when the schema declares no `inheritable` attribute (every
  XSD 1.0 schema, most XSD 1.1 schemas).
- Prune trivial self-match entries from the substitution-group map and
  drop the map altogether for substitution-free, abstract-free schemas,
  so content-model term matching does no hash probes per child in the
  common case. Abstract-head entries are kept — their self-name omission
  is what blocks abstract elements from matching in instances.
- Precompute each content model's initial NFA state set once per compiled
  complex type instead of re-running the epsilon closure per element.
- Pool `ElementValidationState` shells across elements, retaining
  collection capacity (`text_content`, `seen_attributes`, …); a
  drift-guard test keeps `reset()` equivalent to a fresh state.
- Key the per-type content-model map and the outer substitution-group map
  with `ahash` instead of SipHash (interned keys, hot-path probes).

### Added

- Value-free push-API twins for throughput drivers that discard
  per-element PSVI: `validate_element_novalue`,
  `validate_element_by_id_novalue`, `validate_end_of_attributes_novalue`,
  and `validate_end_element_novalue` (returns `SchemaValidity`). The DOM
  driver uses all of them; the existing value-returning methods are
  unchanged.

### Fixed

- Identity constraints: duplicate names across schema documents of one
  namespace are now a compile error (§3.11 symbol space); NaN compares
  identical to NaN in key/unique fields (W3C bug 9196); a field matching an
  element with an attribute-only (empty) complex type violates
  cvc-identity-constraint clause 3.
- NOTATION: enumeration values must resolve to declared notations
  (Datatypes §3.3.20); `public` is optional when `system` is present under
  XSD 1.0 (errata).
- Facets: `length` may coexist with `minLength`/`maxLength` when inherited
  per Datatypes §4.3.1.4 (W3C bug 6446); facet elements are rejected inside
  complexContent restrictions; `anyAttribute`/attributes/particles are
  rejected inside simpleType restrictions.
- Derivation: user restrictions of `xs:anySimpleType` are rejected
  (cos-st-restricts.1.1); simpleContent restriction of a mixed base
  requires an inline `<simpleType>` (src-ct.2.2); restriction-declared
  attributes must be admitted by the base's attribute wildcard
  (derivation-ok-restriction.2); constraining facets on anySimpleType
  content are rejected; Element Declarations Consistent is enforced across
  extension merges.
- Substitution groups: the head type's `{prohibited substitutions}`
  (`complexType/@block`) now participates in Substitution Group OK
  (Transitive) clause 2.3.
- Wildcards: XSD 1.0 attribute-wildcard unions that are not expressible
  (§3.10.6) are rejected at compile time; XSD 1.1 unaffected.
- Content models: an empty `<xs:choice/>` with `minOccurs ≥ 1` is
  unsatisfiable instead of matching empty content.
- QNames: prefixed QName attribute values with undeclared prefixes are
  rejected (src-qname); dangling element `ref`s are rejected in
  non-chameleon documents (src-resolve).
- anyURI (XSD 1.0): enumeration facet values are checked against RFC 2396
  lexical rules (malformed scheme, incomplete `%`-escape, `\`, `^`).
- Schema loading: file locations are canonicalized, so case variants on
  case-insensitive filesystems and symlinked paths identify one schema
  document.

## [0.1.1] - 2026-06-27

Performance-focused release. No breaking changes to the public API.

### Performance

- Compile content models once at schema load time and share them across
  validations via `Arc`, instead of recompiling per element.
- Avoid cloning `ActiveStates` on the content-model hot path.
- Represent NFA states as a bitset with fused epsilon-closure computation, and
  use keyed `ahash` for name interning.
- Materialize PSVI typed values lazily / opt-out, avoiding allocation when the
  typed value is not consumed.
- Add an allocation-free `i128` fast path for numeric value parsing.

### Fixed

- Gate arena mutations so that mutating an existing entry invalidates the
  effective-facets cache (prevents stale derived facets).
- Resolve all `rustdoc` warnings.

### Changed

- Decompose `validate_end_element` into smaller units for maintainability
  (internal refactor; no behavioral change).

## [0.1.0] - 2026-06-09

Initial release: XML Schema (XSD 1.0/1.1) validator with PSVI and a built-in
XPath 2.0 engine.

[Unreleased]: https://github.com/semyonc/xsd-schema/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/semyonc/xsd-schema/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/semyonc/xsd-schema/compare/v0.1.5...v0.2.0
[0.1.5]: https://github.com/semyonc/xsd-schema/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/semyonc/xsd-schema/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/semyonc/xsd-schema/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/semyonc/xsd-schema/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/semyonc/xsd-schema/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/semyonc/xsd-schema/releases/tag/v0.1.0
[RUSTSEC-2026-0194]: https://rustsec.org/advisories/RUSTSEC-2026-0194
[RUSTSEC-2026-0195]: https://rustsec.org/advisories/RUSTSEC-2026-0195
