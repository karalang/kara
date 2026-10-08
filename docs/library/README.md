# The Kāra standard library

This directory specifies the standard library's public API: its types, functions, methods, their signatures and their effects. The language itself is specified in [design.md](../design.md) and [core-semantics.md](../core-semantics.md). A library file states what a call does; the rules it relies on (moves, views, drops, effects, panics) are linked, not restated.

| File | Covers |
|---|---|
| [core-types.md](core-types.md) | `Option` and `Result` methods; numeric methods, parsing and printing |
| [collections.md](collections.md) | `Array`, `Vec`, `Slice`, `Map`, `Set`, `SortedMap`, `SortedSet`, `VecDeque`, `PriorityQueue`, `from` constructors, fallible allocation, hashing |
| [iterators.md](iterators.md) | Iterator adaptors and terminal methods, `FromIterator` |
| [strings.md](strings.md) | `String` and `Str` methods, normalization, UTF-8 conversion, `CStr` and `CString` |
| [io.md](io.md) | Standard streams, files, buffered I/O, TCP, process exit |
| [concurrency.md](concurrency.md) | Channels, `TaskGroup`, `Mutex`, atomics |
| [time-random-env.md](time-random-env.md) | `Clock`, `RandomSource` and `Env` APIs |
| [log.md](log.md) | Structured logging (`std.log`) |
| [secret.md](secret.md) | `Secret[T]` (post-v1 library) |

## How the library is built

The standard library is ordinary Kāra code. The compiler knows a closed list of lang-item traits (`Copy`, `Drop`, the operator traits, `Index`/`IndexMut`/`IndexSet`, `Iterator` and `Iterable`, the `?` protocol and `Display`; see [design.md §9](../design.md#lang-item-traits)) and a small set of intrinsics for memory, arithmetic, hashing, UTF-8 and runtime calls ([design.md §15](../design.md#intrinsics)). Everything else is written in Kāra on top of them: `Clone`, `Default`, `From`/`Into`, `IntoIterator`, every iterator adaptor, every collection method.

Two consequences follow:
- A user type gets the same surface as a library type. A user `impl Iterator` gets every adaptor in [iterators.md](iterators.md).
- Accessors return views. A Kāra body may build `Option[ref T]` (`Some(ref self.items[0])`), so `get`, `first`, `last` and `peek` return `Option[ref T]` rather than a copy. What a view may and may not do is [core-semantics.md §5](../core-semantics.md#5-references-and-views-c5).

## Signature conventions

- **Parameter modes** follow [design.md §7](../design.md#parameter-modes). A bare parameter `x: T` borrows its argument (for a `Copy` type it is a copy). `x: own T` takes ownership, and is written only where the function stores, moves or consumes the value: `fn push(mut ref self, val: own T)`. `x: mut ref T` borrows exclusively; a call that passes a `mut ref` argument marks it `mut` at the call site, and a method receiver is not marked.
- **Receivers.** A method that only reads takes `self`, a borrow. A method that grows or changes the value takes `mut ref self`. A method that consumes the value takes `own self`: `unwrap`, `into_iter`, `join`, and the iterator adaptors and terminal methods.
- **Read-only text and sequences** are taken as `Str` and `Slice[T]`. A `String` argument passes as `Str`, and a `Vec[T]` or `Array[T, N]` argument passes as `Slice[T]`.
- **Function parameters** use the kinds `Fn`, `MutFn` and `OnceFn` ([core-semantics.md §9.6](../core-semantics.md#9-closures)). A parameter that is called once is `OnceFn`; one called repeatedly is `MutFn`. Since `Fn` is accepted where `MutFn` or `OnceFn` is expected, these are the most permissive kinds for the caller. A borrowed `MutFn` or `OnceFn` cannot be called, so such a parameter is written `own`: `f: own OnceFn() -> T`. Function parameters are non-escaping unless written `escaping`, and an `escaping` parameter is owned. Inside a function type, `own` marks an argument the closure receives ownership of (`MutFn(own Self.Item) -> U`); a bare argument is lent to it. A lazy iterator adaptor, which stores its function, takes it as a generic parameter instead (`f: own F` with `F: MutFn(own Self.Item) -> U`; see [iterators.md](iterators.md#how-the-methods-work)).
- **Named parameters** follow a `;` in the parameter list, and only they may have defaults: `fn new(; limit: i64 = i64.MAX) -> TaskGroup` is called `TaskGroup.new()` or `TaskGroup.new(limit: 8)` ([design.md §7](../design.md#named-and-default-parameters)).
- **Sizes, counts, indexes and offsets are `i64`**, I/O byte counts included.
- **Effects** follow [design.md §12](../design.md#12-effects). Signatures list the effects a caller must know about: resource effects such as `reads(FileSystem)` or `sends(self)`, and `blocks`. `allocates(Heap)` and `panics` are default-permitted, so signatures here omit them; a "panics if ..." note marks a method that can panic. A function-typed parameter needs no effect annotation: a call has the effects of the argument passed ([core-semantics.md §12](../core-semantics.md#12-effects-soundness-defaults-c10)).
- **Not yet implemented** marks an item the language promises but the compiler does not yet provide.

## Layers: `core`, `alloc`, `std`

The library is split into three layers, each depending only on the ones before it:

- **`core`** needs no OS and no allocator. It holds the primitive types, `Option[T]`, `Result[T, E]`, `Array[T, N]`, the traits, and math (`sin`, `cos`, `sqrt` and the rest, through libm).
- **`alloc`** needs a heap allocator. It holds `Vec[T]`, `Map[K, V]`, `String` and the other collections, `f"..."` interpolation (which produces a heap-allocated `String`), and `shared` types (whose handles are heap-allocated).
- **`std`** needs an OS. It holds file I/O, networking, environment access, and the channel and task runtime.

An ordinary program uses all three. Running with only `core` or `core` + `alloc` is part of the systems track ([deferred.md](../deferred.md#systems)).

## The prelude

The prelude is the set of library names in scope in every module without an `import`. The mechanism (a synthetic lowest-precedence wildcard import that any explicit import shadows) is [design.md §4](../design.md#4-modules-and-packages). The prelude holds core types and traits only:

- **Types:** `Option`, `Result`, `Ordering`, `String`, `Str`, `Array`, `Vec`, `Slice`, `Map`, `Set`, `SortedMap`, `SortedSet`, `VecDeque`, `PriorityQueue`, `Entry`. The primitive types are keywords and need no prelude.
- **Variants:** `Some`, `None`, `Ok`, `Err`, which expressions write bare. Other enums' variants are written qualified in expressions (`Ordering.Less`, `NormalizationForm.Nfc`), and may be written bare in patterns ([design.md §5](../design.md#enums)).
- **Traits:** the lang-item traits; `Clone`, `Default`, `From`, `Into`, `TryFrom`, `TryInto`; `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`; `Debug`, `Display`; `Iterable`, `Iterator`, `IntoIterator`, `FromIterator`; `Error`.
- **Error types:** `AnyError`, the erased error ([design.md §10](../design.md#error-and-anyerror)); and the error types the core methods return: `IoError`, `VarError`, `AllocError`, `ParseError`, `Utf8Error`.
- **Other types the core methods take:** `NormalizationForm`.
- **Functions:** `print`, `println`, `eprintln`. Each takes any `Display` value; `print` and `println` have `writes(Stdout)` and `eprintln` has `writes(Stderr)`.
- **Builtins:** `todo`, `unreachable`, `panic`, `dbg`, `assert`, `assert_eq`, `assert_ne`. Their bodies are compiler-provided because they capture source locations or have the `Never` type; their signatures are declared in library source so documentation and tools see them. `assert`, `assert_eq` and `assert_ne` are usable everywhere, production code included ([design.md §14](../design.md#assertions)).
- **Resource aliases:** `stdin`, `stdout`, `stderr`, `env` and `fs`, the lowercase names of the built-in resources `Stdin`, `Stdout`, `Stderr`, `Env` and `FileSystem`. Their functions are reached through them (`stdin.read_line()`, `env.args()`); see [io.md](io.md) and [time-random-env.md](time-random-env.md).
- **Modules:** `ptr`, for raw-pointer construction inside `unsafe` ([design.md §15](../design.md#15-unsafe-ffi-and-layout-control)).

Everything else is imported from a `std.*` module:

| Module | Items |
|---|---|
| `std.sync` | `channel`, `Sender`, `Receiver`, `SendError`, `TaskGroup`, `TaskHandle`, `Mutex`, `MutexGuard`, `Atomic` |
| `std.time` | `Clock`, `Instant`, `SystemTime`, `Duration`, `sleep` |
| `std.random`, `std.uuid` | `next_u64`; `v4` |
| `std.io` | `File`, `SeekFrom`, `BufReader`, `BufWriter` |
| `std.net` | `TcpListener`, `TcpStream`, `TcpError` |
| `std.hash` | `Hasher`, `BuildHasher`, `SipHash13BuildHasher`, `FxBuildHasher`, `StableHash` |
| `std.iter` | `DoubleEndedIterator`, `Extend`, the adapter structs |
| `std.process` | `exit` |
| `std.log` | `debug`, `info`, `warn`, `error`, `init`, `Backend`, `Record`, `json_backend`, `text_backend` |
| `std.secret` | `Secret`, `ConstantTimeEq`, `Zeroize` |

Domain names (an HTTP `Response`, a parser's `Parser` and `Span`, regex and JSON types) live under `std.*` as well and are never in the prelude. A local binding may shadow any prelude name; a lint flags the likely-unintended cases.
