# xsd-schema

`xsd-schema` is a Rust XML Schema validator for XSD 1.0 and 1.1. Its push-based API integrates with event-based parsers such as `quick-xml` and DOM-style sources such as `roxmltree`, and a built-in XPath 2.0 engine adapts to any DOM through the `DomNavigator` trait.

## Documentation

| Document | Description |
| --- | --- |
| [Introduction](doc/INTRODUCTION.md) | Public API overview, feature sets, schema loading, validation flow, XPath entry points, and async loading notes. |
| [Extensibility Guide](doc/EXTENSIBILITY.md) | Extension points for annotations/appinfo, schema loaders, DOM navigation, and custom XPath functions. |
| [Unsafe Code](doc/UNSAFE.md) | Inventory of unsafe blocks, safety invariants, and Miri verification commands. |

## Test Results

| Suite | Command | Total | Passed | Failed | Skipped | Errors | Pass rate |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| W3C XSD 1.0 conformance | `cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --version 1.0` | 39,510 | 39,408 | 61 | 34 | 7 | 99.8% |
| W3C XSD 1.1 conformance | `cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --version 1.1` | 2,319 | 2,312 | 7 | 0 | 0 | 99.7% |
| XQTS XPath 2.0 | `cargo test --test xqts_xpath --features xsd11 -- -s /Users/semyonc/Projects/XmlPad-Windows/XQTS_1_0_2 --all -v -f` | 8,047 | 8,047 | 0 | 0 | 0 | 100.0% |

# AI Disclosure

This project was generated with AI as an experiment.
The generated code and content were reviewed and refined by the author.
Use of this repository is governed by its license, including any production use.
This notice is provided for transparency.
