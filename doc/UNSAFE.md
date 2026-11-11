# Unsafe Code

The library contains five `unsafe` blocks, in three places; the tests one more.
This list is complete: `grep -rnw unsafe src` finds no other.

| # | Where | What |
| --- | --- | --- |
| 1 | `src/namespace/table.rs`, `NameTable::resolve_ref` and `try_resolve_ref` | `&str` into interned names without a `RefCell` guard |
| 2 | `src/validation/runtime.rs`, `begin_assertion_buffering` | the self-referential fragment builder of assertion buffering |
| 3 | `src/xml/reader.rs`, `WfReader::read_event_into` | the read buffer reached again after quick-xml refused bytes that are not UTF-8 |
| — | `src/types/mod.rs`, test `test_xml_type_code_roundtrip` | `transmute` of `u8` to `XmlTypeCode` (`#[cfg(test)]` only) |

## 1. NameTable::resolve_ref / try_resolve_ref

**File**: `src/namespace/table.rs`

`resolve_ref(&self, id: NameId) -> &str` (and its checked twin
`try_resolve_ref`) bypasses `RefCell`'s borrow guard via `RefCell::as_ptr()` to
return `&str` into interned string data without allocation. Required because
`DomNavigator` trait methods return `&str`.

**Safety invariants**:

- `Entry.text` is `Box<str>` — heap pointer is stable across `Vec` reallocation.
- Entries are append-only: no removal, compaction, or overwrite of `text` after
  insertion (other fields of an entry, such as the hash chain, may change; only
  `text` is borrowed).
- Returned `&str` points to the `Box` heap, not the `Vec` buffer.
- `NameTable` is `!Sync` (contains `RefCell`); no concurrent access.
- Lifetime is tied to `&self`.

## 2. ValidationRuntime: Self-Referential Fragment Builder

**File**: `src/validation/runtime.rs`, `begin_assertion_buffering()`

`ValidationRuntime` stores a `Box<Bump>` arena and a
`BufferDocumentBuilder<'a>` that borrows it — a self-referential pair.
The `unsafe` block extends the arena borrow lifetime to `'a`. Required for
XSD 1.1 assertion buffering during streaming validation.

**Safety invariants**:

- `Box<Bump>` heap address is stable across struct moves.
- Drop order: builder is declared before arena, so it drops first.
- Builder is always consumed (`take()`) before arena reset.
- No external mutable access to the arena while the builder exists.
- `Bump` uses interior mutability; builder holds `&Bump`, not `&mut Bump`.
- `ValidationRuntime` is `!Send + !Sync`.

## 3. WfReader::read_event_into: The Buffer After a Refused Read

**File**: `src/xml/reader.rs`, `WfReader::read_event_into()`

quick-xml reads an event's bytes into the caller's buffer and then checks that
they are UTF-8; when they are not, it returns `Error::Encoding` and the bytes
stay in the buffer. The well-formedness layer reports such input with the rule,
message and position its own checks give for those bytes, so it reads them
there. The event a successful read returns borrows the buffer for the
lifetime of the caller's `&'b mut Vec<u8>`, and the borrow checker extends
that borrow to the error path as well (NLL "problem case #3"; Polonius accepts
the code). Two blocks work around it: the call reborrows the buffer mutably
through a raw pointer made from that `&'b mut`, and the error path reborrows
it shared through the same pointer.

**Safety invariants**:

- The pointer comes from the function's own `&'b mut Vec<u8>`: valid,
  aligned and unaliased for `'b`.
- On success the event (derived from the mutable reborrow) is returned and the
  pointer is not used again.
- On an error nothing borrows the buffer: `quick_xml::Error` owns its data and
  the mutable reborrow ended with the call. The shared reborrow is the last
  use of the pointer and does not outlive the function.

The reader over a slice (`SliceWfReader::read_event`, behind
`drive_quick_xml_bytes`, `drive_quick_xml_with_bytes`,
`BufferDocumentBuilder::build_bytes` and schema loading) needs neither block:
its events borrow the input, not a buffer, and after a refused read quick-xml
has moved past the refused bytes exactly as the buffered reader has, so they
are the input between the offsets before and after the read (safe slicing;
the test `xml::tests::quick_xml_moves_past_refused_bytes_alike_in_both_readers`
pins that behaviour of quick-xml).

## Test code

**File**: `src/types/mod.rs`, `test_xml_type_code_roundtrip` (`#[cfg(test)]`)

The test transmutes each `u8` in `10..=60` to `XmlTypeCode` to walk the
built-in types. Sound because `XmlTypeCode` is `#[repr(u8)]` and every value
`0..=61` is one of its discriminants.

## Miri Verification

```bash
cargo +nightly miri test --lib namespace::table::tests
MIRIFLAGS=-Zmiri-disable-isolation cargo +nightly miri test --lib --features xsd11 \
    validation::runtime::tests::assertion_runtime_tests
MIRIFLAGS=-Zmiri-disable-isolation cargo +nightly miri test --lib --features xsd11 \
    types::tests::test_xml_type_code_roundtrip
cargo +nightly miri test --lib \
    xml::tests::bytes_that_are_not_utf8_are_refused_as_the_checks_refuse_them
```

The first covers block 1, the second block 2, the third the test-only
`transmute`, the fourth block 3 (both paths: every kind of event refused,
read whole and in one-byte chunks, after events that were returned; each
case also through the slice reader, which has no `unsafe`). The other
`xml::tests` run every reader case through both sources as well
(`cargo +nightly miri test --lib xml::`, also with
`MIRIFLAGS=-Zmiri-tree-borrows`).
`-Zmiri-disable-isolation` lets the XPath engine read the clock
(`fn:current-dateTime`), which an isolated Miri refuses. The whole library
can run under Miri as well (`cargo +nightly miri test --lib`, with
`--features xsd11` and the flag above), but slowly: the 23 assertion tests
alone take minutes.
