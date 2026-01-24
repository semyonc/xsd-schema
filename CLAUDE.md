# CLAUDE.md - xsd-schema

XSD 1.0/1.1 schema validator with XPath 2.0 implementation in Rust.

## Build & Test

```bash
cargo build              # Build library
cargo test               # Run unit tests
cargo test conformance   # Run W3C conformance tests (tests/conformance/)
```

## Architecture

- **Arenas**: All schema components stored via slotmap with typed IDs (`ids.rs`, `arenas.rs`)
- **Two-phase loading**: `parse_schema_only()` then `process_loaded_schemas()` for multi-schema support
- **Pipeline**: `load_and_process_schema()` for single schema processing

## Key Modules

| Module | Purpose |
|--------|---------|
| `parser/` | Streaming XML parser with location tracking |
| `schema/` | Schema component model (elements, types, groups) |
| `types/` | Built-in types, facets, validators |
| `compiler/` | NFA compiler for content models |
| `xpath/` | XPath 2.0 parser, evaluator, and navigation |
| `validation/` | Instance document validation |

## Code Conventions

- Use `quick-xml` for streaming, `roxmltree` for test suite parsing
- Errors via `thiserror` with `SchemaError`/`SchemaResult` types
- LALRPOP for XPath grammar (`src/xpath/*.lalrpop`)
