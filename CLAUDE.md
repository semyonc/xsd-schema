# CLAUDE.md - xsd-schema build instructions

The standard: https://www.w3.org/TR/xmlschema11-1/

## Build Workflow

After making code changes, always run clippy and tests for **both** feature configurations:

### Default (XSD 1.0 only):
```bash
cargo clippy --all-targets                    # Lint check (0 warnings expected)
cargo test --all-targets                      # Run XSD 1.0 tests + conformance
```

### Full (XSD 1.1 + XPath):
```bash
cargo clippy --all-targets --features xsd11   # Lint check (0 warnings expected)
cargo test --all-targets --features xsd11     # Run ALL tests
```

### Miri (unsafe code verification):
The `unsafe` blocks are verified by targeted runs under an isolated Miri (no
`-Zmiri-disable-isolation`); `doc/UNSAFE.md` maps each run to its block:
```bash
cargo +nightly miri test --lib namespace::table::tests
cargo +nightly miri test --lib --features xsd11 validation::runtime::tests::assertion_runtime_tests
cargo +nightly miri test --lib types::tests::test_xml_type_code_roundtrip
cargo +nightly miri test --lib xml::tests::bytes_that_are_not_utf8_are_refused_as_the_checks_refuse_them
cargo +nightly miri test --lib xml::tests::                            # also with MIRIFLAGS=-Zmiri-tree-borrows
cargo +nightly miri test --lib --features xsd11 xpath::context::tests::current_date_time_from_a_fixed_clock
```
The whole library suite (`cargo +nightly miri test --lib`) has tests that create files and resolve schema
locations against the working directory, so it does not run under an isolated Miri; it is not the
verification of record for the `unsafe` blocks.

### Doctests (the guides in `doc/` are doctested):
```bash
cargo test --doc                              # default features
cargo test --doc --features xsd11
cargo test --doc --all-features               # also COMPOSE.md and the async examples
```
`cargo test --all-targets` does not run doctests. A guide example that needs a
feature is guarded with `#[cfg(feature = "...")]` (see `doc_guides` in `src/lib.rs`).

### W3C conformance (always `--release`, always `--features xsd11`):
```bash
cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --version 1.0
cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --version 1.1
# The tests whose manifest names no XSD version (run as 1.0 above), under SchemaSet::xsd11():
cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --versionless-1.1
```
Compare runs by the FAIL and ERROR lines and the passed count.
