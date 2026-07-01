# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Conformance sweep: W3C XSD 1.0 suite failures reduced 47 → 19 (99.95%);
every remaining failure in both suites is a documented W3C dispute or an
intra-suite contradiction. No public API changes.

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

[0.1.1]: https://github.com/semyonc/xsd-schema/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/semyonc/xsd-schema/releases/tag/v0.1.0
