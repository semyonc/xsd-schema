# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Phases P0 and P1 of `XSD_COMPILER_REWORK.md` (branch
`perf/compiler-rework-p0-p1`): the exact-occurrence correction with its
resource-failure contract, plus the two measurement-phase allocation gates.

Also the additive `xsd-schema` prerequisites of the XSLT 2.0 crate
(`XSLT2_DESIGN_SKETCH.md` §5, the rows labelled 0.1.6 there — they ship in
this release): multi-document node identity, `DomNavigator::type_annotation`,
`XPathExpr` dependency metadata, the `DynamicContext` extension slot and
`set_function_evaluator`, an owned default function namespace, and
`BufferDocument::serial()`.

### Removed

- **The never-constructed all-group-extension composite matcher.**
  `ContentModelMatcher::AllGroupExtension`, `CompiledContentModel::AllGroupExtension`,
  `ContentValidatorState::AllGroupExtension`, `validation::content::AllGroupExtPhase`
  and `compiler::inspect::CompiledView::AllGroupExtension` modelled an
  all-group base followed by an NFA extension. No compile path ever built one:
  Structures §3.4.2.3.3 clause 4.2.3 gives an extension of an all-group base a
  `{particle}` that is the base particle itself (4.2.3.1), one merged all group
  — "a model group whose {compositor} is all and whose {particles} are the
  {particles} of the {term} of the ·base particle· followed by the {particles}
  of the {term} of the ·effective content·" (4.2.3.2) — or, in the "otherwise"
  case 4.2.3.3, a sequence containing the base's all group, which All Group
  Limited (§3.8.6.2) clause 1 forbids. The compiler already produced the merged
  all group and rejected the third case, so the composite was unreachable in
  every configuration. These are public enum variants, so this is a breaking
  change for exhaustive `match`es on them (the unreleased set already carries
  the `NfaTable` change). No validation verdict, diagnostic or W3C conformance
  outcome changes. `ContentValidatorState::try_is_complete` now always returns
  `Ok`, and `is_complete` no longer panics — the one execution limit it could
  hit lived in the removed arm.

### Fixed

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
  `&'static str` to leak per stylesheet. `default_function_namespace()`
  prefers it over the public `default_function_ns` field; XPath 1.0 mode still
  yields `""`.

### Changed

- `MaxOccurs::is_effectively_unbounded` is deprecated; it now equals
  `is_unbounded` because no finite bound is approximated any more.
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

### Performance

- **Precomputed successor closures for counter-free content models**
  (`NfaTable::successor_closure`, `StateSet::union_with`). Each NFA state's
  epsilon closure of its consuming successors is computed once per table
  (lazily, shared through the `Arc`), so one child step is the OR of a few
  4-word bitsets over the matching frontier states instead of a depth-first
  epsilon search per child — the "precomputed epsilon-closed NFA successors"
  option of `XSD_COMPILER_REWORK.md` §6.7, exact by distributivity of the
  closure over unions. Paired A/B on the 47 MiB catalog against the
  integrated refactor branch: validate-only 67.4 → 72.4 MiB/s (roxmltree),
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
  identity constraint is actually active (`XSD_COMPILER_REWORK.md` §12.1).
  Both W3C suites are unchanged.

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

[0.1.5]: https://github.com/semyonc/xsd-schema/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/semyonc/xsd-schema/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/semyonc/xsd-schema/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/semyonc/xsd-schema/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/semyonc/xsd-schema/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/semyonc/xsd-schema/releases/tag/v0.1.0
[RUSTSEC-2026-0194]: https://rustsec.org/advisories/RUSTSEC-2026-0194
[RUSTSEC-2026-0195]: https://rustsec.org/advisories/RUSTSEC-2026-0195
