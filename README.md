# xsd-schema

`xsd-schema` is a Rust XML Schema validator for XSD 1.0 and 1.1 with a full post-schema-validation infoset (PSVI). Its push-based API integrates with event-based parsers such as `quick-xml` and DOM-style sources such as `roxmltree`, and a built-in XPath 2.0 engine adapts to any DOM through the `DomNavigator` trait.

## Documentation

| Document | Description |
| --- | --- |
| [Introduction](doc/INTRODUCTION.md) | Public API overview, feature sets, schema loading, validation flow, XPath entry points, and async loading notes. |
| [Architecture Overview](doc/OVERVIEW.md) | Crate structure, pipeline diagram, module map, key abstractions, milestone history, and build reference. |
| [Extensibility Guide](doc/EXTENSIBILITY.md) | Extension points for annotations/appinfo, schema loaders, DOM navigation, and custom XPath functions. |
| [Unsafe Code](doc/UNSAFE.md) | Inventory of unsafe blocks, safety invariants, and Miri verification commands. |

## Test Results

| Suite | Command | Total | Passed | Failed | Skipped | Errors | Pass rate |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| W3C XSD 1.0  | `cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --version 1.0` | 39,510 | 39,427 | 49 | 34 | 0 | 99.9% |
| W3C XSD 1.1  | `cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --version 1.1` | 2,319 | 2,313 | 6 | 0 | 0 | 99.7% |
| XQTS XPath 2.0 | `cargo test --test xqts_xpath --features xsd11 -- -s ../../XQTS_1_0_2 --all -v -f` | 8,047 | 8,047 | 0 | 0 | 0 | 100.0% |


# AI Disclosure

This project was generated with AI as an experiment.
The generated code and content were reviewed and refined by the author.
Use of this repository is governed by its license, including any production use.
This notice is provided for transparency.

## Source Provenance

No third-party source repositories were used as rewrite sources for this codebase, with the sole exception of the author's own prior work:

- [semyonc/xpath2](https://github.com/semyonc/xpath2) — C# XPath 2.0 implementation
- WmHelp XmlPad — earlier Delphi tool by the same author together with Edward Aponasko and Alex Pospelov

A small number of Microsoft .NET API shapes are mirrored where they map naturally onto the data model, but no Microsoft source code was ported or rewritten. 