# The Kāra standard library

This directory specifies the standard library's public API: its types, functions, methods, their signatures and their effects. The language itself is specified in [design.md](../design.md) and [core-semantics.md](../core-semantics.md). A library file states what a call does; the rules it relies on (moves, views, drops, effects, panics) are linked, not restated.

| File | Covers |
|---|---|
| [core-types.md](core-types.md) | `Option` and `Result` methods; numeric methods, parsing and printing |
| [collections.md](collections.md) | `Array`, `Vec`, `Slice`, `Map`, `Set`, `SortedMap`, `SortedSet`, `VecDeque`, `PriorityQueue`, `from` constructors, fallible allocation, hashing |
| [iterators.md](iterators.md) | Iterator adaptors and terminal methods, `FromIterator` |
| [strings.md](strings.md) | `String` and `Str` methods, normalization, UTF-8 conversion, `CStr` and `CString` |
| [path.md](path.md) | `std.path`: lexical path functions on `Str` |
| [encoding.md](encoding.md) | `std.encoding`: Base64 and hex |
| [crypto.md](crypto.md) | `std.crypto`: SHA-2, HMAC, constant-time comparison, legacy SHA-1 |
| [json.md](json.md) | `Json` values: `parse` and `stringify` |
| [io.md](io.md) | Standard streams, files, buffered I/O, TCP, process exit |
| [regex.md](regex.md) | Regular expressions: `Regex`, `Match`, `Captures` |
| [concurrency.md](concurrency.md) | Channels, `TaskGroup`, `Mutex`, atomics |
| [pool.md](pool.md) | `std.pool`: connection pools, `Pool`, `PooledConnection`, `PoolError` |
| [time-random-env.md](time-random-env.md) | `Clock`, `RandomSource` and `Env` APIs |
| [log.md](log.md) | Structured logging (`std.log`) |
| [secret.md](secret.md) | `Secret[T]` (post-v1 library) |
| [data.md](data.md) | The `kara-data` package: `Column`, `DataFrame`, expressions, `LazyFrame`, `Stats`, CSV and Arrow IPC (post-v1 package) |

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
- **Effects** follow [design.md §12](../design.md#12-effects). Signatures list the effects a caller must know about: resource effects such as `reads(FileSystem)` or `sends(self)`, and `blocks`. `allocates(Heap)` and `panics` are default-permitted, so signatures here omit them; a "panics if ..." note marks a method that can panic, and [Panics](#panics) gives each one's condition and kind. A function-typed parameter needs no effect annotation: a call has the effects of the argument passed ([core-semantics.md §12](../core-semantics.md#12-effects-soundness-defaults-c10)). A method bounded on a trait of the values it holds (`where T: PartialEq`), a keyed collection's methods, and `println`, an f-string hole and `assert_eq` likewise need none: each has the effects of that trait's impl for the type at the call ([collections.md](collections.md), Element traits).
- **Not yet implemented** marks an item the language promises but the compiler does not yet provide.

## Panics

A library call that panics produces a panic record whose **kind** is part of the contract and whose **message** is informative: a tool keys on the kind and the location, and the message may be reworded between releases ([design.md § Panic record](../design.md#panic-record)). Each row gives the condition, the kind, and the message every backend prints today. A page's "panics if ..." note refers here.

| Operation | Panics when | Kind | Message today |
|---|---|---|---|
| `v[i]`, `v[i] = x`, `v.swap(i, j)`, `v.insert(i, x)`, `v.remove(i)`, `v.split_at(i)`, `v.split_off(i)` on `Vec`, `Array`, `Slice`; `VecDeque.insert` / `remove` | the index is negative or past the end (`insert` and `split_*` allow the end itself) | `index_out_of_bounds` | `index out of bounds` |
| `v[a..b]` on a sequence | `a > b`, or `b` is past the end | `index_out_of_bounds` | `index out of bounds` |
| `s.bytes()[i]` | `i` is out of range | `index_out_of_bounds` | `index out of bounds` |
| `s[a..b]`, `s.split_at(i)` on text | an end falls inside a multi-byte char | `index_out_of_bounds` | `byte index N is not a char boundary` (N is the first end that cuts a char; the text is not quoted) |
| `s.substring(a, b)` | an end falls inside a multi-byte char | `explicit` | the same message as `s[a..b]` |
| `s.truncate(n)` on a `String` | `n` falls inside a multi-byte char | `explicit` | `String.truncate: the length is not on a UTF-8 char boundary` |
| `v.drain(a..b)` | the range is inverted or past the end | `index_out_of_bounds` | `drain range out of bounds` |
| `v.rotate_left(mid)` | `mid` is past the end | `explicit` | `rotate_left: mid out of bounds` |
| `dst.clone_from_slice(src)` | the lengths differ | `explicit` | `clone_from_slice: destination and source slices have different lengths` |
| `v[a..b] = w` | `w`'s length differs from the range's | `explicit` | `range assignment: the value's length differs from the range's` |
| `m[k]` on `Map` or `SortedMap` | `k` is not a key (`get` returns `None` instead) | `explicit` | `key not found` |
| `windows(n)`, `chunks(n)` | `n <= 0` | `explicit` | `windows: size must be positive`, `chunks: size must be positive` |
| `step_by(n)` | `n <= 0` | `explicit` | `step_by: step must be positive` |
| `Array.from_fn(n, f)` | `n` is not the array's length | `explicit` | `Array.from_fn: n is not the array's length` |
| `c.is_digit(r)`, `c.to_digit(r)` | `r` is outside `2..=36` | `explicit` | `char.is_digit: radix 40 is not in 2..=36` |
| `+`, `-`, `*`, unary `-`, `pow`, `abs` on integers; `MIN / -1`; a `RangeFrom` stepped past `MAX` | the result does not fit the type | `overflow` | `integer overflow` |
| `<<`, `>>` | the shift amount is negative or at least the width | `overflow` | `shift amount out of range` |
| `/`, `%` on integers; `Duration / 0` | the divisor is zero | `div_by_zero` | `division by zero` |
| `unwrap()` | `None`, or `Err` | `unwrap` | `called unwrap() on None`, `called unwrap() on Err` |
| `unwrap_err()` | `Ok` | `unwrap` | `called unwrap_err() on Ok` |
| `expect(msg)`, `expect_err(msg)` | as `unwrap` / `unwrap_err` | `unwrap` | `msg` alone |
| `assert(c)`, `assert_eq(a, b)`, `assert_ne(a, b)` | the assertion is false | `assertion` | `assertion failed: <expr>`, then `left:` / `right:` (or `value:`) lines; a given message replaces the first line |
| `unreachable()` | it is reached | `unreachable` | `entered unreachable code` |
| `todo()` | it is reached | `explicit` | `not yet implemented` |
| `panic(msg)` | always | `explicit` | `msg` |
| a `requires` clause | the clause is false at the call | `explicit` | `contract violated: requires clause` |
| a borrow of a `shared` value's `mut` field | another live borrow conflicts with it at run time | `borrow_conflict` | `already borrowed: ...` |
| `recv()` on sequential `wasm_wasi` | no task can run to send | `deadlock` | `deadlock: ...` |
| an allocation | the allocator fails or the size overflows | `alloc_failure` | see B-2026-11-16-317 |

Several operations that look as if they might panic return instead, as their pages say: `Vec.filled` and `Vec.with_capacity` with `n < 0` give an empty `Vec`, `substring` saturates out-of-range bounds, `pop`, `first` and `min` on an empty sequence return `None`, `parse` returns `Err`, and a float-to-integer `as` cast saturates.

## Layers: `core`, `alloc`, `std`

The library is split into three layers, each depending only on the ones before it:

- **`core`** needs no OS and no allocator. It holds the primitive types, `Option[T]`, `Result[T, E]`, `Array[T, N]`, the traits, and math (`sin`, `cos`, `sqrt` and the rest, through libm).
- **`alloc`** needs a heap allocator. It holds `Vec[T]`, `Map[K, V]`, `String` and the other collections, `f"..."` interpolation (which produces a heap-allocated `String`), and `shared` types (whose handles are heap-allocated).
- **`std`** needs an OS. It holds file I/O, networking, environment access, and the channel and task runtime.

An ordinary program uses all three. Running with only `core` or `core` + `alloc` is part of the systems track ([deferred.md](../deferred.md#systems)).

## The prelude

The prelude is the set of library names in scope in every module without an `import`. The mechanism (a synthetic lowest-precedence wildcard import that any explicit import shadows) is [design.md §4](../design.md#4-modules-and-packages). The prelude holds core types and traits only:

- **Types:** `Option`, `Result`, `Ordering`, `String`, `Str`, `Array`, `Vec`, `Slice`, `Map`, `Set`, `SortedMap`, `SortedSet`, `VecDeque`, `PriorityQueue`, `Entry`; `Never`, the type of an expression that does not return ([design.md §5](../design.md)); `ExitCode`, what `main` may return; `CStr` and `CString`, the C strings of [strings.md](strings.md). The primitive types are keywords and need no prelude.
- **Variants:** `Some`, `None`, `Ok`, `Err`, which expressions write bare. Other enums' variants are written qualified in expressions (`Ordering.Less`, `NormalizationForm.Nfc`), and may be written bare in patterns ([design.md §5](../design.md#enums)).
- **Traits:** the lang-item traits; `Clone`, `Default`, `From`, `Into`, `TryFrom`, `TryInto`; `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`; `Debug`, `Display`; `Iterable`, `Iterator`, `IntoIterator`, `FromIterator`; `Error`.
- **Error types:** `AnyError`, the erased error ([design.md §10](../design.md#error-and-anyerror)); and the error types the core methods return: `IoError`, `VarError`, `AllocError`, `ParseError`, `Utf8Error`, `DiscriminantError` (an integer that names no variant) and `NulError` (a `CString` built from text holding a NUL).
- **Other types the core methods take:** `NormalizationForm`.
- **Functions:** `print`, `println`, `eprint`, `eprintln`. Each takes any `Display` value; `print` and `println` have `writes(Stdout)` and `eprint` and `eprintln` have `writes(Stderr)`. The `ln` forms add a newline (B-2026-11-20-53). And `drop`, `fn drop[T](x: own T) {}`, which ends a value's life early: the value moves in and drops when `drop` returns, so a `BufWriter` flushes and a file closes there ([core-semantics.md §7](../core-semantics.md)). It is an ordinary function and adds no rule (B-2026-11-09-72).
- **Builtins:** `todo`, `unreachable`, `panic`, `dbg`, `assert`, `assert_eq`, `assert_ne`. Their bodies are compiler-provided because they capture source locations or have the `Never` type; their signatures are declared in library source so documentation and tools see them. `assert`, `assert_eq` and `assert_ne` are usable everywhere, production code included ([design.md §14](../design.md#assertions)).
- **Resource aliases:** `stdin`, `stdout`, `stderr`, `env` and `fs`, the lowercase names of the built-in resources `Stdin`, `Stdout`, `Stderr`, `Env` and `FileSystem`. Their functions are reached through them (`stdin.read_line()`, `env.args()`); see [io.md](io.md) and [time-random-env.md](time-random-env.md).
- **Modules:** `ptr`, for raw-pointer construction inside `unsafe` ([design.md §15](../design.md#15-unsafe-ffi-and-layout-control)).

Everything else is imported from a `std.*` module:

| Module | Items |
|---|---|
| `std.sync` | `channel`, `Sender`, `Receiver`, `SendError`, `TaskGroup`, `TaskHandle`, `IsolatingGroup`, `TaskFailed`, `Mutex`, `MutexGuard`, `Atomic`, `MemoryOrdering`, `Semaphore`, `SemaphoreError`, `RateLimiter`, `BoundedChannel`, `OnFull`, `ChannelError`, `Channel` (the legacy handle pair); experimental: `OnceLock`, `OnceCell`, `AlreadySetError`, `LazyLock` |
| `std.time` | `Clock`, `ClockProvider`, `Instant`, `SystemTime`, `Duration`, `sleep` |
| `std.env` | `EnvProvider` |
| `std.random`, `std.uuid` | `next_u64`, `RandomProvider`; `v4` |
| `std.io` | `File`, `OpenOptions`, `SeekFrom`, `BufReader`, `BufWriter`, `Metadata`, `LinesIter`, `StdinLines`, `StdoutProvider`, `StderrProvider`, `StdinProvider`, `FileSystemProvider` |
| `std.net` | `TcpListener`, `TcpStream`, `UdpSocket`, `NetError` (and its alias `TcpError`); HTTP: `Client`, `Request`, `RequestBuilder`, `Response`, `HttpError`, `Server`, `WebSocket`; TLS: `TlsConfig`, `TlsListener`, `TlsStream`, `TlsError` ([io.md](io.md)) |
| `std.hash` | `Hasher`, `BuildHasher`, `SipHash13BuildHasher`, `FxBuildHasher`, `StableHash` |
| `std.cmp` | `Reverse` ([core-types.md](core-types.md#reverse)), `F32`, `F64`, `F16`, `Bf16` (the total-order float wrappers) |
| `std.iter` | `DoubleEndedIterator`, `ExactSizeIterator`, `Extend`, the adapter structs |
| `std.process` | `exit`, `Command`, `Stdio`, `Child`, `ChildStdin`, `ChildStdout`, `ChildStderr`, `ExitStatus`, `Output` |
| `std.regex` | `Regex`, `RegexError`, `Match`, `Captures` |
| `std.pool` | `Pool`, `PooledConnection`, `PoolError` |
| `std.path` | `parent`, `file_name`, `extension`, `stem`, `join`, `normalize`, `is_absolute`, `components` |
| `std.encoding` | `base64_encode`, `base64_decode`, `base64url_encode`, `base64url_decode`, `hex_encode`, `hex_decode`, `percent_encode`, `percent_decode`, `DecodeError` |
| `std.crypto` | `constant_time_eq` |
| `std.crypto.sha2` | `sha256`, `sha384`, `sha512`, `Sha256`, `Sha384`, `Sha512` |
| `std.crypto.hmac` | `hmac_sha256`, `hmac_sha384`, `hmac_sha512`, `HmacSha256`, `HmacSha384`, `HmacSha512` |
| `std.crypto.legacy` | `sha1`, `Sha1` |
| `std.log` | `debug`, `info`, `warn`, `error`, `init`, `Level`, `Record`, `Backend`, `Stream`, `json_backend`, `text_backend` |
| `std.secret` | `Secret`, `ConstantTimeEq`, `Zeroize` |
| `std.arena` | `Arena`, `ArenaRef`, `ArenaCheckpoint` ([collections.md](collections.md)) |
| `std.intern` | `Symbol`, `Interner`: an `Interner` maps each distinct string to a small `Symbol` and back, so repeated names compare and hash as integers |
| `std.runtime` | `Runtime`, `has_debug_metadata`, `list_tasks`, `list_par_blocks`, `TaskInfo`, `ParBlockInfo`, `WaitTarget` ([design.md § Debugger contract](../design.md#debugger-contract)) |

Experimental modules: implemented in 0.8 and imported like any other, but their surface may change before v1 ([deferred.md](../deferred.md)).

| Module | Items |
|---|---|
| `std.simd` (experimental) | `Vector` |
| `std.gpu` (experimental) | `GpuBuffer` |
| `std.embedded` (experimental) | `CriticalSectionGuard` |
| `std.tracing` (experimental) | `Span`, `LogEvent`, `SpanField`, `NoOpExporter`, `StdoutExporter`, `Log` |
| `std.protobuf` (experimental) | `ProtoBuf`, `ProtoReader` |

`VolatileCell` checks but runs on no backend, so it is deferred and not in 0.8 ([deferred.md § Volatile access](../deferred.md#volatile-access-inline-assembly-interrupts)).

The data track (`Tensor`, `Column`, `DataFrame`, `LazyFrame`, `LazyExpr`, `LazyGroupBy`, `Stats`) is the post-v1 `kara-data` package and is not in 0.8 ([data.md](data.md), [deferred.md § M4c](../deferred.md)).

Domain names (an HTTP `Response`, a parser's `Parser` and `Span`, JSON types) live under `std.*` as well and are never in the prelude. A local binding may shadow any prelude name; a lint flags the likely-unintended cases.
