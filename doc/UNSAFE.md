# Unsafe Code

This crate contains two `unsafe` blocks. Both are verified under Miri.

## 1. NameTable::resolve_ref

**File**: `src/namespace/table.rs`

`resolve_ref(&self, id: NameId) -> &str` bypasses `RefCell`'s borrow guard
via `RefCell::as_ptr()` to return `&str` into interned string data without
allocation. Required because `DomNavigator` trait methods return `&str`.

**Safety invariants**:

- `Entry.text` is `Box<str>` — heap pointer is stable across `Vec` reallocation.
- Entries are append-only: no removal, compaction, or overwrite after insertion.
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

## Miri Verification

```bash
cargo +nightly miri test --lib namespace::table::tests
cargo +nightly miri test --lib
cargo +nightly miri test --lib --features xsd11
```
