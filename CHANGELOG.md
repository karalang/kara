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

- A `?`, a `return`, or a `break` or `continue` whose loop encloses the
  block is an error in a `defer` or `errdefer` block, one diagnostic for the
  four: `a defer/errdefer block cannot leave its scope early (`?`, `return`,
  `break`, `continue`)` (design.md §10 Rules, B-2026-11-14-19). `karac check` and
  `build` died of a stack overflow on every one but `errdefer { return .. }`,
  which passed check and failed in the compiler's MIR stage. A loop or
  labeled block written inside the block still exits itself.

- A `test "..." { }` block in a package file that is not a `_test.kara`
  file is an error, `E_TEST_OUTSIDE_TEST_FILE`, naming the companion file
  to move it to: `karac test` runs only the blocks in `_test.kara` files,
  so it never ran (design.md §14 Testing, B-2026-11-13-76). A lone file
  outside a package is unchanged.

- `assert`'s condition must be a `bool`, and `assert_eq` / `assert_ne`
  operands are checked as `a == b` is: `assert(1)` and
  `assert_eq(1, "a".to_string())` passed check and then failed in the
  compiler's MIR stage (B-2026-11-13-36). Any count of arguments other than
  the operands and an optional message is an error.

- A `MutFn` stored in a struct field or a collection is called only through
  a unique access, as a local one is (core semantics §9.6). `(self.f)()`
  in a method with a bare `self`, `(h.f)()` on a bare parameter, and
  `(h.f)()` / `(fs[0])()` on a plain `let` were accepted, and the closure's
  state changed through each shared path. They now need `mut ref self`,
  `h: mut ref Holder` or `let mut`, which `karac fix` writes; such a call
  also counts as a use of the `let mut`, which drew `unused_mut`
  (B-2026-11-13-3).
- A method with a `self` or `mut ref self` receiver returns a borrow of
  `self` only (core semantics §5.4), whatever the result's type. Only
  `ref T` and `Slice[T]` arguments were held to it, so
  `fn pick(self, other: Str) -> Str { other }` was accepted, and so were
  `Option[ref T]`, `Option[Str]`, `Result[Str, E]`, tuple, `Vec[Str]` and
  view-struct arguments returned or wrapped (`Some(o)`, `(o, 1)`). A caller
  that kept the result past the argument read freed memory. They are now
  refused at the return; make the method an associated function, whose
  result borrows its view parameters (B-2026-11-13-31).
- `SortedMap.floor` and `ceiling` return `Option[(ref K, ref V)]`, the entry
  itself as docs/library/collections.md gives it and as `first` and `last`
  already did, not a copy typed `Option[(K, V)]`. A binding of the key is a
  `ref` into the map, so the map cannot be written while it is live: kata
  455's `let Some((size, _)) = cookies.ceiling(want)` followed by
  `cookies[size] -= 1` now takes the key out as `let size = *size;`. A
  helper declared over `Option[(K, V)]` takes `Option[(ref K, ref V)]`
  (B-2026-11-13-25).
- A `u128` literal past `i128::MAX` in another integer context is out of
  range: `let z: i128 = 170141183460469231731687303715884105728u128` was
  accepted and held `i128::MIN`, and `-340282366920938463463374607431768211455u128`
  was accepted and held `1`. The first is now "out of range for 'i128'", the
  second "cannot initialize unsigned type 'u128'" (B-2026-11-13-28).
- A registry dependency with no configured proxy is refused with error
  `E_REGISTRY_DEP_UNSUPPORTED` by `karac check`, `run`, `build` and `test`
  (B-2026-11-13-41). `build` and `test` warned and went on without it, so
  its first import was "unknown module"; `check` and `run` said nothing.
  `check` and `run` now also fetch a git dependency as `build` does
  (`E_GIT_FETCH_FAILED` when they cannot), the bare package `karac check`
  reports a cycle among path dependencies as `E_DEPENDENCY_CYCLE` rather than
  E0112 at the import, and every command warns of an unknown `[package]`
  key.
- A `shared` value held by a plain `let` cannot be lent as `mut`: a `mut`
  marker on the bare binding lends the binding itself, and a callee's
  `*t = n` replaced the object the `let` named. `advance(mut tail)` with
  `let tail = T { v: 1 }` was accepted; it now needs `let mut tail`, and
  `karac fix` adds the `mut` (design.md § Call-site `mut` markers,
  B-2026-11-10-22). A marker on a field or element of the value
  (`bump(mut t.v)`) still writes through the handle and needs none.
- A method that writes its receiver needs `let mut` on it for every library
  type, not only those the ownership pass's own table listed:
  `w.push_back(1)` on `let w: VecDeque[i64]` and `r.read_line(s)` on a plain
  `let r` reader were accepted. The fix is `let mut` (B-2026-11-09-101).
- `BufReader.new(x)` / `BufWriter.new(x)` (and `with_capacity`) over
  anything but a `File`, a child's pipe, a `TcpStream` or a `TlsStream` is
  refused with E0297. A `BufReader` over a `TcpStream` passed check, then
  failed under `karac run` ("of a closed file") and was freed as a file by a
  compiled build (B-2026-11-10-14). A generic wrapper
  (`fn wrap[R](r: own R) { BufReader.new(r) }`) is judged at each call that
  instantiates it, and the call that makes it buffer a non-source is refused
  there (B-2026-11-10-28).
- A program's impl of a library trait for a library type that the library
  already implements is refused as an overlap, naming the library's impl
  (design.md § Coherence: the application is exempt from the orphan rule,
  not the overlap rule). `impl Clone for String { .. }` was accepted and
  replaced the library's `clone` (B-2026-11-09-31).
- A float handed to an integer slot through a borrow is refused as it is by
  value: `Vec[u8].contains(d)` or `VecDeque[u8].contains(d)` with `let d =
  1.5`, and a `ref u8` or `mut ref u8` parameter given a float. The
  interpreter stopped on comparing an integer with a float and the binary
  printed `false` (B-2026-11-09-19).
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
- A tuple or an array held in a collection keeps its numbers' types, as a
  number held there does: a `Vec[(i64, i64)]` handed to a
  `Vec[(i64, u64)]` parameter was accepted, and the interpreter printed `-9`
  where the compiled binary printed `18446744073709551607`. A map whose key
  was an open literal let `Map[?, i64]` pass for a `Map[i64, u64]` the same
  way (B-2026-11-08-89).
- The deferred `std.cli` parser's `Parser`, `Arg`, `Args`, `CliError`,
  `Subcommand` and `SubcommandResult` are reached through `import std.cli`
  (deferred.md § std.cli), as `std.regex` and `std.json` are: `karac check`
  refuses a bare use, with the import as its fix, and `karac build` and
  `run` are unchanged. A program's own `enum CliError` or `struct Parser`
  no longer draws a `prelude_shadow` warning on every command
  (B-2026-11-08-29).
- A bare `collect()` kept by a `ref` or `mut ref` binding
  (`let q: ref Vec[i64] = it.collect();`) is refused as a reference to a
  temporary that outlives its statement (core-semantics §5.5), as
  `let q: ref Vec[i64] = make();` already was. It was accepted and the
  interpreter then stopped on `deref of non-reference` (B-2026-11-08-43).

### Deprecated (errors at the next minor release)

- An argument at a method's `mut ref` parameter takes the call-site `mut`
  marker, as a free function's already does (design.md § Call-site `mut`
  markers): `r.fill(s)` with `fn fill(ref self, buf: mut ref String)` warns
  `deprecated` and `karac fix` writes `r.fill(mut s)`. A `mut ref` or
  `mut Slice` forwarded from a parameter or binding needs no marker, and
  `#[allow(deprecated)]` on the enclosing item silences it. The corpus and
  the katas were migrated with `karac fix`, outputs unchanged. The next
  minor release refuses the unmarked form (B-2026-11-09-100).
- `concat()` on a `Vec` or `Slice` whose elements are neither sequences
  nor text, which joined their `Display` forms, warns `deprecated`;
  `karac fix` writes `join("")`, which prints the same. `#[allow(deprecated)]`
  on the enclosing item silences it. The next minor release refuses it
  (design.md § Vec; B-2026-11-11-51, tracker B-2026-11-11-81).

### Added

- A struct or enum takes `const` parameters (design.md § Const generic
  parameters; B-2026-11-13-80): `struct Buf[const N: i64] { a: Array[i64, N] }`,
  written `Buf[3]`, `Buf[2 + 1]` or `Buf[SIZE]`, one type per value, served
  by `impl[const N: i64] Buf[N]`, where `N` is a value. Every argument on a
  user type was refused as "not yet supported in this slice".
- A function's result borrows what a view parameter views (core-semantics.md
  §5.4; B-2026-11-13-79): with `struct Cursor { src: Str, pos: i64 }`,
  `fn parse_until(c: mut ref Cursor, stop: char) -> Str` returns a view of
  the text `c.src` views, so two results can be live while `c` advances, a
  method on `Cursor` likewise. The second call was E0516. Returning a view of
  a field the parameter owns (a `String`) is now E0509 at the return; a
  `ref` written in the result keeps the borrow of the parameter.
- `TcpStream.read_to_end` and `TlsStream.read_to_end` append everything up
  to the end of the stream and return the count (docs/library/io.md;
  B-2026-11-13-73). A connection that carries one payload and closes, such as
  an FTP data connection, needed a hand-written `read` loop.
- A workspace root may be virtual: a `kara.toml` with `[workspace]` and
  `[workspace.dependencies]` and no `[package]` (design.md § Workspaces;
  B-2026-11-13-50). `karac build`, `check` and `test` at it run in each of
  `[workspace].members`, and a member's `workspace = true` dependencies come
  from it. A `path` in `[workspace.dependencies]` is relative to the root
  that declares it; it was joined to the member's directory, so a root's
  `path = "util"` was not found from `core/`.
- `div_euclid` and `rem_euclid` on every integer width and on `f32` /
  `f64`, as Rust's (design.md § Division, remainder and shifts). They
  existed only on `i64`; library/core-types.md now lists them, and
  `abs_diff` (B-2026-11-13-5).
- `assert(cond, msg)`, `assert_eq(a, b, msg)` and `assert_ne(a, b, msg)`
  take an optional `String` or `Str` message, evaluated only when the
  assertion fails; it replaces the `assertion failed: ...` line, and the
  operand lines follow (design.md §14 Assertions, B-2026-11-13-36).

- `String.normalize(form)` and `std.regex` build for `--target=wasm_wasi`. They
  passed `karac check --targets=wasm_wasi` and then failed in wasm-ld on an
  undefined `karac_unicode_normalize_mir` / `karac_regex_mir`. A wasm link now
  takes the opt-in wasm archive the object's symbols name
  (`libkarac_runtime_wasm_unicode.a`, `libkarac_runtime_wasm_regex.a`, and the
  `_threads_` pair), as a native link takes the native ones, and a missing one
  fails with its build recipe (B-2026-11-13-1).
- A script inside a package but outside its `src/` (`app/scripts/s.kara`)
  gets the package's dependencies under `karac run`, `check` and `build`,
  wherever it is run from (design.md § 4, Finding the package;
  B-2026-11-13-30). It was refused with "this file belongs to no package".
  The package's own `src/` modules are not part of the script's program.
- `Duration` has units below a millisecond: `Duration.us(n)` and
  `Duration.ns(n)` build one, and `as_us()`, `as_ns()` and `as_secs()`
  read one (library/time-random-env.md; B-2026-11-11-33). `sleep` of
  part of a millisecond now pauses that long; it used to round down to
  no pause. `karac fix` rewrites Rust's `from_micros`,
  `from_nanos`, `as_millis`, `as_micros` and `as_nanos` to them.
- `Duration` adds, subtracts and scales: `a + b`, `a - b`, `d * n`, `d / n`
  and their compound assignments (library/time-random-env.md;
  B-2026-11-11-72).
- `concat()` on a `Vec[Vec[T]]` or `Slice[Vec[T]]` flattens it, as Rust's
  does: the `Vec[T]` of every element's items in order, so
  `[a, b].concat()` builds one buffer from two (design.md § Vec,
  library/collections.md; B-2026-11-11-51). It was refused, since a `Vec`
  has no `Display` to join. A sequence of `String` or `Str` still gives one
  `String`.
- `String.from_utf8_lossy(bytes)` decodes bytes that may not be UTF-8,
  replacing each invalid sequence with U+FFFD, as Rust's does
  (library/strings.md; B-2026-11-11-24). Bytes read off a socket or a
  file now decode without a `Result` to unwrap.
- `std.cmp.Reverse[T]` orders a value the other way round
  (library/core-types.md § Reverse): `import std.cmp.Reverse;`, then
  `Reverse { value: score }` on one part of a heap's item or a sort key
  orders that part descending and the rest ascending, as
  `(Reverse { value: p.score }, p.name.clone())`. It is a struct with one
  field, `value`, and implements `Ord` by `T`'s order reversed, and `Eq`,
  `Hash`, `Clone`, `Copy`, `Debug` and `Display` when `T` does
  (B-2026-11-10-41).
- `read_exact(mut buf)` on `TcpStream`, `TlsStream`, `File`, `ChildStdout`,
  `ChildStderr` and `BufReader` reads until `buf` is full, as Rust's does:
  the usual way to read a binary protocol's fixed-size header
  (`conn.read_exact(mut head)?`). An early end is `UnexpectedEof`, which
  `NetError` gains for it (B-2026-11-10-44). A program that matches every
  `NetError` variant needs the new arm. A `BufReader`'s takes the bytes
  it holds first (B-2026-11-11-63).
- `impl From[NetError] for IoError`: a function that uses `?` on file and
  socket operations alike can return `IoError` alone, as Rust's
  `io::Result` does; it was refused "cannot convert error 'NetError' to
  'IoError'" (B-2026-11-11-25). Same-named variants keep their name, and
  the rest become `Other` with the error's display.
- `Iterator.eq(other)` compares two iterators item by item, as Rust's
  does: `digits.iter().eq(digits.iter().rev())`. Both must yield one item
  type. It was refused with "`VecIter` does not implement `PartialEq`"
  (B-2026-11-10-26). A trait default whose bound ties another iterator's
  item to `Self.Item` (`J: Iterator[Item = Self.Item]`) now checks in
  every impl it is copied into, a program's own traits included. Before,
  `Self.Item` was left unresolved there and `==` between the items was
  refused.
- `Result.unwrap_or_default()` gives the value, or `T`'s default on `Err`,
  as `Option.unwrap_or_default` does on `None`:
  `String.from_utf8(bytes).unwrap_or_default()` (B-2026-11-10-46).
- `Option.as_deref()` views an `Option[String]` as an `Option[Str]` and an
  `Option[Vec[T]]` as an `Option[Slice[T]]`, without moving or allocating
  (library/core-types.md): `req.header("X-Token").as_deref() == Some(TOKEN)`.
  The view borrows the option, so it cannot outlive it (B-2026-11-11-3).
- `BufReader` and `BufWriter` wrap a `TlsStream` and a socket a `Network`
  provider opened, on `karac run` and compiled builds alike: a buffered read
  over TLS waits inside its task, the handshake included, and a TLS failure
  is an `IoError`; over a provider's socket the buffer calls the fake's
  `read_stream` / `write_stream`, where the interpreter refused at the
  constructor and a compiled build exited 101 (B-2026-11-10-28).
- `BufReader` and `BufWriter` wrap a `TcpStream` (io.md § Buffered I/O):
  `BufReader.new(conn).lines()` reads a connection line by line, a read
  waits inside its task as the connection's own does, the calls have the
  connection's effects, and dropping the buffer closes the connection
  (B-2026-11-10-14).
- `drop(x)` in the prelude, an ordinary `fn drop[T](x: own T) {}`: `x`
  moves in and drops at the call, so a `BufWriter` flushes and a file
  closes there. A program's own `fn drop` still wins (B-2026-11-09-72).
- An `HttpError` for a status of 400 or more keeps the response:
  `status()`, `header(name)` (names compared without case) and `body()`,
  so a client can branch on a 409 or read a `Retry-After`. All three are
  `None` for a failure with no response, and for an `HttpError` a program
  or a fake provider builds. The message is unchanged
  (`<url>: status code <n>`) (B-2026-11-09-44).
- `File.write_all`, as `TcpStream`, `BufWriter` and `ChildStdin` have it:
  every byte of the buffer, by `write` until none is left
  (B-2026-11-09-70).
- `Json` implements `Clone`, so a field of a parsed body can be copied out
  of a borrowed tree (B-2026-11-09-48).
- `JsonError`, `SendError`, `RecvTimeoutError`, `SendTimeoutError`, and
  `PoolError[E]` when `E: Display`, implement `Display` and `Error`, as
  design.md §10 says every standard-library error type does, so `?`
  converts them into `AnyError`; `Json.parse(t)?` in a function returning
  `Result[_, AnyError]` was refused (B-2026-11-09-30).

- **Completion and rename in the editor.** `kara-lsp` answers
  `textDocument/completion`: after `.`, the fields and the methods taking
  `self` of the receiver's type, the library's and the builtin types'
  included (an iterator's `map` and `filter` before the file names them);
  after `std.`, the library's modules and their items; elsewhere, the
  locals and parameters in scope and the file's items. `textDocument/rename`
  renames a name at its declaration and every use in the file.

- **Signature help and inlay hints in the editor.** Inside a call's
  parentheses `kara-lsp` shows the callee's declaration with the parameter
  under the cursor, and says for each parameter what its mode means for the
  argument: borrowed (the default, core-semantics §4.1), `own` (it moves in)
  or `mut ref` (marked `mut` at the call). A builtin type's method with no
  Kāra declaration (`Vec.push`, `String.push_str`) is shown as the library
  reference states it, and enum variants and `println` are covered too:
  1,710 of 1,896 calls sampled across the corpus apps get a signature.
  Inlay hints show the inferred type of a `let` written without one and the
  inferred effects of a private function without a `with` clause; both
  come from the check the last edit already ran. In VS Code,
  `kara.inlayHints.types` is on and `kara.inlayHints.effects` off by
  default: the corpus apps carry 37 effect hints per thousand lines, most
  of them `panics` or `allocates(Heap)`.

- **The editor re-checks only what an edit touched.** On a keystroke inside
  a function body, `kara-lsp` checks that body with the rest of the file's
  bodies emptied (the ones it calls kept, or stood in for by a body with
  the same inferred effects), and keeps the previous diagnostics
  everywhere else. A change to a signature, a declaration, what a body
  names, or a private function's inferred effects checks the whole file,
  as does every open and save. Completion checks only the body under the
  cursor. On `mini_markdown` (14,776 lines) an edit's diagnostics take
  0.1 to 0.2 s rather than 1.7 s, and a completion 0.13 s rather than
  1.5 s; a sweep that edits each of the 472 corpus apps at three points
  finds the incremental answer identical to a full check every time.
  `KARAC_LSP_INCREMENTAL=0` turns both off (B-2026-11-09-33).

- **`Slice.split(pred)`** (and so on `Vec` and arrays): the runs between
  the elements `pred` accepts, which are dropped, with Rust's empty runs
  (`[1, 0, 2, 3, 0].split(|x| x == 0)` is `[[1], [2, 3], []]`), so Rust's
  `line.split(|&ch| ch == '#')` carries over through `karac fix`
  (docs/library/collections.md § Slice; B-2026-11-08-47).

- **A block may declare a constant.** `const MOD: i64 = 1_000_000_007;` as
  the first line of a function body, the way a Rust solution writes it, was
  a parse error advising `r#const`. Its scope is the rest of the block; it
  shadows like a `let`; its initializer names constants only, and naming a
  local is refused with that reason (design.md § Constants,
  B-2026-11-08-60).

### Changed

- `s.push_str(x.repeat(n))` and `s += x.repeat(n)`, with `x` a literal or
  an owned `String` other than `s`, append the copies to `s` in place,
  where they built a `String` of the repeat and freed it once appended
  (B-2026-11-12-15). Kata 1138's bench runs in 0.22 s where it took
  0.83 s; Rust's, which builds the repeat, takes 0.60 s.
- An open literal is fixed by its uses whatever order they come in. A
  comparison beside a type that widens into the default (`i < n` with
  `n: i32`) fixes nothing and no longer settles the literal at `i64` where it
  stands, so `let i = 0; if i < n { take_i32(i) }` is accepted as
  `take_i32(i); if i < n {..}` was (B-2026-11-14-1). A value stored into an
  element of an open fill joins its literal, so `let mut t = Vec.filled(3, 0);
  t[1] = 4; let r: i32 = t[1];` is accepted as the other order was, and a
  stored `i32` fixes the fill as `x = nums[0]` fixes a binding, so
  `dist[i] = time[i] * 2` makes `Vec.filled(n, 0)` a `Vec[i32]` with no
  suffix (B-2026-11-12-12). A literal no use fixes still ends at `i64`.
- `for i in (a..b).step_by(k)` over `i64` (and `..=`) is a counting loop,
  where it called the library's `IterStepBy.next` per item, through a
  `first` flag, `advance_by` and two `Option`s (B-2026-11-12-17). Kata
  1144's bench runs in 0.47 s where it took 0.58 s, beside 0.45 s for the
  same loop written with `while` and 0.46 s for Rust's.
- `partition_point` is the halving binary search Rust's is: `base` moves
  to `mid` or stays through a select marked unpredictable, which stays a
  `cmov` on x86, where it branched on every step and mispredicted half of
  them on a random target (B-2026-11-13-34). Kata 1150 runs in 0.35 s
  where it took 0.73 s; Rust's takes 0.37 s.
- `==` and `!=` on two `Vec`s or slices of integers, `bool` or `char`
  compare lengths and then the bytes in one `memcmp`, as `Str` equality
  does; they compared one element per step (B-2026-11-13-33). Kata 1147's
  arm over `text.bytes()` runs in 0.60 s where it took 1.19 s.
- `==` and `!=` between a `Slice[T]`, a `Vec[T]` and an `Array[T, N]`
  compare elements in either order, as Rust's do (design.md § Slices,
  Comparison; B-2026-11-13-17). `a == s` and `v == a` were refused (the
  first with an `.as_slice()` fix) while `s == a` was accepted, and an
  `Array` against a `Slice` of another length is `false`. An ordering
  comparison (`v < s`) still takes `.as_slice()`.
- An unsuffixed integer literal past `i128::MAX` takes `u128` from its
  context, as any unsuffixed literal takes its type:
  `let h: u128 = 170141183460469231731687303715884105728;`, a `u128`
  argument, element or operand, and `let a = <it>; let b: u128 = a;` are
  accepted, where the parser refused them all ("add the `u128` suffix").
  Anywhere else it is out of range for the type it gets, `i64` by default
  (B-2026-11-13-28).
- An attributed block-like statement ends without `;`, as it does without
  the attribute: `#[allow(unused_mut)] { let mut x = 1; println(x); }` and
  `#[allow(x)] if c { .. }` were parse errors (B-2026-11-13-27).
- `set_read_timeout` on a socket a `Network` fake opened returns `Ok(())`
  and changes nothing, since a fake's read never waits; it was
  `Err(Unsupported)`, so a client that sets a timeout could not be tested
  against a fake as it ships. Below 1 ms it is still `InvalidInput`, and
  once the override has ended a compiled program's call is
  `ProviderUnavailable` as the interpreter's was (B-2026-11-09-73).
- **`is_ok()` and `is_err()` on an `Option` are deprecated.** They are
  `Result` methods that an `Option` accepted as `is_some()` and
  `is_none()`; `karac check` now warns (`deprecated`), `karac fix`
  rewrites them, and they become an error at the next minor release, as
  `vec!` and `mut` on a plain field do (docs/library/core-types.md;
  B-2026-11-09-21).

- `Json.deserialize(text: Str)` takes a view, so a line of `text.lines()`
  or a slice reads without `.to_string()`; a `String` still passes
  (B-2026-11-08-79).
- **Fill, align and width pad any value.** `{x:>8}` and `{x:+}` on a float,
  `{flag:>6}` on a `bool` and `{c:^3}` on a `char` were refused ("a float
  format spec needs a precision", "format specifiers apply to int, float,
  and string values only"). A float without `.precision` now pads its
  `Display` text (`{nan:>5}` is `  NaN`, an `f32` its own shortest digits);
  `+` and `0` stay numbers-only, `type` integers-only and `.precision`
  floats-only, each still an error at the hole (design.md §3 Format
  specifiers; B-2026-11-08-74).

- **`Map`, `SortedMap`, `Set` and `SortedSet` compare with `==`.** Two
  maps are equal when they hold the same keys with equal values, two sets
  when they hold the same elements, whatever the insertion or iteration
  order; a struct holding one can derive `PartialEq` (and `Eq`)
  (docs/library/collections.md § Common traits; B-2026-11-08-82). They
  were refused, with a hint to derive `PartialEq` on the library type; a
  refusal on a library or built-in type no longer suggests a derive.
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

- An index evaluates its base before its index when the base is a literal,
  as it already did for a call or a name (design.md §8.1): with `p(n)`
  printing `e{n}`, `[p(1), p(2)][p(0)]` prints `e1 e2 e0`, where it printed
  `e0 e1 e2`, and so do a `vec!` and a struct literal's field. The value was
  right; the order of output, a panic or a drop was not (B-2026-11-14-22).
- A type parameter bounded by two instances of one generic trait
  (`X: Conv[String] + Conv[f64]`) can call either: `Conv[f64].conv(x)` was
  typed by the first bound and refused with "expected 'f64', found
  'String'". A bare `x.conv()` there is ambiguous and names both
  (B-2026-11-13-47).
- The interpreter sends an array handed by value, such as a byte string
  literal in `sock.send_to(b"STOP", to)`; it stopped with "expected a
  sequence", where the compiled build sent it (B-2026-11-14-27).
- `SendError` prints "channel full" / "channel closed" through `f"{e}"`,
  `e.to_string()` and a `?` into `AnyError` on both MIR backends, as
  concurrency.md says; it printed the derived `Full(2)` / `Closed(3)`
  (B-2026-11-14-30).
- A closure that captures a struct by move and uses it only through its
  fields moves just those fields, as a borrowing capture borrows just its
  fields (core semantics §3.6, §9.1; B-2026-11-14-20). It moved the whole value: a
  later use of another field (`let f = || eat(s.a); eat(s.b);`) passed
  check and failed every run with the compiler's internal move check, and
  a returned `|| s.a.n` kept `s.b` and `s.c` alive until the closure
  dropped. A struct with a `Drop` body still moves whole.
- `parse` reports the `ParseError` its documentation gives at the edges.
  A whitespace-only input is `Empty`, as `""` is (it was `InvalidDigit`
  for an integer and `Invalid` for a float), and whitespace around a
  number no longer changes the error, so `u8.parse(" -1 ")` is
  `Underflow`. `u8.parse("-0")` and `"-00"` are `InvalidDigit`, not
  `Invalid`, which an integer never reports (B-2026-11-13-21).
- A view literal built in place at an `own` parameter moves in with what
  it borrows: `pick([t.as_str()])` with `fn pick(other: own Vec[Str])` and
  `pickv(V { s: t.as_str() })` were refused as "a reference to a temporary
  outlives its statement", though the result borrows `t`, not the literal
  (core-semantics §5.3; B-2026-11-13-81).
- A loop over `s.split(..)` may move `s` on a `return` inside it again:
  since the split iterator keeps its own `String` pattern
  (B-2026-11-13-43), its drop after the return read as a use of `s`, so
  `return Err(path)` was E0516 (apps/graph/doc_tree). A struct or enum
  with no `Drop` body of its own now reads a borrow at its drop only where
  a part's drop does (B-2026-11-13-82).
- A compiled program that ends through a foreign `exit()` writes what it
  printed first: standard output to a pipe or a file was buffered and
  lost, though the interpreter printed it (B-2026-11-13-75).
- A type written with no arguments takes its parameters' defaults when it
  has one for each: `let a: Holder = Holder { v: 3 }` with
  `struct Holder[T = i64]` was refused as `Holder` against `Holder[i64]`
  (design.md § Default generic arguments, B-2026-11-13-78).

- Workspace members share one `kara.lock` and one `dist/`, at the workspace
  root (B-2026-11-13-60). The lock is solved once over every member's
  dependencies, so two members agree on a dependency's version. Each member
  used to resolve alone and write its own `kara.lock` and `dist/`, so a member
  allowing `demo_dep = "1.0"` locked 1.5.0 while a sibling capped below 1.2
  locked 1.0.0. `karac update` in a member refreshes the root's lock, `karac
  clean` in a member removes the shared `dist/`, and `karac doc` writes a
  member's pages to `dist/doc/<member>`.
- `karac check src/x_test.kara` in a package reports the package's test
  files' errors (B-2026-11-13-42). The named test file was left out of the
  package check, so it printed "All checks passed." and exited 0. `karac fix`
  of a test file now fixes its imports.
- A where clause that bounds a projection by an operator trait gives it the
  operator: design.md §9's own `total`, `where I.Item: Add[Output =
  I.Item]` with `acc = acc + x`, was refused on every lane with "type
  'I.Item' does not implement trait Add" (also `Mul`, `Sub`, `+=`). A bare
  `where I.Item: Add` names no `Output`, which `karac fix` adds, as for
  `T: Add` (B-2026-11-13-48).
- An error on an imported name in a package's `_test.kara` names that file,
  not its sibling's (B-2026-11-13-40), and `karac fix` of a mistyped module
  in an import of a package's non-entry module rewrites the module name. It
  wrote the fix at the start of the file: `import db.conx.x;` became
  `db.conndb.conx.x;`.
- `s.split(p)`, `s.splitn(n, p)`, `s.match_indices(p)` and `s.matches(p)`
  return an iterator that borrows `s` only (core semantics §5.4), but the
  iterator kept a view of the pattern too, so dropping the pattern before
  walking it (`let it = { let p = sep(); s.split(p.as_str()) };`) read
  freed memory in a built binary. The iterators now keep their own copy of
  the pattern (B-2026-11-13-43).
- A top-level `const` names its value as an array or vector length:
  `Array[i64, SIZE]` with `const SIZE: i64 = 3;` was kept as an opaque name,
  so `[0; 3]` was refused as a mismatch with its own declared type
  (B-2026-11-13-22).
- On the MIR interpreter (`karac repl`, comptime), `i128.MIN.abs()` traps as
  integer overflow instead of returning `i128.MIN`, and `i128.abs_diff` returns
  the whole `u128` distance (`i128.MIN.abs_diff(3)` is `2^127 + 3`) instead of
  wrapping through `i128` (B-2026-11-13-24). The compiled backend was right.
- A `Slice` compared with an `Array` (or a `Vec`) of the same element
  type, `pkt[236..240] != COOKIE`, was never equal in compiled code: the
  right side was read as a slice. The interpreter was right. Compiled code
  now compares both sides as slices (B-2026-11-13-16).
- A program built for macOS keeps a frame record in every function, as
  Apple's arm64 and x86_64 ABIs require (arm64 leaf functions may skip
  theirs, as rustc's and clang's do). Compiled code saved `x29` / `rbp`
  without pointing it at its own record, so `sample`, Instruments and crash
  reports walked past every Kāra frame. `KARAC_MIR_FRAME_POINTER=0` leaves
  the records out (B-2026-11-12-10).
- A tuple, array, `Vec`, `Option` or derived-`Ord` struct holding a value
  with a hand-written `Ord` sorts, searches and keys a sorted table by that
  `Ord`, as its derived `cmp` does. `sort`, `sort_by_key`, `binary_search`,
  `is_sorted`, `SortedMap` and `SortedSet` compared the holder's fields
  directly instead, so `v.sort_by_key(|x| (Down { v: x }, 0))` sorted
  ascending under a descending `Down`, on `karac run` and in compiled
  builds alike (B-2026-11-11-60).
- A generic `a.cmp(b)` under `T: Ord` instantiated at `String` compiles; it
  was refused as "not lowered yet" (B-2026-11-10-41).
- A compiled `Pool.acquire` no longer stalls for its whole timeout when a
  value is free. On a multi-worker executor, another task could take the
  value between the call's readiness check and the call itself, and the
  losing call then slept its worker thread for the timeout (about one run in
  ten of two tasks trading one value); it now waits again. Two tasks that
  each find room no longer both create a value past `max_connections`
  (B-2026-11-11-47).
- `Semaphore.acquire` on an exhausted semaphore waits in its task for
  another task's `release`, up to its timeout, instead of failing with
  `Timeout` at once, and `release` is no longer capped at the starting
  count, so `Semaphore.new(0)` signals between tasks. Outside a task an
  exhausted `acquire` waits its whole timeout, as `Pool.acquire` does
  (B-2026-11-11-34).
- A compiled program whose tasks wait with a timeout (`Semaphore.acquire`,
  `Pool.acquire`) no longer sometimes waits that whole timeout at a group's
  end after its tasks have finished. The main thread, which alone resumes the
  code after the group, could sleep to the timeout while another worker ran
  the remaining tasks: kata 1115's differential took 62 s instead of 3 s in
  about one run in twenty (B-2026-11-11-66).
- A program that calls the HTTP client (`Client`, `http.get`, `http.post`)
  from more than one task no longer races on the client's event loop. One
  runtime was shared by every calling thread, and ThreadSanitizer reported
  races in its socket bookkeeping; each thread now has its own, and each
  request closes its connection before it returns (B-2026-11-11-1).
- A `Str` view of a parameter passed through a closure combinator and
  returned (`s.split_once("=").map(|p| p.0).unwrap_or(s)`,
  `.and_then(|t| t.strip_suffix(q))`) is accepted: the borrow check
  counted the closure, lent to `map`, as something the result borrowed,
  and refused it with E0509. A closure that lends one of the function's own
  locals into the result is still refused (B-2026-11-09-98).
- **A closure handed to a bare `f: F` parameter with `F: Fn(i64) -> i64`
  takes the bound's signature.** A bare parameter borrows, so its slot is
  `ref F`, and the bound was looked up only for a plain `F`: a closure whose
  body fixes nothing about its parameter (`ap(1, |x| x)`, `|x| { println("a");
  x }`) passed `karac check` and then stopped every backend with an invalid
  `ap[Fn(()) -> ()]`, and `|x| { let y = x; y }` was refused. A named
  function handed to such a parameter also solves what only the bound
  mentions: `app(double, 4)` over `F: Fn(i64) -> U` is a `U = i64`
  (B-2026-11-09-39).
- A closure called where it is written whose body ends in `collect()`
  (`let s: Set[i64] = (|v: ref Vec[i64]| v.iter().collect())(a);`) builds
  what the call's context decides, where it stopped at lowering with
  `UnknownParam("C")` (B-2026-11-10-18).
- `v.iter().fold(None, |acc: Option[i64], x| Some(x))` was refused with
  `Option[ref i64]`: the annotation on `acc` did not settle `fold`'s
  accumulator type before the tail was checked. It now does, and the tail
  `Some(x)` reads `x` as its value, as `let o: Option[i64] = Some(x);`
  does, printing `Some(3)` (B-2026-11-09-36).
- `let f = |x, n| x + n; println(f(1, 2));` was refused at the operator,
  "type '?T0' does not implement trait Add", before the call that types
  `x` and `n` was seen. An operator on closure parameters nothing has
  typed yet now waits for the later use design.md § Closures lets type
  them, and prints 3; a parameter no use types gets the closure message,
  "cannot infer closure parameter type", alone (B-2026-11-10-8).
- `while let Some(x) = it.find(|x| x < 5) { .. }` over `v.iter()`, and
  the same with `Peekable.next_if` or a `find` in a `for` body, was refused
  E0516: the iterator, given `mut ref`, was taken to keep the reference to
  the closure passed to it, so the next iteration's closure conflicted. A
  value whose type has no place for a closure now keeps only what the
  closure holds (B-2026-11-10-24).
- A tuple literal's numbers were fixed as `i64` where they were written:
  `let t = (0, 0); let (d, u) = t; take_i32(d)` and a Dijkstra that
  pushes `(0, 0)` into a `PriorityQueue` and returns the popped distance
  from an `i32` function were refused as narrowing. A tuple of literals is
  literals now, open in a `let` and in a push until a use fixes them, as a
  bare literal is (B-2026-11-10-40).
- `let x = if c { 1 } else { 0 }; take_i32(x)` was refused as narrowing,
  as were a `match` and a block of literals bound the same way and kata
  1118's `let feb = if leap { 29 } else { 28 }` returned from an `i32`
  function. A join of literals is a literal now, in a `let` and as a list
  element, open until a use fixes it. Beside a type that widens into the
  default (`rods[i] |= bit` over `[u8; 10]`) it keeps `i64`, as a bare
  literal does, and the refusal now names the binding and offers its
  annotation (B-2026-11-10-49).
- `let bit = 4; rods[i] |= bit` over a `u8` array was refused as narrowing
  an `i64`, and `karac fix` declared `bit: u8`. A compound assignment
  into a target whose type is fixed is an assignment to it, so an open
  literal operand takes the target's type, as `rods[i] = bit` does (kata
  2103). `let mut b = 0; b += y` is unchanged: there the target is the
  open binding.
- An unknown method in a `let` initializer,
  `let lines: Vec[Str] = text.trim_end_matches('\n').split("\n").collect();`,
  was followed by "a reference to a temporary outlives its statement" at
  the same span. A value already refused inside is not reported again for
  its temporaries (B-2026-11-10-48).
- Rust's `let local = [127u8, 0, 0, 1];` passed to an `Array[u8, 4]`
  parameter was refused at each use with no fix. Each refusal now says
  the binding is a `Vec` because a list literal with no expected type is
  one, and when it only ever holds that literal of the parameter's length
  `karac fix` declares it `local: Array[u8, 4]` (B-2026-11-10-47).
- Rust's `use std::iter::Peekable; use std::str::Chars;` with a parameter
  `&mut Peekable<Chars>` stopped `karac fix` at `std.iter has no item
  Peekable` and `unknown module std.str`. An import of `std::iter`'s or
  `std::str`'s prelude types is deleted like a collections one, `Chars`
  and `CharIndices` are renamed `StrChars` and `StrCharIndices`, and
  `Peekable[StrChars]` is refused with its item filled in,
  `Peekable[StrChars, char]`, as the fix. docs/library/iterators.md now
  gives `peekable`'s result as `Peekable[Self, Self.Item]`
  (B-2026-11-10-35).
- An `if` or `match` of literals as an element of a list or tuple literal
  was typed `i64` where it was written, so `let v: Vec[i32] = [1, if b
  { 2 } else { 3 }];` and `let t: (i32, i32) = (1, if b { 2 } else { 3 });`
  were refused as narrowing. Such an element is a literal now, typed by its
  slot, with every branch range-checked (B-2026-11-11-38).
- `let v: Vec[i32] = [0].into_iter().chain([1, 2].into_iter()).collect();`
  was refused as `C: FromIterator[i64]`. A list literal iterated where it
  is written keeps its number literals open, as a `let` of one does, for
  the chain's end to fix. The same chain with a `scan` on one side built a
  `Vec[i32]` from `i64` items when the `scan`'s state literal had settled
  first (garbage compiled, an AddressSanitizer abort); it is refused now,
  with the `scan` state's `0i32` fix offered through the `chain`
  (B-2026-11-11-41).
- `for x in it` over an owned parameter `it: own I` with `I:
  Iterator[Item = i64]` typed `x` as `I`, so `x.abs()` and `let y: i64 = x`
  were refused. It yields the bound's item now. A `T` that only the bound's
  equation names (`fn show[T: Display, I: Iterator[Item = T]](it: own I)`)
  is solved at the call from the iterator's item, where it was refused as
  `I.Item = T` not satisfied (B-2026-11-13-18).
- A `par for` whose body's value is a tuple, written in place or returned
  by a call, was refused with the internal "an expression with no recorded
  type is not lowered yet" (B-2026-11-13-4).
- A `par` or `par for` calling an un-annotated `let f = mk();` was refused
  as a conflict through "`unknown (a call through a value)`" even when
  `mk`'s return type declares `with ()`. The `let` takes that clause now,
  as an annotated one does (B-2026-11-13-19).
- A list literal compared with `==` or `!=` was typed `Vec` on its own, so
  `m != [75u8, 82u8, 69u8, 67u8]` with `m: Array[u8, 4]` was refused as
  "cannot compare 'Array[u8, 4]' and 'Vec[u8]'", as were `s == [1, 2]`
  with `s: Set[i64]` and `v == [1, 2]` with `v: Vec[i32]`. The other
  operand is the literal's expected type, as it is for a number literal,
  in either order (design.md § Collection literals, B-2026-11-13-2).
- `self` in a generic `impl[R] Wrap[R]` was typed `Wrap` without its
  arguments, so `helper(self)` with `fn helper[R](w: ref Wrap[R])` was
  refused "expected 'Wrap[R]', found 'Wrap'", and in a library `impl[R]
  BufReader[R]` a call of the native `read` on `self` was refused by the MIR
  builder as having no resolved callee. As a generic call's argument and as
  a method's receiver, `self` is now the impl's `Wrap[R]`
  (B-2026-11-11-62).
- A `scan` whose state starts at an unsuffixed literal settled the state
  to `i64` at the closure's first read of it, so kata 1118's Rust shape
  `[0].into_iter().chain(lengths.iter().scan(0, |total, d| { let total =
  total + d; Some((total, total)) })).collect()` into a `Vec[i32]`, with
  `lengths` a literal list, was refused. A literal at a generic parameter
  now stays open when the call's result carries that parameter as a type
  argument (the `IterScan` a `scan` returns), through the closure and any
  further adapters, until the `collect` fixes the state, the items and
  `lengths` together (design.md §8 Literals, B-2026-11-11-61). Beside items
  of an already fixed type, the state keeps the default as before, with
  its `0i32` fix.
- LeetCode's `class H2O` written `struct H2O` is Const class (CN-1, CN-4)
  and was refused with only "consider renaming to `H2o`". A private
  `struct` or `enum` with such a name now gets that rename as one fix over
  every use of the name in the file, withheld when an f-string's hole
  names it (B-2026-11-11-35).
- `karac run` read a reset TCP connection as an end of stream (`Ok(0)`),
  and a connected `UdpSocket`'s `recv` after its datagram was refused
  waited out its read timeout (`TimedOut`). The interpreter peeks a socket
  to learn when a waiting read can go on, and the peek took the socket's
  pending error; the read now reports it, `os error 104` and `connection
  refused`, as a compiled build does (B-2026-11-10-15).
- `let v: Vec[Str] = ["a", "b"].to_vec();` was refused as a reference to
  a temporary (the array literal) outliving its statement, in the check
  and again in MIR borrowck; the `Vec` copies the elements and carries
  only what they borrow (B-2026-11-09-50).
- `let v = it.collect();` whose target a later use decides, such as
  passing `v` to a `Vec[T]` parameter, checked clean and then stopped at
  lowering with "the method call `.collect` with no resolved callee"; it now
  builds the parameter's type, and an item type that collection cannot hold
  is the same `FromIterator` error an annotated binding gets
  (B-2026-11-09-56).
- A closure whose body ends in `collect()` builds what its uses decide
  (`take(to32(a))` with `take(v: Vec[i32])`), where it stopped at lowering
  with `UnsolvedParam("C")` (B-2026-11-09-40).
- A program's own `struct Entry` (a cache's entry) kept none of its own
  methods: every call on it went to the map entry's dispatch and was
  "no method ... on type 'Entry'". Only the map entry's `or_insert`,
  `or_insert_with`, `and_modify` and `or_default` go there now
  (B-2026-11-09-93).
- Under a `FileSystem` override, a compiled `File.open`, `File.create` or
  `OpenOptions.open` of a `String` path built at run time handed the
  fake's `open` a garbage path (the `String`'s address read as a `Str`),
  so a state file saved through a fake was not found again; a literal
  path worked (B-2026-11-09-71).
- An HTTP request past its `timeout` failed in `karac run` with ureq's
  "Network Error: Network Error: Error encountered in the status line:
  timed out reading response", without the URL; it now says
  `<url>: request timed out after <ms> ms`, as a compiled program does
  (B-2026-11-09-69).
- A `let` inside a block no longer outlives it for the `let mut` rule: `let
  mut q = 7; if c { let q = 3; } q = 8;` was refused as a write to the inner,
  immutable `q` (B-2026-11-09-99).
- String literals in `[..].to_vec()` take the `Str` a `Vec[Str]` (or
  `Vec[(Str, T)]`) parameter wants; `total(["a", "b"].to_vec())` was
  refused as a `Vec[String]` (B-2026-11-09-49).
- Eleven library error types did not implement `Display` and `Error`
  (design.md §10 says every one does): `Utf8Error`, `NulError`,
  `NetError` (and its alias `TcpError`), `TlsError`, `HttpError`, `RegexError`,
  `CliError`, `ChannelError`, `SemaphoreError`, `DiscriminantError` and
  `AlreadySetError`. `String.from_utf8(b)?` in a function returning
  `Result[T, AnyError]` was refused, and `f"{e}"` printed the variant's
  name. Each now reads as a short phrase (`invalid UTF-8 byte`, `os error
  5`) (B-2026-11-09-54).
- A program that declares its own `Response` to send headers (io.md §
  HTTP server) gets it back from the HTTP client too, and the compiled
  client wrote only the library's two fields there: the program's
  `headers` was left unwritten and its drop freed a garbage pointer
  (`free(): invalid pointer` or a segfault, where the interpreter printed
  the right answer), and a declaration in another field order read the
  status from the body. Both backends now fill the fields by name, and
  `headers` holds the response's headers (B-2026-11-09-34).
- `Clone.clone(x)` is the call `x.clone()` (design.md § Method
  Resolution). `karac check` typed it as an error without a diagnostic, and
  `karac run` and `build` then refused the program with "a type that failed
  to check is not lowered yet" (B-2026-11-09-32).
- Raw pointers reach a non-`Copy` element: `ptr.mut(xs[i])` takes its
  address without moving it, `p.read()` copies the bits out and `p.write(v)`
  stores without dropping, as design.md § Raw pointers now says. The first
  was refused as a move out of the collection, the second as a move out of
  a borrowed place, and the MIR interpreter stopped at the third
  (B-2026-11-09-20).
- `T.from(x)` under `T: From[X]` and `T.try_from(x)` under `T: TryFrom[X]`
  call the instance's conversion: the library's `From` and `TryFrom` now
  declare `from` and `try_from` as design.md § Conversion traits writes
  them, and were refused with "a type that failed to check is not lowered
  yet". `impl From for Out { fn from(a: A) }` still reads as `From[A]`
  (B-2026-11-09-4).
- Inside `fn fresh[T: Default]() -> T { T.default() }`, `T` is the
  instance's type also where the program declares a `struct T`: the call
  went to the struct's `default` at every instance, so `let u: U = fresh()`
  built a `T` (B-2026-11-09-3). A local of the name wins the same way: the
  library derives' own `comptime T: Type` parameter made `#[derive(Serialize,
  Deserialize)]` on a program's `struct T` fail with an internal error, and
  `Json.deserialize` read any type as the program's `T` (B-2026-11-08-78).
- `#[derive(Serialize, Deserialize)]` writes a tuple field or payload as a
  sequence, `[1,true]` in JSON, and reads it back from one of exactly its
  length (design.md § Serialize and Deserialize). It passed `karac check`
  and failed every run lane in MIR; a tuple inside another type
  (`Vec[(A, B)]`) is now refused at check, naming the field
  (B-2026-11-08-78).
- A missing field in `Json.deserialize` is named by its key in the input,
  `missing field `userName`` for a field renamed `userName`, not by its Kāra
  name (B-2026-11-08-80).
- A binding moved inside a `let … else` else block and moved again after
  the `let … else` is accepted: the else block diverges, so the two moves
  are on different paths. It was refused as a use after move, and `karac
  fix` wrote a needless `.clone()` at the later move (B-2026-11-09-16).
- The element literal of `let cnt = [0; 26];` and `let ans = Vec.filled(n,
  0);` is fixed by where the collection goes, as a scalar's is (design.md
  §8 Literals): returned as an `Array[i32, 26]`, or handed to a
  `mut ref Vec[i32]` parameter, it is an `i32`. Both were refused as
  `Array[i64, 26]` / `Vec[i64]`. Where an operator left the element at
  `i64` first, returning the collection whole names the binding and
  `karac fix` writes the literal `0i32`, as for an element read
  (B-2026-11-08-57).
- A constant computed from another constant matches as a pattern:
  `const BB: i64 = AA + 1;` then `match x { BB => .. }` was refused with
  "this range pattern bound is not lowered yet", though a constant in a
  pattern is its value (design.md § Pattern matching, B-2026-11-08-63).
- **A method returning `Self` in an impl of a `shared struct` is the
  handle type at its call.** `let y: Node = Node.new(2);` was refused
  "expected 'Node', found 'Node'" (B-2026-11-08-70).

- **A slice local may be bound to its own sub-slice.** `rest = rest[1..]`
  on a `Slice[T]` local was refused with E0516, as if the sub-slice
  borrowed the local; it borrows what the slice borrows, as the `Str` twin
  already did (core semantics §5.2; B-2026-11-08-66). A body that writes
  through a `mut Slice` keeps the stricter reading.
- **`entry(k).or_insert(v)` stores `v` at the map's value type.** A
  literal default on a map whose value is narrower than `i64`
  (`cnt.entry(p).or_insert(0)` over a `Map[(i32, i32), i32]`) was built as
  an `i64`, and the compiled program wrote 8 bytes into the 4-byte value:
  AddressSanitizer's stack-buffer-overflow. The interpreter's answer was
  right (B-2026-11-08-69).
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
- **A `collect()` passed to a `mut ref` parameter is lent as a
  temporary.** `total(mut v.iter().map(f).collect())` was handed over as if
  the collection were the reference: the interpreter stopped on `deref of
  non-reference` and a compiled binary segfaulted. The collection now lives
  to the end of the statement and is dropped there (core-semantics §5.5,
  B-2026-11-08-43).
- **A generic trait implemented twice is called by naming its arguments.**
  With `impl Conv[String] for N` and `impl Conv[f64] for N`,
  `Conv[String].conv(n)` was refused ("no method 'conv' on `Conv[…]`", plus
  a spurious move of `n`), and the ambiguity diagnostic for `n.conv()`
  suggested `Conv.conv(N)` twice, a form that is itself ambiguous. The
  qualified form now picks that instance's impl, and the diagnostic lists
  `Conv[String].conv(N)` and `Conv[f64].conv(N)` (design.md §9 UFCS,
  B-2026-11-08-77).
- **`karac run --interp prog | head -1` stops when `head` does**, with
  status 141, as the JIT and a built binary do (io.md): the interpreter
  ignored the closed pipe, ran every remaining iteration and exited 0
  (B-2026-11-08-28).
- **`IoError` and `VarError` display as phrases** (io.md § Errors):
  `f"lgrep: nope: {e}"` prints `lgrep: nope: not found`, not `NotFound`, and
  `fn main() -> Result[(), IoError]` exits with `Error: not found`.
  `VarError` reads `environment variable not found`. Both now derive
  `Debug`, so `dbg(IoError.NotFound)` shows the variant where it was refused.
  `AllocError`'s exit from `main` printed its variant too and now prints its
  phrase (B-2026-11-08-30).
- **A map's inserted literals take the types a later use gives them**, as a
  `Vec`'s pushes do: `let mut d = SortedMap.new(); d.insert(1, 2);
  m(d);` into `d: SortedMap[i32, i64]` was refused as 'found
  SortedMap[i64, i64]' (design.md §8, B-2026-11-08-42).
- **A closure returning `Some((Vec.new(), x))` solves the callee's type
  parameters.** `step(v, |seen| Some((Vec.new(), seen.len())))` over
  `g: Fn(S) -> Option[(S, O)]`, and the same `scan` state reset, were refused
  as "cannot infer type parameter 'O'" (B-2026-11-08-58).
- **A type that implements both `Iterable` and `IntoIterator` is borrowed
  by a bare `for`**, as `Vec` is (design.md § `Iterator` and `Iterable`): it
  was refused as "implements `IntoIterator` but not `Iterator`"
  (B-2026-11-08-76).
- **A range of bare literals takes the type its use gives it.**
  `fn f() -> Vec[i32] { (1..=5).collect() }`, and the same through
  `filter` and `map`, into a `Set[u8]` or an `i32` sum, were refused as
  "`C: FromIterator[i64]` is not satisfied"; the range now stays open until
  the collect fixes it, as a lone literal does (design.md § Type inference,
  B-2026-11-08-86). A lookup's needle fixes a literal as any argument does,
  so `seen.contains(x)` on a `Set[i32]` or a `Vec[i32]` takes an
  unannotated `let x = 3` (or a range's item) as `i32`.
- **A list literal's numbers take the type its binding's use gives them.**
  `let mut v = [0, 0]; g(mut v)` into `mut ref Vec[i32]`, and `[0, 7]` into a
  `Vec[i32]` argument, were refused as 'found Vec[i64]' (design.md §8,
  B-2026-11-08-88).

### Performance

- **A char's `to_string()` that is only lent does not allocate.** When the
  `String` is only borrowed, as in `"aeiou".contains(c.to_string())` (the
  form the char-pattern fix-it writes), a compiled build keeps the char's
  UTF-8 bytes in the function's frame. Kata 2062's bench went from 0.76 s
  to 0.38 s and 2063's from 0.65 s to 0.20 s. A `String` that is returned
  or stored still owns a heap buffer (B-2026-11-10-25).
- **`karac run` of a large program compiles on every core, and no longer
  builds one enormous function.** A program the build splits into codegen
  units is compiled the same way for the JIT, one object per unit on its
  own thread, and the runner links the objects. The JIT's inliner also stops
  folding single-call functions into a caller past 10,000 instructions,
  which turned mini_tldextract's hundred table functions into one function
  of 183,000 instructions that code generation took 67 s over. On the
  4-core container, `karac run` of mini_tldextract went from 92 s to 6.9 s
  (its build: 16 s), mini_pyasn1 from 30 s to 8.8 s and mini_markdown from
  24 s to 11.5 s, no slower than building it. The kata benches are
  unchanged: the budget touches none of their 1,132 programs, and they are
  below the split's size (B-2026-11-10-5).
- **`PriorityQueue` sifts move a hole** instead of swapping a level at a
  time, as Rust's `BinaryHeap` does: the element being sifted is read out
  once, each element it passes moves one level, and a `defer` writes it
  back where it settles, which is also what a `cmp` that panics inside an
  isolating group runs at teardown. On x86 (callgrind) kata 632 went from
  2.59G to 2.16G instructions, 743 from 2.11G to 1.86G, 480 from 4.33G to
  3.93G and 786 from 7.93G to 6.70G (B-2026-11-08-87).

- `v.sort_by(|a, b| b.cmp(a))` (or `a.cmp(b)`, or a function written so)
  over integers, `char`s or `bool`s sorts with the runtime's integer sort,
  reversed for the descending order, instead of a merge sort that calls
  the comparator per pair. The result is the same: such a comparator calls
  nothing, and equal elements are the same bits. Kata 1058's bench went
  from 1.44x to 1.08x Rust's `sort_by` (B-2026-11-06-34).

- `karac check` no longer parses the 22 gated `std` modules (`std.web`,
  `std.pool`, `std.crypto`, ...) a program does not import: each is parsed
  when something asks for it. A check of a 3-line program runs 5% fewer
  instructions (B-2026-11-09-28).

- `<`, `<=`, `>` and `>=` on a tuple of integers, `bool`s and `char`s
  compare element by element instead of building an `Ordering` for each:
  a `PriorityQueue` of tuples sifts with one compare and branch an element
  (kata 632 2.91G to 2.62G instructions, kata 743 2.40G to 2.09G on x86;
  B-2026-11-09-1).
- **`s.chars().last()` reads the last char from the back** instead of
  walking the whole text, on a `String` and on a `Str` view, as Rust's
  `Chars::last` does. Kata 2030's string arm, which reads the top of its
  stack this way, was quadratic; 20000 reads of a 20000-char string went
  from over two minutes under the interpreter to a third of a second
  (B-2026-11-08-81).

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
- **`PriorityQueue`'s sift loops work on a slice view of the heap.** The
  vector's pointer and length stay in registers rather than being reloaded
  from the queue every level, and a direction's loop no longer tests it
  each level. A push-and-pop loop over 400,000 `i64`s: 268M instructions
  to 213M; kata 480: 4.92G to 4.36G (B-2026-11-08-10).
- **`skip(k)` and `nth(k)` jump over a `Vec` or a slice.** The skipped
  items are stepped past with `advance_by`, which the `Vec` and slice
  iterators now do by moving their index, as a range already did; a
  `skip(i * n).take(n)` row walk was quadratic. 50,000 rows of
  `v.iter().skip(i).take(1).sum()`: over 120 s to 0.3 s under the
  interpreter (B-2026-11-08-52).

### Diagnostics

- Rust lifetimes are fixed away: `&'static str` becomes `Str` and `&'a mut T`
  becomes `mut ref T`, as `&str` and `&mut T` do, and a lifetime in a generic
  list (`struct P<'a>`, `impl<'a>`, `fn f<'a, T>`) is deleted with its comma,
  or with the list when it holds nothing else. The parse used to stop at the
  first one (B-2026-11-14-25).
- A closure whose result is `*n`, with `n` lending a non-`Copy` value
  (`.iter().map(|(_, n)| *n)`), is refused by the check with a fix that writes
  `n.clone()`, or a tuple's parts one by one. Only the build refused it, with
  no fix (B-2026-11-14-26).
- A value read out of a tuple a literal was pushed into
  (`graph[a].push((b, 0))`, then `for (next, color) in graph[n]`) and narrowed
  into an `i32` names that literal, and `karac fix` writes its suffix
  (`0i32`), which types that position of every tuple pushed (kata 1129,
  B-2026-11-12-2).
- An `errdefer` in a function returning neither `Result` nor `Option` is
  `warning[unreachable_errdefer]`: there is no failure exit, so it never
  runs; the help points to `defer` (design.md §10 Rules, B-2026-11-14-19).
- A new `redundant_clone` lint warns on `f(x.clone())` when `f`'s parameter
  is bare and so only borrows `x`, and `karac fix` drops the `.clone()`. It
  stays quiet where the copy is needed: an `own` parameter, a sibling argument
  that names the same value (`f(v.clone(), mut v)`), the method's own receiver,
  a type with a hand-written `Clone`, and a clone that changes the type (a
  string view cloned to a `String`) (B-2026-11-14-3).
- `Array[i64, 26].filled(0)` names the receiver as written, not `Array[…]`,
  and says a fixed array is built with the repeat literal `[0; 26]`, which
  `karac fix` writes when the element and the length are literals or scalar
  names. `Array.filled(n, v)` gets the same pointer (B-2026-11-14-6).
- `karac fix` writes five rewrites its messages already named: an unknown
  method's "did you mean 'len'?", `{x:?}` to `{x}`, `-> escaping Fn(..)` to
  `-> Fn(..)`, a nested `fn` that shares its line with other code moved above
  its item, and `Box.new(e)` to `e` inside an f-string hole. `karac fix --help`
  lists what it covers today (B-2026-11-14-8).
- The ownership cycle on a recursive plain enum (`enum Tree { Leaf(i64),
  Node(Tree, Tree) }`) says to declare it `shared enum Tree`, which `karac
  fix` writes, or to wrap the recursive field in `Vec`, as design.md does. It
  offered `Box[T]`, which Kāra does not have, and `ref`. A cycle with no case
  that ends it names `Option[T]` or `Vec[T]` (B-2026-11-14-2).

- `karac explain` knows every symbolic code the compiler mints as a code:
  the package manager's (`E_DEPENDENCY_CYCLE`, `E_PATH_DEP_NOT_FOUND`, the
  `E_LOCKFILE_*`, `E_TOOLCHAIN_*`, `E_PROXY_*` and `E_INSTALL_*` families),
  the ownership checker's (`E_CROSS_BORROW_CONFLICT`,
  `E_SLICE_BORROW_CONFLICT`, …), and the attribute, parser and FFI ones
  such as `E_TEST_BLOCK_NOT_TOP_LEVEL` and `E_EXTERN_REF_PARAM`, and the
  codes a message opens with, which `karac check` prints as its headline
  (`error[E_CHAR_AS_NARROW_INT]: …`). 86 of the first kind were refused as
  unknown and 166 of the second read as a class name, against design.md
  §2's "every code is catalogued"; a test now fails when a new one has no
  entry (B-2026-11-13-20, B-2026-11-14-10).

- A view stored where its borrow is not kept (an escaping closure, a
  `shared` value, a channel) names what it views, "a view of `t.s`",
  rather than "a view of a temporary value", and a view stored in a
  `shared` field prints its type as written, `Option[Str]` rather than
  `Option[str]` (B-2026-11-13-77).

- `karac fix` rewrites `println!` and `format!` whose arguments hold a
  string or char literal with an escape, a brace or a colon, such as
  `text.matches("\r\n").count()` (B-2026-11-13-70). An f-string hole takes
  such a literal as written; the rewrite used to refuse it.
- `s.rsplit(sep)` names `rsplit_once(sep)` and `split(sep).last()`, rather
  than offering `split`, which starts from the other end (B-2026-11-13-71).
- `group.join()` on a `TaskGroup` says that dropping a group joins its
  tasks, and `karac fix` writes `drop(group)` (B-2026-11-13-72).
- A `ref x` over a `String` beside a `Str` or a string-literal branch of an
  `if` or `match`, or against a `Str` annotation, is refused with a fix that
  writes `x.as_str()`: a `ref String` is a `Str` only as a call argument.
  Rust's `if c { ".." } else { &arg }` used to stall the Mend loop at
  `ref arg` (B-2026-11-13-74).
- `unused_mut` honours `#[allow]`, `#[deny]` and `#[expect]` on the `let`
  statement or an enclosing block statement, innermost first, so a
  statement `allow` beats a function `deny`. A statement-level
  `#[expect(unfulfilled_lint_expectation)]` is E_EXPECT_ON_UNFULFILLED and an
  unknown lint name there is `unknown_lint`, as on an item
  (B-2026-11-13-26).
- A run-time value as a const generic argument (`Array[i64, n]` with `n` a
  parameter or local, or `n + 1`) is one error at the argument, "const
  generic argument must be a compile-time constant expression", naming the
  value and pointing to `Vec`. It used to surface later as a type mismatch,
  an "undefined type", or three cascading errors (B-2026-11-13-22).
- Assigning into a field of a value moved as a whole (`eat(p); p.a = 1;`)
  says the value is initialized again only as a whole (core semantics
  § 3.4), and to assign all of it or move only the fields the callee
  needs. The help used to suggest deriving `Clone` and cloning at the
  move, though nothing reads the value, with a `karac fix` edit to match
  (B-2026-11-13-23).

- An E0516 whose borrow lends a `shared` value (`let e2 = e;` over
  `m.get(k)` for a `shared struct Ent`) says that `*r` is a second counted
  handle, which holds no borrow. It no longer offers `.clone()` and "`*r`
  for a `Copy` value", and `karac fix` writes `let e2 = *e;`.
- Rust's slice copy, `dst.copy_from_slice(src)`, used to be refused as
  "no method 'copy_from_slice' on type 'Slice'" with no fix, and a borrow
  error followed at every later use of the buffer. The error now names
  Kāra's spelling, and `karac fix` writes it (design.md § Range indexing;
  B-2026-11-13-15): `pkt[4..8].copy_from_slice(xid)` becomes the range
  assignment `pkt[4..8] = xid` when the element is `Copy`, and any other
  receiver gets `clone_from_slice`.
- `karac fix` mends Rust's pause to `std.time.sleep` (B-2026-11-11-73):
  `use std::thread;` and `use std::thread::sleep;` become
  `import std.time.sleep;`, and `thread::sleep(d)` and
  `std::thread::sleep(d)` become `sleep(d)`. A program that calls the
  full path without an import line gets the import once. A
  `use std::thread;` that a `thread::spawn` also goes through stays, for
  the spawn's own diagnostic.
- `karac fix` carries Rust's `std::cmp::Reverse` over to `std.cmp.Reverse`:
  `use std::cmp::Reverse;` becomes `import std.cmp.Reverse;` (an unimported
  `Reverse` gets the import), `Reverse(x)` becomes `Reverse { value: x }`
  in expressions, `let` and `match` patterns and closure parameters, and
  `r.0` becomes `r.value`. A `Reverse` around a whole heap item is still
  dropped for a smallest-first `PriorityQueue.new()` when every `Reverse`
  in the program can go; when one stays, all do and each heap is
  `PriorityQueue.max_first()` (B-2026-11-10-41, kata 2102).
- A program struct written as Rust's tuple-struct pattern (`let P(x, y) =
  p`, `P(u, _) =>`) gets the struct pattern `P { a: x, b: y }` as its fix,
  in place of "refutable pattern" or "an enum-variant pattern" errors
  (B-2026-11-10-41).
- A bare `File`, child pipe or `TcpStream` parameter handed to
  `BufReader.new` / `BufWriter.new` (`fn serve(conn: TcpStream)`, a Rust
  port's owned parameter) gets the refusal any `own` parameter does, with
  the fix that writes `own`, instead of E0297 "not a `ref TcpStream`"; any
  other borrowed handle is refused by name (B-2026-11-10-33).
- Collecting text into a `String` (`let s: String = parts.map(..).collect()`,
  Rust's `FromIterator<String> for String`) says to collect a `Vec[String]`
  and `join("")` it (B-2026-11-10-34).
- A tuple of borrowed elements moved where owned ones are wanted
  (`out.push((k, v))` in `for (k, v) in headers` over a bare parameter)
  names the borrowed elements, and `karac fix` adds `.clone()` to each, as
  the one-element form already did; bound out of one bare parameter, the
  fix is that parameter's `own` (B-2026-11-09-96).
- `opt != Some(TOKEN)` with an `Option[String]` and a `Str` in the `Some`
  explains that comparison looks through each operand but not inside type
  arguments, and `karac fix` inserts `.as_deref()` on the owned side
  (`opt.as_deref() != Some(TOKEN)`), which neither moves nor allocates; it
  wrote `Some(TOKEN.to_string())` (B-2026-11-09-97, B-2026-11-11-2). An
  `Option[Vec[T]]` against an `Option[Slice[T]]` takes the same fix
  (B-2026-11-11-3).
- A Rust path through a module Kāra's expressions name keeps that module
  in its fix: `std.fs.read_to_string(p)` and `std::fs::read_to_string(p)`
  become `fs.read_to_string(p)`, `std::env::args()` `env.args()` and
  `std::process::exit(0)` `process.exit(0)`, where the fix wrote a bare
  function that does not resolve (B-2026-11-10-16).
- `unused_mut` warns a `let mut` of a `mut ref` binding written only
  through it (a field store, a mutating method, `f(mut r)`), which needs no
  `mut`; a rebind, a `shared` value lent whole as `mut`, and a `let mut`
  that copies a `Copy` value out of a reference keep theirs
  (B-2026-11-10-19).
- `format!` whose argument is an `if`, `match` or block expression gets the
  f-string rewrite as its fix, a block argument in parentheses
  (B-2026-11-10-17).
- A `let v = it.collect();` used as a receiver or an indexed base before
  the use that decides its target is "cannot infer what this `collect`
  builds where it is first used", with the binding's type as a fix, where it
  was "a type that failed to check is not lowered yet" (B-2026-11-09-56).
- Each bad format spec of an f-string is reported at its own hole, not at
  the literal's closing quote (B-2026-11-09-91).
- A field named `private` (or another structural marker) is reported once,
  "`private` cannot name a field: it is a structural marker, which `r#`
  does not escape", in place of "Expected identifier, found `:`" and a
  cascade through the struct literal (B-2026-11-09-92).
- `opt ?? "" == value` names the precedence (`??` binds loosest) with the
  fix `(opt ?? "") == value`, where it gave three mismatches
  (B-2026-11-09-94).
- A `ref Str` where a `String` is wanted gets `.to_string()`, as a `Str`
  does (B-2026-11-09-95).
- A format spec in Python's spelling, `{x:.1f}` or `{name:>8s}`, is named
  ("Kāra has no `f` type, a float's precision alone sets its digits after
  the point; write `.1`") and `karac fix` deletes the letter; it was
  "unsupported type `f`" with no fix (B-2026-11-09-68).
- `karac fix`'s `fn main() -> Result[(), AnyError]` for a `?` in `main`
  also turns each bare `return;` of `main` into `return Ok(());`, so the
  fixed program checks; a closure's `return` stays (B-2026-11-09-67).
- New lint `unused_mut` (warn by default): a `let mut` that nothing
  writes, assigns through or borrows as `mut` is reported, and `karac fix`
  deletes the `mut`. Calling a local `MutFn` closure counts as a write.
  `#[allow(unused_mut)]` on a function or its `impl` block silences it
  (B-2026-11-09-55).
- A library type without a trait is named as the library's: `e.clone()` on
  a `JsonError` says "the library's `JsonError` does not implement
  `Clone`" instead of advising a derive on the library's declaration
  (B-2026-11-09-48).
- Six mistakes Rust habits make in Kāra were diagnosed with no fix that
  `karac fix` applies; each now has one, found by the Rust-ism audit
  (`scripts/mend/rustism_audit.py`) (B-2026-11-09-53):
  - `pub(crate)`, `pub(super)` and `pub(in ..)` are deleted (the default
    is package-wide) and `pub(self)` becomes `private`.
  - An unreachable `match` arm (`_ =>` after every variant) is deleted,
    with its line when it has one to itself.
  - `?` in a `main` returning `()` declares the error the operand carries
    (`Result[(), String]`), `AnyError` when every error is an `Error`, and
    unwraps an `Option` operand or an error that is neither.
  - A tuple struct `struct S(T);` gets named fields (`value`, or `f0`,
    `f1`, ..), and its uses follow: `S(x)` becomes the literal `S { value:
    x }` and `.0` the field. On a `distinct type`, `.0` becomes `.raw()`.
  - `let v = it.collect();`, what `.collect::<Vec<_>>()` leaves once its
    turbofish goes, is annotated `: Vec[T]` with the iterator's item.
  - `impl<T: Add<Output = T>>` is fixed to brackets; an associated-type
    binding inside the list stopped the fix before.
- `text?` on a bare `text: Option[String]` parameter (a borrow, since
  parameters borrow by default) was refused as "'?' operator requires
  `Result` or `Option`, found 'ref Option[String]'", with no fix. It now
  says the `?` would move the `String` out of the borrow and `karac fix`
  writes `text.clone()?`, as for `??`; a borrowed wrapper whose payload
  and error are `Copy` is read and runs (B-2026-11-09-43).
- `Default.default()` is refused as naming no type, with a fix to the
  expected type's own `T.default()` (`let w: W = Default.default();` becomes
  `W.default()`); it was refused as "a type that failed to check is not
  lowered yet" (B-2026-11-09-32).
- A program's inherent impl on a library type that redefines a library
  method (`impl Option[Ordering] { fn is_lt .. }`) draws one error, at the
  program's impl. It also drew a conflicting-impl error placed at the
  library's own impl, past the program's last line, with "in the standard
  library: " repeated six times (B-2026-11-09-29).
- `karac test --help` spells the gate `#[test(requires: [...])]`, as the
  spec does; it said `requires = [...]`, which the parser refuses with a fix
  to the colon.
- A defaulted `i64` literal read back out of a collection it was pushed
  into (`left[k].push(a)`, then `x` of `for x in ref left[k]` as
  `best.min(x + ..)` with `best: i32`) is refused with the same
  `a: i32` annotation `karac fix` applies when the use reads `a` itself; it
  had no edit (kata 2035, B-2026-11-09-15).
- Rust's `binary_search` taken apart as a `Result` says Kāra's returns
  `Option[i64]` and names `partition_point`, and `karac fix` rewrites the
  mechanical shapes: `.unwrap_or_else(|i| i)` to
  `partition_point(|y| y < v)`, another closure body and a `match` or
  `if let` over `Ok(i)`/`Err(i)` to a block that tests the index
  `partition_point` finds. A shape that would evaluate the sequence or
  the value again, or a `Result`-only method, says why it is not
  rewritten (B-2026-11-09-14).

- A `let … else` that destructures an element (`let Json.Array(xs) =
  row[3] else { … };`), or a plain `let` with a struct or variant pattern
  over one, gets the fix `ref row[3]` when its bindings are only read, as
  `let t = row[3];` already did; it was refused with no fix
  (B-2026-11-09-27).
- A call on a type parameter that none of its bounds declares
  (`T.build(n)` under `T: Make`) is refused naming the bounds, where it got
  an internal "a type that failed to check is not lowered yet"
  (B-2026-11-09-4).
- Rust's `scan` closure over a lent state (`|m, x| { *m = (*m).max(x);
  Some(*m) }`) gets one error saying Kāra's closure takes the state by
  value and returns `Some((next_state, item))`, and `karac fix` writes
  `|m, x| { let m = m.max(x); Some((m, m)) }`; it drew six "drop the `*`"
  fixes that left a closure of the wrong type (B-2026-11-08-45).

- `match v.last_mut() { Some(x) if g => body, _ => other }` and
  `if let Some(x) = v.last_mut() { .. }` are rewritten by `karac fix` to
  `if v.len() > 0 and g { .. } else { .. }` through `v[v.len() - 1]`
  (or `v[0]` for `first_mut`) when only `x` reaches the element
  (B-2026-11-08-46).

- A `select` that is a function's tail value no longer draws `must_use`
  warnings on the statements it is rewritten into, and a warning from a
  library body (`recv_timeout`'s) no longer reaches a program's output at a
  line its file does not have; a discarded `select` still warns, once
  (B-2026-11-08-75).

- An undefined `Deque`, `ArrayDeque`, `deque`, `Queue` or `LinkedList` names
  `VecDeque`, and `karac fix` writes it, as `HashMap` names `Map`
  (B-2026-11-08-59).
- **A method that changes a slice in place, called on a local bound from a
  view, names the binding and carries `.to_vec()`.** `let mut b =
  s.bytes(); b.sort()` said only "Slice.sort() requires a mutable slice";
  it now says `let mut` does not make a view writable, and `karac fix`
  writes `s.bytes().to_vec()`, as for `b[0] = 120`, which was reported
  twice and is now reported once (B-2026-11-08-64).

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
- The same refusal from a literal bound in a tuple (`let mut (l, res) =
  (0, 0)`) or filling an array or vector (`[0; 101]`, `Vec.filled(n, 0)`,
  read back as `v[i]`), and a `min` / `max` argument refused beside such a
  receiver (`res.max(x as i32)`), now offer the literal's suffix, `0i32`
  (B-2026-11-08-53).
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
- An error whose message carried its own `error[E_NAME]:` printed two
  headers on one line (`error[typecheck]: f.kara:2:1: error[E_SECRET_TRAIT_FORBIDDEN]: ...`).
  That code is now the one header, `error[E_SECRET_TRAIT_FORBIDDEN]: f.kara:2:1: ...`,
  and `--output=json` gives it as `symbol`, with the message after it
  (B-2026-11-08-41).

### Tooling

- A `let` binding after a `#[derive]`d type had no type on hover: the
  derive's items were spliced in numbered from zero, so the program's own
  nodes after the type were renumbered away from the ids the type checker's
  tables are keyed by. Spliced items now take fresh ids.

- `karac fmt` keeps a comment beside the parameter or call argument it
  follows: such a list is laid out one element per line, where the comment
  used to move into the body or to the end of the statement
  (B-2026-11-07-44).

- `kara-lsp` completion checks the buffer against the library it reaches,
  as `karac check` does, and takes what the rest of the library offers
  from the whole library checked once a process: completion on a 5-line
  buffer went from 120-165 ms to about 32 ms after the first request
  (about 190 ms), with the same entries (B-2026-11-09-28).

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
