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

## 0.8.1 — unreleased

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
