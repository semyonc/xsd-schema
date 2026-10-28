# Unsafe Code

The library contains four `unsafe` blocks, in three places; the tests one more.
This list is complete: `grep -rnw unsafe src` finds no other.

| # | Where | What |
| --- | --- | --- |
| 1 | `src/namespace/table.rs`, `NameTable::resolve_ref` and `try_resolve_ref` | `&str` into interned names without a `RefCell` guard |
| 2 | `src/validation/runtime.rs`, `begin_assertion_buffering` | the self-referential fragment builder of assertion buffering |
| 3 | `src/xpath/context.rs`, `DynamicContext::eval_function` | a re-borrow of the context's function evaluator |
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

## 3. DynamicContext::eval_function

**File**: `src/xpath/context.rs`

`eval_function(&mut self, handle, args)` calls the context's custom
`FunctionEvaluator`, which takes the context mutably. The evaluator is held as
`Option<&'a dyn FunctionEvaluator<N>>`; the block turns that reference into a
raw pointer and back before the call.

**Safety invariants**:

- The pointer is made from a `&'a dyn FunctionEvaluator<N>` the context holds
  and is dereferenced once, within the same call, as a shared reference: no
  lifetime is extended, and nothing is mutated through it.
- The evaluator lives outside the context — the context holds only a
  reference to it — so the shared reference and the `&mut DynamicContext`
  passed to `eval` do not alias.
- The reference is `Copy` with the lifetime `'a`, so a plain copy would do the
  same; the block is a candidate for removal.

## Test code

**File**: `src/types/mod.rs`, `test_xml_type_code_roundtrip` (`#[cfg(test)]`)

The test transmutes each `u8` in `10..=60` to `XmlTypeCode` to walk the
built-in types. Sound because `XmlTypeCode` is `#[repr(u8)]` and every value
`0..=61` is one of its discriminants.

## Miri Verification

```bash
cargo +nightly miri test --lib namespace::table::tests
cargo +nightly miri test --lib
cargo +nightly miri test --lib --features xsd11
```
