# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
