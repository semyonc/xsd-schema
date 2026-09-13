# xsd-schema

`xsd-schema` is a Rust XML Schema validator for XSD 1.0 and 1.1 with a full post-schema-validation infoset (PSVI). Its push-based API integrates with event-based parsers such as `quick-xml` and DOM-style sources such as `roxmltree`, and a built-in XPath 2.0 engine adapts to any DOM through the `DomNavigator` trait.

## Documentation

| Document | Description |
| --- | --- |
| [Introduction](doc/INTRODUCTION.md) | Public API overview, feature sets, schema loading, validation flow, XPath entry points, and async loading notes. |
| [Architecture Overview](doc/OVERVIEW.md) | Crate structure, pipeline diagram, module map, key abstractions, milestone history, and build reference. |
| [Extensibility Guide](doc/EXTENSIBILITY.md) | Extension points for annotations/appinfo, schema loaders, DOM navigation, and custom XPath functions. |
| [Unsafe Code](doc/UNSAFE.md) | Inventory of unsafe blocks, safety invariants, and Miri verification commands. |
| [Composing XML from Rust](doc/COMPOSE.md) | XPath 2.0 with Rust values bound as `$variables`, the Lisp-style `form!` constructor, iterator pipelines for the FLWOR clauses, and serialization. |
| [Changelog](CHANGELOG.md) | Release notes for every version, with upgrade notes for breaking releases. |

To see exactly what the validator compiled for a complex type — the authored
particle tree, the automaton it became, counters, and where each state comes
from in the schema — use the content-model inspector:
[Inspecting a compiled content model](doc/INTRODUCTION.md#7-inspecting-a-compiled-content-model).

To build XML from query results in Rust — XPath 2.0 with Rust values bound as
`$variables`, iterator pipelines for the FLWOR clauses, and a Lisp-style
constructor — use the composition layer:
[Composing XML from Rust](doc/COMPOSE.md).

## Test Results

| Suite | Command | Total | Passed | Failed | Skipped | Errors | Pass rate |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| W3C XSD 1.0  | `cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --version 1.0` | 39,510 | 39,458 | 18 | 34 | 0 | 99.95% |
| W3C XSD 1.1  | `cargo test --test conformance --features xsd11 --release -- --test-suite ../../xsdtests --version 1.1` | 2,319 | 2,313 | 6 | 0 | 0 | 99.7% |
| XQTS XPath 2.0 | `cargo test --test xqts_xpath --features xsd11 -- -s ../../XQTS_1_0_2 --all -v -f` | 8,047 | 8,047 | 0 | 0 | 0 | 100.0% |

All 18 remaining XSD 1.0 failures and all 6 XSD 1.1 failures are documented
disputes: W3C-queried tests (Bugzilla 4146/4680/4957/6901/29085), tests that
contradict other accepted tests in the same suite (the `elemM002` /
`xsd015.e` family vs. Saxon's `Missing` group), or documented waivers.

## Benchmark

Instance-validation throughput and memory for `xsd-schema`'s three ingestion
strategies, over a synthetic dataset ≈ 15.56 MB — validated against its XSD. Each
strategy runs in its own subprocess (so RSS deltas are clean), timing is averaged
over 10 iterations, and the schema is compiled **once, off the clock**.

- **streaming** — push-based, no DOM (`drive_quick_xml` + `SchemaValidator`)
- **DOM (roxmltree)** — third-party tree via the `DomNavigator` trait
- **DOM (BufferDoc)** — built-in compact 16-byte-node document

| Strategy | Parser | Time | Throughput | RSS delta |
| --- | --- | ---: | ---: | ---: | 
| streaming | quick-xml | 308 ms | 50.6 MB/s | **568 KB** |
| DOM (roxmltree) | roxmltree | 265 ms | 58.8 MB/s | 88.4 MB |
| DOM (BufferDoc) | quick-xml | 339 ms | 45.9 MB/s | 61.4 MB |

Re-measured 2026-09-11 after the exact-occurrence, allocation-gate and
precomputed-closure work (four runs per strategy, spread under 3 %); the
previous table read 353 / 303 / 377 ms. Both W3C suites are unchanged by that
work (failure lists byte-identical to v0.1.5).



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