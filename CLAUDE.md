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
```bash
cargo +nightly miri test --lib namespace::table   # NameTable unsafe verification
cargo +nightly miri test --lib                     # Full lib under Miri
cargo +nightly miri test --lib --features xsd11    # Full lib + XSD 1.1 under Miri
```
