# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
