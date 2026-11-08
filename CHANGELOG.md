# Changelog

All notable changes to the Kāra language and compiler will be documented in this file.

> **Maintenance note (2026-10-28).** This file is a coarse milestone record.
> 0.8.0 was released on 2026-11-01; 0.8.x releases carry fixes only. Progress is in
> [`docs/roadmap.md`](docs/roadmap.md), the bug ledger
> [`docs/bugs-v2.jsonl`](docs/bugs-v2.jsonl) (rollup in
> [`docs/bugs-v2.md`](docs/bugs-v2.md); the pre-M3 ledger is archived in
> [`docs/legacy/`](docs/legacy/)) and `git log`. The 2026-08-15 entry below
> describes the legacy compiler, deleted at M3 on 2026-10-24: its tree-walk
> interpreter, layout blocks, contracts, GPU and self-hosting claims do not
> describe v1.

---

## 0.8.2 — unreleased

Fixes only. Threads add entries here; 0.8.1 was tagged at 95de259351.

### Programs 0.8.1 accepted that are now refused

- A generic body that puts its parameter in an `Atomic` is judged at each
  call that instantiates it: `wrap(1i8)` over `fn wrap[T](x: own T) ->
  Atomic[T]` is refused with E0272, as `Atomic[i8]` written directly is
  (B-2026-11-07-35).
- An `i64` or `u32` into an `isize`, and a `u64` into a `usize`, need `as`
  at a binding, an argument or a return (design.md § Mixed-width operands):
  `isize` and `usize` may be 32 bits, so they widen as 64-bit types but take
  only what fits in 32. `let m: isize = k` with `k: i64` was accepted
  (B-2026-11-08-48).
- A socket, the HTTP client or a server reached from `main` is refused when
  compiling for `wasm_wasi` (design.md § Resources each target provides):
  WASI preview 1 has no socket `connect` or `bind`, so `Network` is not
  provided there. `karac check --target=wasm_wasi` passed such a program and
  `karac build` then failed in `wasm-ld` on internal runtime symbols; the
  refusal now names the call chain (B-2026-11-08-37).
- `collect()` into a `Result[C, E]` refuses items whose error type is not
  `E`: `Err(5)` items into a `Result[Vec[i64], String]` were accepted
  (iterators.md § collect, B-2026-11-08-23).
- The deferred `std.cli` parser's `Parser`, `Arg`, `Args`, `CliError`,
  `Subcommand` and `SubcommandResult` are reached through `import std.cli`
  (deferred.md § std.cli), as `std.regex` and `std.json` are: `karac check`
  refuses a bare use, with the import as its fix, and `karac build` and
  `run` are unchanged. A program's own `enum CliError` or `struct Parser`
  no longer draws a `prelude_shadow` warning on every command
  (B-2026-11-08-29).

### Added

- **A block may declare a constant.** `const MOD: i64 = 1_000_000_007;` as
  the first line of a function body, the way a Rust solution writes it, was
  a parse error advising `r#const`. Its scope is the rest of the block; it
  shadows like a `let`; its initializer names constants only, and naming a
  local is refused with that reason (design.md § Constants,
  B-2026-11-08-60).

### Changed

- **`contains` and `binary_search` on a sequence of `String` or `Vec[T]`
  take the element's view.** `["inc", "dec"].contains(cmd)` with `cmd: Str`,
  `names.contains(s)` on a `Vec[String]` and `rows.contains(w[1..3])` on a
  `Vec[Vec[i64]]` check and allocate nothing, as a table's lookups already
  did (docs/library/collections.md, "Lookup by view"; B-2026-11-08-55).
  They were refused, and `karac fix` added a `.to_string()`.
- **`None` orders before every `Some(x)`.** `sort()` on a `Vec[Option[T]]`,
  `<`, `cmp`, `min`/`max`, `SortedSet`/`SortedMap` keys and `PriorityQueue`
  now put `None` first, as Rust, Haskell and OCaml do
  (docs/library/core-types.md § Ordering). 0.8.1 put `None` last, after its
  declaration order (B-2026-11-08-24). `Ok(x) < Err(e)` is unchanged.
- **`karac fix` moves every `usize` to `i64` together.** When a program
  mixes `usize` with `i64`, the casts (`e[1] as usize`) and `usize.MAX`
  are rewritten along with the written `usize` types, so the fix no longer
  leaves a value it did not retype feeding one it did (B-2026-11-08-49).

- **An operator fixes a binding's literal beside a type that does not widen
  into `i64`** (design.md §8 Literals). `let k = 2; i - k` with `i: u64`,
  and Rust's `let mut k = 1; while k * k <= n` over a `usize`, were refused
  as mixing `u64` (or `usize`, `i128`, `u128`) with `i64`; the literal now
  takes that type. Beside a type that widens (`u8` through `i32`) it keeps
  `i64`, as before (B-2026-11-08-18).
- **A number literal takes the type a later use gives it** (design.md § Type
  inference). `let x = 5; take_i32(x);` makes `x` an `i32`, the literals
  pushed onto a `Vec.new()` take the element type of the parameter it is
  later handed to, and `let a = 255; let b: u8 = a;` is a `u8` literal. A
  literal nothing fixes still defaults to `i64` / `f64` at the end of its
  body. Programs 0.8.1 refused as a narrowing now run; a literal that does
  not fit the type it is fixed to is now refused at the literal
  (`let a = 300; let b: u8 = a;`), and arithmetic in the narrower type traps
  on overflow there (B-2026-11-07-3).
- **An unannotated collection filled from references to a `Copy` type holds
  the values** (design.md §8). `let mut m = Map.new(); for x in nums.iter()
  { m.insert(x, 1) }` builds a `Map[i64, i64]`, as do `push`, `entry` and a
  literal element such as `[x, x]`; 0.8.1 refused `push` and `insert` there
  because the loop's per-item copy was lent past its iteration, and compiled
  `entry` with a reference slot fed a value. A written `Map[ref i64, V]` or
  `Vec[ref i64]` keeps its references (B-2026-11-08-19).

### Wrong behaviour fixed

- **A method returning `Self` in an impl of a `shared struct` is the
  handle type at its call.** `let y: Node = Node.new(2);` was refused
  "expected 'Node', found 'Node'" (B-2026-11-08-70).

- **A slice local may be bound to its own sub-slice.** `rest = rest[1..]`
  on a `Slice[T]` local was refused with E0516, as if the sub-slice
  borrowed the local; it borrows what the slice borrows, as the `Str` twin
  already did (core semantics §5.2; B-2026-11-08-66). A body that writes
  through a `mut Slice` keeps the stricter reading.
- **A negated float literal is an `f32` method's argument.**
  `h.copysign(-1.0)`, `pow`, `hypot` and `atan2` on an `f32` refused the
  `-1.0` as an `f64`; a bare `1.0` was accepted (B-2026-11-07-18).
- **Parsing or splitting a borrowed string inside a `par {}` branch is
  accepted.** The effect checker did not know the text library's natives
  (`str` methods), read them as an unknown effect, and refused the branches
  as conflicting (B-2026-11-07-36).
- **A string literal inside `Some(..)`, `Ok(..)`, `Err(..)` or a tuple
  compares with an `Option[Str]`** (or a `Result` or tuple of `Str`):
  `name == Some("bob")` was refused with a type mismatch (B-2026-11-07-54).
- **A method called straight on a constant reads it at its declared type.**
  With `const CC: u8 = 200;`, `CC.leading_zeros()` gave 56 (the `i64`
  count) and a `u64` or `i128` constant past `i64` printed through
  `to_string()` as a negative number; `let v = CC;` and `println(CC)` were
  already right (B-2026-11-07-56).
- **A later use fixes a const parameter only the result names** (design.md
  § Const generic parameters). `let b = zeros(); let c: Array[i64, 4] = b;`
  over `fn zeros[const N: i64]() -> Array[i64, N]` was refused at the first
  `let`; an argument, a struct field or the caller's own `Array[i64, M]`
  fixes it too. One nothing fixes is refused at the call once the body has
  been checked (B-2026-11-07-9).
- **A value assigned to a binding, or a generic call's argument, fixes the
  binding's literal.** `let mut b = 0; b = y;` and `max(b, y)` with
  `y: i32` made `b` an `i64`, and a function returning `i32` then refused
  it as a narrowing (B-2026-11-07-57).
- **A binding used as an element of a number collection literal takes its
  element type.** `let p = 0; let v: Vec[i32] = [p, 1];` and
  `out.push([prev, pos])` into a `Vec[Vec[i32]]` made `p` and `prev` an
  `i64` and refused them as a narrowing (B-2026-11-08-21).
- **A borrowed number passed for a wider parameter is read, then widened.**
  `take(x)` with `x: ref u8` from a slice iterator and `take(x: i64)`
  passed the reference's address in a compiled program and stopped the
  interpreter on `Add on Int and Ref`; it now adds the value
  (B-2026-11-08-31).
- **A number, `char` or `bool` literal inside `Some(..)`, `Ok(..)` or a
  tuple compares with the other side's element type.** `m.get(k) ==
  Some(3)` over a `Map[String, i64]`, `v.first() == Some('a')`, and
  `o == Some(4)` with `o: Option[i32]` were refused as mismatches; only a
  string literal was promoted (B-2026-11-08-22).
- **A part destructured out of a slice read no longer holds the slice
  borrowed.** `let (at, _) = v[i..j].iter().enumerate().min_by_key(..).unwrap();
  v.remove(at);` was refused as a mutation of `v` while borrowed, though
  `at` is an owned `i64`; a part that is still a borrow keeps the slice
  borrowed as before (B-2026-11-08-12).
- **`split_whitespace()` and `split(sep)` walk from both ends**, so
  `s.split_whitespace().rev()` gives the words last first and
  `s.split(",").rev()` the forward pieces in reverse; both were refused with
  "no method 'rev'" (B-2026-11-08-15).
- **A literal argument of a generic call takes the type of its result.**
  `v.iter().skip(1).fold(0, |x, _| x + 1)` returned as an `i32` was refused
  as an `i64`, while `v.iter().fold(0, ..)` was accepted (B-2026-11-08-44).
- **An `isize` or `usize` operand beside a wider type compiles.**
  `fn f(i: isize, k: i64) -> i64 { i - k }` passed `karac check` and then
  failed with "invalid MIR: binary operands of different types isize and
  i64"; the `isize` now widens, as does a `usize` beside a `u64`
  (B-2026-11-08-48).
- **`collect()` into a `Result[C, E]` types a closure's `Ok(..)`.**
  `let m: Result[Vec[i64], String] = xs.iter().map(|p| Ok(p.len())).collect();`
  was refused as "cannot infer type parameter 'E'", with advice to annotate
  a binding the closure does not have; the collect's `E` now reaches it
  (B-2026-11-08-23).
- **A package that declares several `[build] targets` is checked per target
  by the check a one-target package gets.** `s.split(" ").count()` was
  refused there as "no method 'count' on type 'Vec'", findings came without
  snippets and repeated under each target; now a finding every target makes
  is reported once under "all targets", as for a file (B-2026-11-08-38).
- **The playground runs exactly what `karac check` accepts.**
  `karac::run_playground` ran its own older checks, so a bare `for` that
  moves its elements out ran there (E0515 on the command line and in the
  editor), and an atomic operation without the deprecated `MemoryOrdering`
  argument was refused. The book's chapter 14 atomics example drops the
  orderings (B-2026-11-08-40).

### Performance

- **The overflow-safe midpoint `lo + (hi - lo) / 2` halves with a shift**
  under its `lo < hi` guard, which keeps `hi - lo` positive; the signed
  division's sign fixup is gone. Kata 1064 on x86 went from 260 ms to
  170 ms (B-2026-11-08-1).
- **A field of a `shared` struct that is not `mut` takes no borrow flag.**
  No handle can write it, so a loan of it cannot conflict; each loan took
  and gave back a run-time flag all the same. Kata 1506's read-only walk
  over `children` went from about 270 ms to 160 ms on x86
  (B-2026-11-08-14).
- **A small `Map` or `Set` is one allocation.** The table's header, room
  for its first four entries and their hashes, and its index are allocated
  together; a table that grows moves its entries and hashes to one block,
  and the record of removed slots is allocated only once one is removed.
  A table used to make five allocations for its first entry. Kata 491 on
  x86: 13.06M allocations to 5.59M (Rust: 5.60M), 5.38G instructions to
  4.07G, 0.45 s to 0.35 s (B-2026-11-08-7).
- **A `Map` or `Set` lookup reads eight index cells at a time.** The
  index is now one control byte per cell (the top seven bits of the slot's
  hash) read as a word, then 4-byte slots, kept up to 7/8 full; it was an
  8-byte cell per slot kept at most half full, so a table of 200,000 keys
  had an 8 MB index where it now has about 1.3 MB, and a lookup touches a
  slot and an entry only where the tag matches. Kata 454 on x86: about
  620 ms to 520 ms (Rust 411 ms), branch mispredicts 8.2M to 4.1M; kata
  219: 760 ms to 690 ms (B-2026-11-08-8).

### Diagnostics

- **`karac fix` carries Rust's `Rc<RefCell<T>>` to a `shared struct`.**
  LeetCode's tree template stopped at "unknown module `std.rc`, did you mean
  `std.io`?". The `use std::rc::Rc;` line's fix now deletes it, declares
  each struct held in an `Rc<RefCell<T>>` a `shared struct` with `mut`
  fields, writes the type as `T` and `Rc::new(RefCell::new(x))` as `x`, and
  drops `.borrow()` / `.borrow_mut()`; `Rc` and `RefCell` elsewhere name
  `shared struct` (B-2026-11-08-65).

- `Ok[i64, String](3)` is refused by `karac check` with the qualified form,
  `Result[i64, String].Ok(..)`: a call takes no type arguments, and the
  brackets were dropped without a word (B-2026-11-08-23).
- **`Json.deserialize` errors name the field, what it wanted and what it
  found, and the line and byte column**, as `Json.parse` does:
  `title: expected a string, found 3 at line 1 column 11`, and
  `expected value at line 2 column 12` where no value starts. They read
  `expected '"' at character 10` before; a service that returns the
  message to its client now tells it which field was wrong
  (B-2026-11-08-27).
- A narrowing refused because a binding's literal kept its default `i64`
  (`let mut b = 0; b += y;` returned as `i32`) names the binding and offers
  its annotation, `let mut b: i32 = 0`, as a machine fix, in place of
  `as i32` at the use (B-2026-11-08-20).
- Comparing an `Option[ref T]` with an `Option[T]` whose `T` is not `Copy`
  now offers `.cloned()` as the fix, after the view and `as_slice` fixes
  (B-2026-11-07-54).
- **Another language's first-day words are named, not guessed at.**
  `null` (fixed to `None`), `nil`, `True` / `False` (fixed), Python's
  `range(n)` (fixed to `0..n`) and `def`, Go's `func` (fixed to `fn`),
  `fmt.Println(x)` (fixed to `println(x)`) and `(v, err)` over a `Result`
  each get a message in Kāra's terms. A did-you-mean suggests a value name
  for a value name and, for a name of five letters or fewer, only a
  one-edit neighbour, so `karac fix` no longer rewrites `null` to `Mul` or
  `func main()` to `func; main();` (B-2026-11-07-40).
- **Rust's day-one shapes get the Kāra form, and a fix only where it is the
  one repair.** `std::thread::spawn(..)` names `TaskGroup.spawn` and `par`
  with no fix (the fix used to write a free `spawn`, which the next check
  refused) (B-2026-11-07-41). `Self { x }` in an impl is fixed to the
  type's name, `P { x }`, where `karac fix` used to insert a `;`
  (B-2026-11-07-42). A body whose last statement is `a + b;` of the return
  type is fixed by removing the `;`, and `?` in a `fn main()` returning
  `()` is fixed to `fn main() -> Result[(), AnyError]` ending in `Ok(())`
  (B-2026-11-07-43).
- A dangling borrow return's help (E0509) teaches the borrow-by-default
  spelling, `fn f(x: T) -> ref T { x }`, instead of the `ref T` parameter
  `karac fix` removes.
- A known variant such as `None` in a `match` over a scrutinee that is
  already an error is no longer also reported as a Type-class binding.
- **Three Rust shapes in a program written as Rust get a `karac fix` edit.**
  A `mut arr: Vec<i32>` parameter is fixed to `arr: own Vec[i32]` with
  `let mut arr = arr;` at the top of the body (B-2026-11-08-11).
  `if let Some(&i) = v.last()` is fixed with `.cloned()` on the scrutinee,
  keeping `i` a copy, where the fix used to drop the `&` and make `i` a
  borrow (B-2026-11-08-13). A `usize` size where an `i64` is wanted
  (`Vec.with_capacity(n * 2)` after `let n = x as usize;` over an `f64`) is
  fixed with `as i64`; when the `usize` comes from a cast, type or suffix
  that the same pass fixes to `i64`, the use is left alone
  (B-2026-11-08-16).
- E0411 (a target lacks a resource the call chain uses) cites design.md
  § Effect-driven target gating, the section that states the rule, and
  offers binding a provider only for a resource one can be bound for: not
  `ProcessTable`, `Heap` or the hardware (B-2026-11-08-39).
- A program's own `struct Response { status: i64, body: String, headers:
  Vec[(String, String)] }`, the shape io.md documents for sending headers,
  no longer draws a `prelude_shadow` warning saying the servers' built-in
  paths no longer apply: they serve it (B-2026-11-08-26).

### Tooling

- `karac fmt` keeps a comment beside the parameter or call argument it
  follows: such a list is laid out one element per line, where the comment
  used to move into the body or to the end of the statement
  (B-2026-11-07-44).

---

## 0.8.1 (beta) — 2026-11-07

Fixes only. Several close soundness holes, so programs that were wrongly
accepted by 0.8.0 are now refused, each with a fix-it.

### Programs 0.8.0 accepted that are now refused

- **Branches of different numeric types no longer join.** `if c { a_i32 }
  else { b_i64 }` and a `match` with a `u8` arm beside an `i64` one were
  typed as one of the two; design.md §8's LUB rule 4 gives them no join.
  Annotate the binding (`let v: i64 = if ..`), where each branch widens. A
  branch that is a literal, or an `if` or `match` of literals, still takes
  its sibling's type (B-2026-11-06-15).
- **A bounded struct's literal meets its bound without an annotation.**
  `let p = Pair { a: Q {}, b: Q {} }` with `struct Pair[T: Ord]` (or
  `where T: Ord`) was accepted when `Q` has no `Ord` (B-2026-11-06-15).
- **`Atomic[T]` holds only `bool`, `i32`, `i64`, `u32` and `u64`**
  (library/concurrency.md § Atomics). `Atomic[i8]`, `u8`, `i16`, `u16`,
  `usize`, `isize`, a 128-bit integer and a raw pointer were accepted, and
  `Atomic[i128]` failed to link under AddressSanitizer
  (B-2026-11-07-21).
- **A match on another package's `#[non_exhaustive]` enum needs a wildcard
  arm,** even when it names every variant, and wherever the enum sits in
  the scrutinee (`Err(DecodeError.InvalidLength)`); `karac fix` adds it. A
  user package's types are covered now, not only the standard library's,
  and so are the struct literal and the struct pattern without `..`
  (B-2026-11-07-23).
- **A literal range bound must fit the scrutinee's type,** as a literal
  pattern must: `1i32..=300i32` on an `i8` and `-1..=3` on a `u8` were
  accepted (B-2026-11-06-15).
- **A `String` building method on a `Str`.** `push_str`, `push`, `extend`,
  `clear`, `pop`, `truncate` and `reserve` on a `Str` view (a parameter, a
  literal, a slice) passed `karac check` and then failed on every backend
  with an internal error. They are refused at check with a hint to call
  `.to_string()` for an owned `String` first, as docs/library/strings.md
  says (B-2026-11-06-31).
- **A type nothing in the body fixes.** `let v = Vec.new();` used only for
  `v.len()`, a channel whose element type nothing names, and a closure whose
  parameter is never typed (`let f = |x| x;` never called) are refused at the
  end of the body with "cannot infer type parameter ..." or "cannot infer
  closure parameter type", as design.md § Type inference says
  (B-2026-11-06-15). Annotate the binding or use the value.
- **A multi-segment attribute path outside the compiler's namespaces.**
  `#[tool::name]` was dropped silently; attribute paths have one segment in
  v1, apart from `karac::` (the protobuf derive's `#[karac::proto]` and
  `#[karac::field]`) and `diagnostic::`, and an unknown `karac::` name is
  refused too (design.md § Attributes, B-2026-11-05-17).
- **A derive helper where no derive reads it.** `#[serde(...)]` outside a
  struct, an enum, a field or a variant, and `#[karac::proto]` /
  `#[karac::field]` off a field, were dropped silently; they are
  `E_HELPER_ATTRIBUTE_INVALID_POSITION`.
- **`karac check` borrow-checks functions nothing calls yet, and libraries.**
  The MIR move and borrow checks ran only on what `main` reaches, so a
  library's `pub fn bad() -> Str { let s = ..; s.as_str() }` checked clean
  and was refused at its first caller's build. Every non-generic function of
  the program is now checked, with or without a `main` (B-2026-11-04-56).
- **A function or `dyn` value that crosses a task must say so.** An erased
  `Fn` / `MutFn` / `OnceFn` type, or a `dyn T`, is `CrossTask` only when
  written `Fn(A) -> R + CrossTask` / `dyn T + CrossTask`, and an unmarked one
  may not reach a task or `par` branch even as a view (core-semantics §6.3;
  B-2026-11-01-26, B-2026-11-02-12). `karac fix` adds the marker where the
  type is declared. `Server.serve*` handlers and `std.time.timeout` take
  marked parameters; a closure literal passed directly needs no change.
  A collection of unmarked function values (`Vec[Fn() -> i64]`) cannot be
  passed where the marked form is expected; mark the collection's type, and
  each push is checked (B-2026-11-02-14). The type-mismatch error names the
  missing marker (B-2026-11-02-13).
  The same holds through generic calls, `collect()` and `mem.replace` /
  `mem.swap` (B-2026-11-02-15), and through every generic method,
  associated function and closure result (B-2026-11-02-18).
- **A task spawned inside `with_provider` onto a `TaskGroup` made outside it**
  is refused, since it could outlive the provider (§14.5, B-2026-11-01-27).
- **A task that reads a `Copy` local borrows it** until the group joins, so a
  later write is refused; copy it first, `let c = k;` (§9.1, B-2026-11-01-32).
- **`with_provider` moves its provider**, so a later use is E0500 (§14.4,
  B-2026-11-01-33).
- **`m[key] = key.len()` with a moved key** is E0500 with a hoist fix-it,
  because the key moves before the value runs (§8.2, B-2026-11-01-46).
- **Writing a map's key through `iter_mut()`** (`for (k, v) in
  m.iter_mut() { k.push_str("x"); }`) is refused: `iter_mut()` over a `Map` or
  `SortedMap` yields `(ref K, mut ref V)` (collections.md), and a key changed
  in place breaks the map's hash or order. `remove` and `insert` instead.
  The value binding is now a `mut ref V` for a `Copy` `V` too, so `*v = 9`
  writes it and `v = 9` gets the auto-deref diagnostic (B-2026-11-03-17).
- An or-pattern whose alternatives bind different names (`A(x) | B(y)`)
  is refused (design.md §5 Or-patterns; B-2026-11-04-60).
- `struct Marker;` is refused; a struct with no fields is `struct Marker {}`
  (design.md §5 Structs), which `karac fix` writes (B-2026-11-04-60).
- **Function kinds (design.md §7).** Calling an `OnceFn` local twice is
  E0500, and the error names the capture that makes it `OnceFn`; `|| s`,
  which returns its capture, is one (B-2026-11-04-67). A function value that
  is not a closure literal or a function name (a call's result, an `own` or
  `escaping` parameter) is move-only, so `let h2 = h;` moves `h`. Calling a
  local `MutFn` needs `let mut`, and a bare `OnceFn` parameter cannot be
  called (`own OnceFn`); `karac fix` writes both (B-2026-11-04-69). Passing
  a `MutFn` local to an owned parameter (`own MutFn()` or `escaping`) moves
  it, so `take(f); take(f);` is a use after move (B-2026-11-05-2).
- A UFCS call to a `mut ref self` method marks its receiver,
  `Counter.bump(mut c)`, and `own` is not written beside `escaping`; `karac
  fix` writes both (B-2026-11-04-69).
- **Soundness holes in borrowing and tasks.** Each was accepted and is now
  refused: a `self` method that returns a borrowed argument as its
  result (B-2026-11-02-28); a non-escaping `Fn` parameter captured by a
  returned closure (B-2026-11-02-31); a closure in a let-bound tuple
  treated as non-escaping (B-2026-11-02-39); a `shared` handle entering a
  `par` branch through a local closure (B-2026-11-03-8); a `Map` key
  written through `iter_mut()` (B-2026-11-03-26); a view (`ref T`) sent
  through a channel, which read freed memory at run time
  (B-2026-11-02-27); `Option.take` / `replace` / `get_or_insert` through
  a borrowed place (B-2026-11-07-16).
- **Effects are charged where the spec charges them.** A `?` charges its
  `From` conversion's effects (B-2026-11-04-18); a call charges the
  callee's declared contract, not its body's (B-2026-11-04-26); two `par`
  branches that write the same user-declared resource conflict
  (B-2026-11-04-25); a declared clause is verified on private functions
  and trait default methods (B-2026-11-04-27); a `dyn` value's drop
  charges its drop glue's effects (B-2026-11-01-39);
  `#[no_effect(allocates(Heap))]` sees `to_vec`, `to_string`, f-strings
  and collection literals (B-2026-11-04-28); integer `/`, `%` and
  checked overflow count as `panics` (B-2026-11-04-29).
- **Trait and type rules.** `Copy` beside `Drop` on one type (the drop
  ran once per copy, B-2026-11-06-24); a derive beside a manual impl of
  the same trait, `#[derive(Drop)]`, an impl for `dyn Trait`, `clone(own
  self)` and an unknown associated binding in a bound
  (B-2026-11-06-27); overlapping impls and an inherent impl on a standard
  type (B-2026-11-04-14, -15); `dbg` of a type without `Debug`, and a
  derived `Debug` or `Clone` on a generic type whose arguments lack it
  (B-2026-11-07-11).
- **Names.** `let`, `for`, closure and pattern bindings must be
  value-class (lower case), as parameters already were (E0127); a type or
  constant name may not start with `_` (B-2026-11-06-18).
- **Constants are evaluated at compile time.** An integer, bool or char
  initializer that overflows, divides by zero or shifts out of width is a
  check error: `const SH: i32 = 1 << 40` read as 0, and `i64.MAX + 1` or
  `1 / 0` read 0 or panicked at each use (B-2026-11-06-20). A cycle between constants is an error instead of a
  compiler stack overflow (B-2026-11-06-19), and polymorphic recursion
  is refused instead of hanging the compiler (B-2026-11-06-12).
- A process spawn is refused on `wasm_wasi` by `karac check
  --targets=all` and a two-target build (B-2026-11-05-11).

### Deprecated (errors at the next minor release)

Each still compiles in 0.8.x with a `deprecated` warning and a
`karac fix` edit; the next minor release refuses it.

- **An implied derive.** `Copy` without `Clone`, `Eq` or `PartialOrd`
  without `PartialEq`, `Ord` without `PartialOrd` and `Eq`: the compiler
  fills the missing derive in and warns (W0245); `karac fix` writes it
  (design.md §9, B-2026-11-06-27, tracked by B-2026-11-07-10).
- **An f-string nested inside an f-string's hole.** When the hole is the
  inner f-string alone, `karac fix` writes its text in place
  (design.md §3, B-2026-11-06-18, tracked by B-2026-11-07-32).
- **`mut` on a field of a plain struct or enum.** Mutability belongs to
  the binding; `karac fix` deletes the field's `mut` (design.md §5,
  tracked by B-2026-11-04-79).

### Changed

- **`karac fmt` puts each `impl` and `trait` body in canonical order**
  (design.md § Canonical formatting): associated types first, then functions
  without `self` (constructors such as `new`), then methods, each group
  alphabetical. Doc comments, comments and attributes travel with their
  item. A formatted program whose members were in another order changes at
  its next `karac fmt` (B-2026-11-05-32).
- **Build output paths follow the spec.** In a package, `karac build` writes
  `dist/native/<pkg>` and `karac build --target=wasm_wasi` writes
  `dist/wasm/<pkg>.wasm` (design.md § 16). They were `dist/<pkg>` and
  `dist/<pkg>.wasm`, so a script that runs the built binary needs the new
  path. `karac install` takes the binary from the new place
  (B-2026-11-05-12).
- **`karac check --targets=all` checks the v1 targets**, native and
  wasm_wasi, plus any target an item in the file or package is gated on. It
  also checked wasm_browser and gpu, and gpu refuses any program that
  prints, so hello world failed it (B-2026-11-05-12).
- **`[profile.release]` sets the CPU baseline.** Its `target-cpu`,
  `target-features` and `cpu-baseline` are the manifest rung of each
  precedence chain (design.md § Build profiles); the table was ignored as
  an unknown key. The older `[release]` spelling is still read, with a
  warning, and a key in `[profile.release]` wins (B-2026-11-05-12).
- `karac build --target-cpu=help` and `--target-features=help` print their
  listing on stdout, not stderr (B-2026-11-05-12).
- `karac check --output=json` on a package with several targets prints one
  object, each target's own findings tagged with the target and those on
  every target once under `shared_diagnostics`, as a file check already
  did. It printed one untagged object per target (B-2026-11-05-12).
- **A package reports in the shapes a file does.** `karac check
  --output=jsonl` on a package member, `karac build --output=jsonl` and
  `karac run --output=jsonl` in a package stream `build_start`, phase
  events, `diagnostic` events with ids and `build_complete`, and a member's
  check runs every phase, so an ownership error after a type error is
  reported. `--output=json` is the batch document, with ids and
  `program_effects`. A member check printed bare `type_error` lines and
  stopped at the type checker, and a package build printed its entries with
  no `type` (design.md § 17, B-2026-11-05-26).
- **JSON diagnostics have one fix shape.** Every machine-applicable fix is a
  hint in the spec's form, `hints: [{"description", "diff": {"file", "line",
  "old", "new"}}]`, with the byte-level edits it summarizes as
  `replacement` (one edit) or `fix_diff` (several), on a file and a package
  alike. The `fix_it` object and `fixes` array a 0.8.0 type-check record
  carried beside its `replacement` are gone, so a tool that reads them reads
  `replacement` instead (design.md § 17, B-2026-11-05-30).
- **Comparisons see through `ref`.** `==`, `!=`, `<`, `<=`, `>` and `>=`
  compare the values whether either side is `T`, `ref T` or `mut ref T`,
  for any `T`, and move neither; arithmetic keeps its Copy-only rule
  (design.md § No automatic dereference in operators, B-2026-11-05-1).
- **The Linux tarballs need glibc 2.35, not 2.39.** They are built on
  Ubuntu 22.04 and the release checks every binary's symbol versions, so
  they start on Ubuntu 22.04 and Debian 12, where 0.8.0's did not. RHEL 9
  and Amazon Linux 2023 (glibc 2.34) still build from source
  (B-2026-11-07-48). README's Install section gives the
  download, checksum and PATH steps (B-2026-11-07-49).
- `"hello".substring(-5, 2)` clamps the start to 0 and gives `"he"`, as
  strings.md says; it gave `""` (B-2026-11-03-16).
- `karac fmt` keeps a one-element tuple type's comma (`(i64,)`), which it
  dropped (B-2026-11-05-14).

### Added

- **Every JSON diagnostic carries a `concept` id** naming the spec section
  that specifies it (design.md § 17 Concept ids, 45 ids), and `karac
  explain ownership/move-semantics` names those sections and the codes that
  carry the id. `karac explain src/order.kara.process` reports a function's
  effects, each with the chain of calls that gives it, and each parameter's
  declared mode beside the weakest mode its body needs (B-2026-11-05-28).

- **The empty effect clause `with ()` on a function type.** A field or
  collection element typed `Fn(A) -> R with ()` holds only values whose
  effects are default-permitted (`allocates(Heap)`, `panics`), and a call
  through it has none, so two `par` branches calling stored pure callbacks
  do not conflict (design.md § How a call through a function value gets its
  effects; B-2026-11-03-1). It is a parse error on a `fn` declaration,
  with a fix-it that removes it. Storing a value into a field of an
  unannotated struct local is now checked against the field's clause too.
- **The release bundle builds wasm.** It now ships the wasm runtime archives
  and the wasi-libc they link with under `lib/`, so
  `karac build --target=wasm_wasi` needs only a wasm linker: the `rust-lld`
  of any rustup toolchain (no wasm target needed), or LLVM's `wasm-ld`. A
  karac built from source looks for wasi-libc in three places, in order:
  the bundle, the `rustc` on PATH, then the toolchain karac was built with.
  Before, it looked only at the `rustc` on PATH, so a wasm build outside
  the repository failed unless `RUSTUP_TOOLCHAIN` was pinned
  (B-2026-11-05-12).
- **The five style lints `karac fix` applies** (design.md § `karac fix` and
  `karac explain`, B-2026-11-05-31). Each is a lint (`W0295`) with a
  machine-applicable fix, off in `check`, `build` and `run` and on in
  `karac fix` (`-W NAME` shows one anywhere): `counter_while_loop` turns
  `let mut i = a; while i < b { ..; i += 1; }` into `for i in a..b { .. }`,
  `manual_compound_assign` turns `x = x + e` into `x += e` on a number (and
  on a `String` for `+`),
  `needless_return` turns a function's final `return e;` into `e`,
  `redundant_literal_method` removes `.to_string()` on a string literal and
  `.iter()` on an array literal a `for` iterates, and `redundant_ref_param`
  removes `ref` from a parameter's type. Each stays silent where the rewrite
  would not mean the same (the counter read after the loop, a `continue`, a
  bound the body changes), and `karac lint --list` lists them.
- Script mode's implicit `main` returns `Result`, so a top-level `?`
  works (B-2026-11-04-19). A nested module imports as itself (`import
  db.connection;`, B-2026-11-04-21). A struct pattern with `..` destructures
  a parameter (B-2026-11-04-63). A blanket impl of the program's own trait
  is found (B-2026-11-04-16).
- Library: `Vec.drain(range)` (B-2026-11-04-77), `Vec.rotate_left` /
  `rotate_right` (B-2026-11-06-35), `Slice.iter_mut` (B-2026-11-06-37),
  `.step_by(n).rev()` over a range (B-2026-11-05-35), `RangeTo`,
  `RangeToInclusive` and `RangeFull` as values and types
  (B-2026-11-05-37), `CString.from_bytes` and `CString`'s `to_str` /
  `to_string` (B-2026-11-06-32), `Option.from` / `Result.from`
  (B-2026-11-07-11), and reading or upgrading a `weak T` through a `ref`
  binding (B-2026-11-07-7).
- The debugger contract's runtime records: a JSONL panic event under
  `karac run` and `karac test`, spawn-site ids, parent frames, task effect
  summaries and the peer a task waits on (B-2026-11-05-18, -34, -36).

### Wrong behaviour fixed

- `import std.sync.{RecvTimeoutError, SendTimeoutError}` names the two
  timeout errors; it was refused as `std.sync has no item`
  (B-2026-11-07-21).
- A struct literal of another package's struct may name its `pub` fields
  again: `Opts { port: 1 }` with `pub port` was refused with E0111 since the
  literal check for B-2026-11-04-46 (B-2026-11-07-22).
- **A client that drops its connection never ends a server.** A compiled
  program now ignores `SIGPIPE` from the start, as the interpreter does, so
  an HTTPS client that disconnects without `close_notify` fails only its own
  connection; the server died with signal 13 and refused the next client
  (B-2026-11-07-25).
- **A server reads an absolute-form target as its path.** `GET
  http://example.com/z?k=v` gives `path()` `/z`, and `/` with no path, as
  `Request.new` does (B-2026-11-07-26). A request line or header that does not
  parse is answered `400 Bad Request` without calling the handler, and the
  connection closes (docs/library/io.md).
- **A generic function's `impl Iterator` collects.** `fn evs[T](xs: own
  Vec[T]) -> impl Iterator[Item = T]` then `evs(v).collect()` failed with
  "the method call `.collect` with no resolved callee is not lowered yet"
  on every backend; `count` worked. The witness is now read at the caller's
  type arguments (B-2026-11-06-26).
- **Const generic parameters work in function bodies.** In
  `fn f[const N: i64](a: Array[i64, N])`, `N` is now the instance's value
  (`N`, `N - 1`, `[0; N]`), and a parameter only the return type names is
  solved from the annotation (`let z: Array[i64, 4] = zeros();`). Each
  length is its own instance, so `a.len()`, `a[0]` and `for x in a` compile
  under `karac run` and `karac build`. Before, the instance dropped `N`
  and these failed with `has no layout` or `index into non-array`
  (design.md § Const generic parameters, B-2026-11-06-14, B-2026-11-06-17).
- **A named constant in a pattern compares against its value.** With
  `const LIMIT: i64 = 3`, `match x { LIMIT => .., _ => .. }` bound a new
  `LIMIT` and took that arm for every `x`, on every backend; so did
  `Some(LIMIT)`, a `Str` constant and `if let LIMIT = 5`. A constant now
  matches only its value, an integer type's limit (`i64.MAX`) is accepted
  as one, and a constant with neither an integer or `char` type nor a
  literal value is refused (design.md § Pattern matching, B-2026-11-06-16).
- A type parameter that only a call's result names is solved by a later use
  in the body: `let mut h = PriorityQueue.new(); h.push((3, 1))`, a
  `Bag.new()` from `impl[T] Bag[T]` followed by `b.push(x)`, and
  `fn empty[T]() -> Vec[T]` followed by `v.push(3)` were refused at the `let`
  with "cannot infer type parameter 'T'". One nothing in the body solves is
  still refused, at the call, and the impl's bounds are checked against the
  type the later use fixed (design.md § Type inference, B-2026-11-06-7).
- The same holds for a binding of `None` or `Ok(..)` (`let mut best = None;`
  then `best = Some(x)`), for a call whose result is the bare parameter
  (`let x = make(); let y: i64 = x;`), and for a call inside a generic
  function whose own parameter has the same name (`Bag.new()` inside
  `fn g[T]`, which read the Bag's `T` as `g`'s) (B-2026-11-06-11).
- A comparison or arithmetic operand whose type is still open is fixed by
  the other operand: `x > best.unwrap()` after `let mut best = None;` was
  refused with "cannot compare 'i64' and '?T0'". A bare `collect()` handed to
  a generic collection parameter (`largest(it.collect())` over
  `fn largest[T](v: Vec[T])`) builds that collection of the iterator's items;
  it stopped with an internal "UnknownParam("T") is not lowered yet"
  (design.md § Type inference, B-2026-11-06-13).
- **`let` can destructure a `shared` value.** `let H { xs, n } = h` on a
  `shared struct H` was refused with ``struct pattern `H` used but type is
  `H` `` though `match` accepted it. It now binds a non-`Copy` part as a `ref`
  into the value, as `match` does, and leaves the handle intact (core
  semantics §4.6, B-2026-11-06-10).
- A `for` over an open range `lo..` now overflows when it steps past the
  type's largest value. It used to wrap: `for b in 254u8..` went 254, 255,
  0, 1, and the interpreter refused the loop. `RangeFrom` now gives out the
  type's largest value before overflowing on the next one. It used to
  overflow one value early (design.md § Ranges, B-2026-11-05-20).
- A tuple of integer literals takes its slot's integer types: as a key of a
  narrow-tuple collection (`s.contains((1, 2))` on a `SortedSet[(i32, i32)]`,
  `m.get((1, 2))`, `v.contains(..)`) it was built as `(i64, i64)` and never
  found on compiled code, and `a == (1, 2)` with `a: (i32, i32)` stopped
  with invalid MIR (design.md § Numeric literals, B-2026-11-04-82).
- An assignment whose place starts with a call, or is a `Map` index with a
  `Copy` key, evaluates the place before the value (§8.2; B-2026-11-01-16,
  -28, -29).
- A temporary in a `match` guard drops at the end of the guard (§7,
  B-2026-11-01-30).
- Cancelling a task while it waits inside a `par` no longer leaks
  (B-2026-11-02-1).
- `let x: i64 = None?;` no longer stops `karac run` with invalid MIR
  (B-2026-11-01-31).
- Trait calls resolve through the bound and the named trait (design.md §9).
  `T.label()` under `T: Named` calls `Named`'s function, not an inherent
  one of the same name (B-2026-11-04-9). UFCS on a program trait
  (`A.m(s)`) is accepted and calls `A`'s impl, also when a second trait
  has an `m` and in a generic body under `T: A + B` (B-2026-11-04-8).
  `x.m()` under one bound no longer fails when the type has another trait's
  `m` (B-2026-11-04-11). A program's associated function named `name` (or
  any other reflection method) is no longer run as reflection
  (B-2026-11-04-10).
- Struct fields follow the three visibility levels of design.md §5: a field
  with no keyword is visible in its whole package, `private` limits it to
  files in its directory, and `pub` exports it. 0.8.0 refused an unmarked
  field outside its own file and did not parse `private` on a field
  (B-2026-11-04-22).
- Of several `From` impls on one type, `T.from(x)` calls the one `x`'s type
  names; it called the first whatever the argument (B-2026-11-04-47).
- A struct literal that names a field not visible where it is written (a
  `private` field from another directory) is refused, `E0111`, as a read of
  that field already was (B-2026-11-04-46).
- `karac fmt` keeps the parentheses of `x @ (1 | 2)`; it wrote
  `x @ 1 | 2`, which binds `x` in the first alternative only.
- `let mut f = f;` rebinds an `own MutFn` parameter, the spelling design.md
  §7 prescribes; it was refused as storing a non-escaping value
  (B-2026-11-04-62). A closure that prints a capture is an `Fn`, so it may
  escape into an `Fn` slot (B-2026-11-04-65).
- A default parameter value may name a function, `g: Fn(i64) -> i64 = id`
  (B-2026-11-04-68), and `v == []` gives `[]` the element type of `v`
  (B-2026-11-04-75).
- `assert`, `assert_eq` and `assert_ne` borrow their operands, so a `Vec`
  or `String` compared by `assert_eq(v, e)` is still usable afterwards; it
  was moved, with a `.clone()` fix (B-2026-11-05-8).
- Definite assignment ignores a branch that diverges: `if k > 0 { x = k; }
  else { return 0; }` (or `panic`, or a panicking `match` arm) leaves `x`
  assigned, and a `loop` whose only `break` follows the assignment inside
  an `if` does too (B-2026-11-05-22).
- `freeze r` is accepted after a value built in place is pushed into a
  field of `r` (`r.kids.push(Nd { .. })`), which makes no second handle
  (B-2026-11-05-6).
- In a trait's default method, `self.m()` calls that trait's `m` in every
  impl the default is copied into, even when the type implements another
  trait with an `m`; it was refused as ambiguous (B-2026-11-04-40).
- **Miscompiles.** A `Map` index place, or a place rooted in any call,
  evaluates before the assigned value, as core-semantics §8.2 says
  (B-2026-11-01-28, -29); `mem.swap` and `Option.replace` on `Fn` values
  holding closure literals (B-2026-11-02-19); `ref`-element natives such
  as `Vec[ref String].fill` and `SortedSet[ref String]` iteration
  (B-2026-11-02-38); `#[target]` on impl members, fields and variants
  (B-2026-11-06-22); a generic function passed as an `Fn` value
  (B-2026-11-07-2).
- **Crashes.** `BufReader.read_line` / `read_to_string` into a string
  that started as a literal segfaulted (B-2026-11-07-19); `Client.get` /
  `post` under a `Network` fake (B-2026-11-03-18); a string or integer
  literal stored into a `ref` slot (B-2026-11-02-37); `m.get_or(k, "lit")`
  on a `Map[String, String]` (B-2026-11-05-38); parallel assignment
  `a, b = b, a` in a package panicked the compiler (B-2026-11-05-19);
  deeply nested blocks ran the parser out of memory (B-2026-11-02-25);
  `mem.replace` on an `Fn` local (B-2026-11-02-16); a private type named
  like a library one (`struct Sender`, B-2026-11-07-6).
- **An early exit in `par {}` cancels the branches after it**, as
  core-semantics §11.7 says; they ran to completion. They are torn down at
  their next wait, running their `defer`s (B-2026-11-07-38).
- **Accepted by check, refused at run or build, now run.** Mixed-width
  compound assignment (`total_i64 += n_i32`, B-2026-11-04-59); or-patterns
  binding under a guard and two other pattern shapes (B-2026-11-04-57);
  `self.raw()` on a borrowed `distinct` value (B-2026-11-04-48); a trait
  default method reached through a blanket impl (B-2026-11-04-49); an
  impl on a type alias (B-2026-11-04-50); cloning a `Vec` of `Fn` values
  (B-2026-11-03-3); a receiver of `Fn + CrossTask` values iterated with
  `for` (B-2026-11-04-4); a closure over dotted unit variants stored as
  `Fn` (B-2026-11-07-13); provider-writing closures as `Fn` values and in
  runtime-run closures (B-2026-10-30-3, -5).
- **Interpreter and compiled code agree.** A negative capacity
  (B-2026-11-03-15); a non-HTTP URL scheme is refused by the compiled
  client too (B-2026-11-07-28); requests through a CONNECT tunnel
  (B-2026-11-07-29); `karac test` accepts what `karac check` accepts
  (B-2026-10-30-4).
- `match (self, other)` in a `ref self` method is no longer refused as a
  move (B-2026-11-07-51). `impl Ord` without `PartialOrd` offers the missing
  impl as a fix (B-2026-11-07-52), and `Dict` suggests `Map`
  (B-2026-11-07-53).
- About 40 more programs 0.8.0 wrongly refused are accepted, most of them
  borrow-check and effect-check false positives (moves after iterator
  chains, disjoint field writes in tasks, closures per branch, operator
  impls and `std.*` imports in a package). The ledger rows are the list.

### Diagnostics

- `cannot infer const parameter 'N'` no longer suggests call-site generic
  arguments (`f[..., 8](...)`), which design.md §8 rules out; it asks for
  an annotation on the binding (B-2026-11-06-14).
- **E0400 explains itself and `karac fix` declares the effects.** Its JSON
  record carries `inferred_effects`, `declared_effects` and a `derivation`,
  the call chain that brought in each undeclared effect (design.md §17
  rule 5). Its fix adds the effects to the `with` clause, or writes the
  clause, so `karac fix` resolves it. A non-exhaustive match on an enum, a
  bool or a tuple of them is fixed with every missing arm in one edit, not
  one arm per run. `karac query effects` and `query ownership` answer with
  a `derivation`, and `karac query queries` with no target reports the
  package in the current directory (B-2026-11-05-29).
- **`karac query` and `karac explain <file>.<item>` see the program `karac
  check` sees.** A program naming a library type (`WsMessage`, as the `karac
  new` scaffold does) was refused with `undefined type`. Both now run with
  the library and the core rules, and answer about the program's own
  functions only, so a bare parameter reads as the borrow it is and a loop
  over a borrowed `Vec` gets its layout query (B-2026-11-06-5).
- `karac query queries`, `query cost-summary` and `query ownership` report
  no RC fallback. Core semantics §6.4 removed it, and where it would have
  fired `karac check` reports the move, so the `rc_fallback_decision`
  (keep_rc / prefer_rc), the `rc_ops` count and the `rc_values` list it
  showed described sharing no build performs (B-2026-11-06-9).
- `#[allow(unfulfilled_lint_expectation)]` on a function or impl covers an
  `#[expect]` on a statement inside it; `-A` / `-W` / `-D` / `-F` naming no
  lint warns `unknown_lint`, as an attribute does; an unknown lint and an
  unfulfilled expectation print one level in their label; and `karac
  explain` answers the spec's symbolic codes (`E_NOT_CROSS_TASK`,
  `E_DUPLICATE_LINT_LEVEL`, ...), the last now with its own code, E0006
  (B-2026-11-05-17).
- `--output=json` and `--output=jsonl` stop a file at 20 primary errors and
  report the rest in a `truncated` marker (design.md §17 rule 3), and
  `--emit-timings=<file>` writes the phase times there as JSON
  (B-2026-11-05-17).
- `karac check --output=jsonl` streams every diagnostic the batch
  `--output=json` document lists, as `diagnostic` events with the same ids
  at each phase's boundary; it printed only parse errors, and those without
  a `type`. `phase_complete` counts errors, warnings and notes from them,
  `build_complete.total_errors` is their sum, a skipped phase names the
  errors that blocked it, and `build_start` carries a `build_id`, a
  timestamp and the arguments (B-2026-11-05-13).
- `karac check --output=jsonl | head` and `karac test | head` end quietly
  with status 141 when the reader goes; they panicked on the closed pipe
  (B-2026-11-05-9, B-2026-11-05-13).
- A failed `assert_eq` / `assert_ne` in `karac test` reports its `left:`
  and `right:` values in the `test_fail` message, and both refuse operands
  whose type has no `Debug`, which they could not print (design.md §14,
  B-2026-11-05-9).
- E_NOT_CROSS_TASK on a value that only contains a `shared` handle names
  its type as such and adds a note with the field path to the handle
  (`at field path inner.cache`); it called the value "a `shared` handle"
  (B-2026-11-05-6).

- `#[track_caller]` on a program's own function now works: a panic inside
  it, or inside a tracked function it calls, is reported at the call into
  it, on stderr and in `TaskFailed.Panicked`'s `location` (design.md §10,
  B-2026-11-04-20). It was accepted and ignored.
- `#[gpu]` and `comptime` outside a `#[derive]` backer draw the
  `later_track` warning (W0292): they run with today's meaning, but are not
  covered by 0.8's compatibility promise (§13, B-2026-11-01-41).
- Fewer false refusals: `par` inside a task (B-2026-11-01-37), closures that
  write a field through a `mut ref` parameter (-34), `Copy` arguments a
  later argument writes (-35, -38), handles bound out of a `ref` scrutinee
  (-3).
- §11.2 now states that v1 has no index-disjointness check: two `par`
  branches writing elements of one `Vec` conflict; split it with
  `split_at_mut` (B-2026-11-01-40, relocated to `deferred.md`).
- `return` inside a `par` branch or `par for` body is accepted, per
  core-semantics §11.7 (B-2026-11-02-35).
- Mixed-width operands are accepted: the narrower operand of an arithmetic
  or bitwise operator widens within its kind (`u8 + i64`, `f32 + f64`), per
  design.md § Mixed-width operands. Integer with float, and a pair where
  neither type holds the other (`i32 + u32`), are still refused. A cast the
  widening makes redundant (`u as i64 + w`) draws `lossless_cast`, and
  `karac fix` deletes it (B-2026-11-03-12).
- A literal returned from a closure inside `Ok` / `Err` / `Some` solves the
  callee's type parameter: `c.or_else(|e| Err(0))` is accepted
  (B-2026-11-03-14).
- `vec![a, b]` is a deprecated spelling of the list literal: `karac check`
  warns (`deprecated`) and `karac fix` rewrites it to `[a, b]`, `vec![v; n]`
  to `Vec.filled(n, v)` and `vec![]` to `Vec.new()` (design.md § Collection
  literals). It still compiles as before.
- More Rust spellings carry a `karac fix` edit that reaches a program that
  parses: generic parameters in angle brackets on a declaration
  (`struct Bx<T>`, `impl<T>`, B-2026-11-04-6), `&[T]` / `&mut [T]` as
  `Slice[T]` / `mut Slice[T]` in one edit, `Box<dyn Fn(A) -> R>` as the
  function type `Fn(A) -> R` (B-2026-11-04-7), and `Box::new(e)` as `e`,
  which `karac fix` used to rename to `Hex.new(e)` (B-2026-11-04-43).
- In a multi-file project, a type error names a type from another module by
  its path (`a.P`, not the internal `P__a`), quotes the file's own line and
  puts the caret under it (B-2026-11-04-24). An expected type is no longer
  shown as a borrow (`'Map[..]'`, not `'ref Map[..]'`) when the borrow is not
  what differs.
- A field read from where it is not visible is coded `E0111
  PrivateItemAccess`, as design.md §4 says, not `E0221` (B-2026-11-04-46).
- A `for` over a borrowed iterator parameter (`it: I` or `it: mut ref I`
  with `I: Iterator`, or `it: impl Iterator[..]`) is refused at `it`: the loop
  consumes an `Iterator`. `karac fix` writes `own` on a read-only parameter
  and `it.by_ref()` for a `mut ref` one. It used to type the loop variable as
  the iterator (B-2026-11-04-17). The item of `it.by_ref()` under
  `I: Iterator[Item = i64]` is `i64`, not `I.Item`.
- A `let` keeping a borrow of a `Vec[..]`, `vec![..]`, repeat or map
  literal's temporary (`let g = Vec["a"].last();`) is refused at the
  statement with the §5.5 temporary rule, as the list literal already was;
  it was refused only later by the MIR borrow check (E0516), which another
  error could hide (B-2026-11-04-45).
- `let w = [];` with no type names the problem
  (`E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION`) and `karac fix` writes
  `Vec.new()`; it was refused with an internal "not lowered yet" message
  (B-2026-11-04-44). A `for` over a borrowed concrete iterator
  (`it: VecIntoIter[i64]`) gets the borrowed-iterator diagnostic and fix
  above instead of the same internal message.
- A list literal builds a `Set` where one is expected:
  `let s: Set[i64] = [1, 2, 2, 3]` and a `Set` argument, per design.md §5
  Collection literals (B-2026-11-04-58).
- An alias whose bound a type argument does not meet reports one E0262
  naming the whole bound, `'f64' does not satisfy 'T: Eq + Hash' required by
  alias 'Counts'`, and a bound the aliased type already requires
  (`type Sk[T: Ord] = SortedMap[T, i32]`) draws `redundant_alias_bound`
  (W0293). design.md names E0262 and E0263 (B-2026-11-04-60).
- An inherent `m(self)` called as `s.m()` borrows `s` even when a trait's
  `m(own self)` is also implemented for the type; the ownership check
  charged the trait's mode (B-2026-11-04-12).
- `karac fmt` prints a collection literal as written: it keeps `vec![..]`
  only where the source used it, and never turns `Vec[..]` or `[..]` into
  `vec![..]`.
- A variant pattern in a parameter (`fn f(Some(x): Option[i64])`) is
  reported as a refutable parameter pattern, and `srt[i64]` as a use-site
  type argument, instead of naming-class and cast advice (B-2026-11-04-69).

### Performance

- Each task keeps its own borrow flags instead of one process-wide locked
  table (B-2026-11-01-25); short counting loops that call a leaf fully
  unroll (B-2026-10-31-44); `unsafe { xs.get_unchecked(i) }` is not
  bounds-checked (B-2026-10-31-45).
- An auto-parallel loop that pushes exactly once per iteration writes its
  result in place instead of joining per-task chunks, halving peak memory at
  two or more workers (B-2026-11-02-8). `shared` types that no `weak` names
  carry an 8-byte count header (B-2026-11-01-24). String building, integer
  formatting and `Vec` fills presize (B-2026-11-02-4, -6, -9, -11;
  B-2026-11-01-21; B-2026-10-31-42).
- A `for i in a..=b` loop now stops on its header's `i <= b`, and steps
  unless `i` is its type's largest value. Before, the loop also tested
  `i == b` before each step. LLVM can now see that `v[i]` stays inside a
  `b + 1` long vector, so it drops that bounds check (B-2026-11-04-39).

---

## 0.8.0 (beta) — 2026-11-01

The first release, and a beta: 1.0 follows outside feedback (decided 2026-10-31). It is the first release of the v2 compiler. Every program lowers to MIR, which the
interpreter (`karac __mir-run`) and the LLVM backend (`karac build`, `karac
run`) both execute; the legacy pipeline was deleted at M3 (2026-10-24).
Normative rules are in [docs/core-semantics.md](docs/core-semantics.md), the
rest of the language in [docs/design.md](docs/design.md), and what waits for
later in [docs/deferred.md](docs/deferred.md).

### Language

- **Ownership without lifetimes.** Parameters borrow by default; `own T`
  takes ownership and `mut ref T` writes. A returned view's source is inferred
  from the signature. `shared struct` / `shared enum` are reference-counted.
- **Effects.** Declared on public functions, inferred for private ones, and
  checked against `par { }`, `par for` and `TaskGroup`. Independent
  statements and loops are parallelised automatically where the effects prove
  it safe (`KARAC_AUTO_PAR=0` opts out).
- **Effect variables** (`[with E]`) for stored callbacks: a struct or enum
  declares one, solved from what is bound to it within a function.
- **`dyn Trait`**: a two-word, never-null owned handle with no `Box`, coerced
  only where a `dyn` type is expected; effects through its `with` clause;
  `dyn T + CrossTask` to cross tasks. No upcasts or downcasts yet.
- **Providers.** `effect resource R: Trait`, `with_provider`, `providers { }
  in { }` and `#[with_provider]` on tests swap a resource's implementation with
  no mocking framework. A missing provider is a compile error. The file system,
  network (HTTP client and TCP), standard streams, clock, environment and
  random source can all be overridden; a program may declare its own
  program-rooted resources with defaults.
- **Concurrency.** Stackless coroutines on a multi-threaded work-stealing
  scheduler; `TaskGroup` with panic isolation, deadlines, channel `select` and cooperative
  cancellation; blocking-style I/O with no `async`/`await`.

### Standard library

- HTTP/1.1, TLS and WebSocket servers and an HTTPS client; files, processes
  and environment; `String`, `Vec`, `Map`, `Set` and their sorted forms;
  iterators; JSON via `Serialize`.

### Tooling

- `karac check --output=json` with machine-applicable fixes, applied by
  `karac fix` (also over a whole package); `karac query` for effects,
  ownership, concurrency, costs and monomorphisation; `karac explain`.
- `karac fmt` (one canonical format, `--check`, package mode), `karac test`
  (colocated `_test.kara` files), `karac new` (`--backend`, `--cli`, `--lib`)
  and `karac init`, `karac lint --list`.
- A VS Code extension with the language server bundled, one `.vsix` per
  platform.

### Targets

- Linux x86_64, Linux aarch64 and Apple-silicon macOS. Windows follows in a
  later release. Plain `wasm32` (`--target=wasm_wasi`) for compute programs.

### Measured before release

- The corpus of runnable programs with expected outputs is the gate, on the
  interpreter, the compiled backend and an AddressSanitizer lane.
- Run time and build time against the earlier compiler, binary size, an
  automatic-parallelism speedup floor, and a load test of 100,000 idle
  connections with tail-latency limits.

### Known limits in 0.8

- A callback stored in a field cannot use an overridden provider
  (core-semantics §14.5); test it by calling it directly.
- Under a `Network` override, TLS, WebSocket, UDP and `Server.serve` are
  refused at check (§14.12).
- An HTTP response with no headers is sent with a JSON `content-type`, and
  standard output to a pipe is block-buffered.
- Struct `invariant` clauses belong to the later verification track and are
  refused at check (`E_DEFERRED_TRACK`); enforce the property in the type's
  constructor. `requires` and `ensures` (with `old(...)`) work.
- A C import (`unsafe extern "C"`) passes integers, `bool`, floats and raw
  pointers; `#[repr(C)]` structs do not cross yet, and exporting a C ABI
  (`pub extern "C" fn`, `--crate-type`) comes in a later release.
- Two task-safety holes that `karac check` does not yet catch, both fixed
  in 0.8.1: a closure holding a `shared` value can cross a task boundary
  when it is stored in a struct field, collection, `Option`, `dyn` or
  channel, or erased to a plain `Fn` type (core-semantics §6.3,
  B-2026-11-01-26); and a task spawned inside `with_provider` onto a
  `TaskGroup` made outside it can outlive the provider (§14.5,
  B-2026-11-01-27). Until then, keep `shared` state out of stored closures
  that tasks run, and create a `TaskGroup` inside the `with_provider` its
  tasks use.
- A few evaluation-order corners differ from core-semantics §8.2: `m[k] = v`
  into a `Map` reads a place key after the value (B-2026-11-01-28), and an
  assignment whose target starts with a call (`at(mut s).n = v`) evaluates
  `v` first (B-2026-11-01-16, -29). A temporary in a passing `match` guard
  drops at the end of the statement, not the guard (B-2026-11-01-30).

---

## [Unreleased] — state as of 2026-08-15

The compiler is far past the parser milestone recorded below. Working today:

- **Full pipeline:** lexer → parser → resolver → typechecker → effect checker →
  ownership checker, with three execution backends — a tree-walk interpreter,
  an LLVM 18 AOT backend (`karac build`), and an LLJIT engine (the `karac run`
  default). `run` == `build` output parity is a tested invariant, as is a third
  surface: the default build auto-parallelizes via effect analysis
  (`KARAC_AUTO_PAR=0` opts out).
- **Language:** effects (declared on public functions, inferred for private),
  tiered ownership (owned → `ref` → RC, `shared struct`/`enum`, RC elision),
  generics `[T]` with trait bounds, pattern matching with exhaustiveness,
  layout blocks (SoA), contracts, refinement/distinct types.
- **Targets:** native (macOS/Linux/Windows CI-tested), `wasm_wasi` (component
  model emission) and `wasm_browser` including a threaded dual artifact, and
  GPU compute — `#[gpu]` kernels compiled to WGSL and dispatched through wgpu
  (`gpu.dispatch`), proven on Metal and Vulkan. The CUDA/NVPTX path is not
  built.
- **Runtime:** static-linked Rust runtime with a tokio-backed event loop
  (TCP/TLS/HTTP/WebSocket), a native work-stealing scheduler, and opt-in
  gpu/regex/arrow archive variants.
- **AI-first tooling:** structured JSON diagnostics (`karac check
  --output=json`), machine-applicable fixes (`karac fix`), and the Mend
  authoring loop that dogfoods both.
- **Self-hosting (in progress):** ~20k lines of Kāra in `selfhost/` (lexer,
  parser, resolver, typechecker, codegen underway), differentially tested
  against the Rust implementation as oracle in CI.
- **Verification:** ~8,900 non-codegen tests, ~3,200 codegen E2E tests, and an
  ASAN/LeakSanitizer fixture suite (~1,100 programs) run at two optimization
  levels across a 26-job CI matrix (x86 + arm64, three OSes).

---

## 2026-06 — language redesign + parser milestone (historical)

### Language Design

- **Complete language redesign.** Replaced the original `fn`/`flow`/`record`/`->` pipeline design with a Rust-inspired systems language featuring:
  - **Effect system** with six built-in verbs (`reads`, `writes`, `sends`, `receives`, `allocates`, `panics`) and user-defined resources
  - **Auto-concurrency** via effect analysis — no async/await, no colored functions
  - **Tiered ownership** — parameter mode inference, owned returns by default, explicit `ref` for borrows, RC fallback with budget controls. No lifetime annotations.
  - **Data layout separation** — logical struct vs physical memory layout (opt-in SoA)
  - **Algebraic data types** — Rust-style enums with exhaustive pattern matching
  - **AI-first compiler interface** — structured JSON diagnostics, compiler query API, canonical formatting
  - **Phased runtime** — v1 blocking I/O, v1.1 network event loop, v2 full hybrid

### Compiler

- **Lexer:** Complete tokenizer for all keywords, symbols, and literals
- **Parser:** Recursive-descent parser with Pratt expression parsing, producing a full AST
  - All expressions: literals, binary/unary operators, function/method calls, field/index access, closures, ranges, casts, `?` operator
  - All statements: `let`/`let mut` bindings with patterns, assignments, parallel/destructuring assignment (`a, b = b, a` — every RHS evaluated before any target is written, so it swaps), expression statements
  - All items: functions, structs, enums, traits, impl blocks, effect declarations, layouts, modules, imports, constants, type aliases, extern functions, alias/independent declarations
  - Effects syntax: resource declarations, effect groups, `with`/`with _`, transparent effects, parameterized resources
  - Ownership types: `ref`, `mut ref`, `weak`, pointer types
  - Pattern matching: wildcards, bindings, literals, struct/tuple destructuring, qualified paths
  - Generics with trait bounds
  - Attributes with arguments
  - Error recovery: continues parsing after errors, reports multiple diagnostics with spans
  - 89 parser tests + 27 lexer tests = 116 total tests
- **AST:** Complete node types with span tracking on every node

### Documentation

- **design.md:** Complete language specification with all committed features
- **syntax.md:** Parser implementer's grammar reference
- **roadmap.md:** 10-phase implementation plan aligned with current design

### Next Steps

- Implement semantic analysis (Phase 3): name resolution, type checking, effect inference, ownership analysis
