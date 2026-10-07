# Kāra Language Design

**How to read the specification.** It is four documents:

| Document | Normative for |
|---|---|
| [`core-semantics.md`](core-semantics.md) | Values and moves, places, parameters and calls, patterns, references and views, sharing, destruction, evaluation order, closures, panics, concurrency safety, and the soundness defaults of effects. Every rule there is pinned by a corpus program. |
| This file | Everything else about the language: lexical structure, modules and packages, types, expressions, functions, inference and generics, traits, errors, the ownership and effect surface, concurrency constructs, testing, unsafe code and FFI, targets, and the tooling contract. |
| [`library/`](library/README.md) | The standard library's APIs. |
| [`deferred.md`](deferred.md) | Designs that are not in v1, grouped by the track that brings them back. |

Where this file and `core-semantics.md` disagree, `core-semantics.md` wins, and the disagreement is a bug in this file.

**In one sentence:** Kāra is an AI-first systems language in which ownership, sharing and effects are explicit at function signatures, so every function can be checked and understood on its own; the compiler verifies and explains those decisions, and parallelizes code only where the effects prove it safe.

**Semantic hierarchy.** The language has three layers, in order of primacy:

1. **Values and types** define structure.
2. **Effects** define observable behavior: what a function reads, writes, sends and receives, and whether it may block, allocate or panic.
3. **Ownership** defines aliasing, lifetimes and the movement of values.

Effects say what behavior is permitted; ownership governs how that behavior is realized. Effects are the primary interface: they are declared on public functions, inferred on private ones, and verified against every body.

---

## 1. What Kāra is

### Starting assumptions

1. **AI writes most code.** Optimize for machine readability, local reasoning, unambiguous semantics, and diagnostics a program can act on. A diagnostic that comes with a machine-applicable fix (`karac fix`) is worth more than one that explains.
2. **Everything a reader needs is in the signature.** Parameter modes, returned references, sharing and public effects are written at the declaration, so a function body can be checked against its signature alone and a caller never needs to read the body.
3. **Performance comes from values, not from a runtime.** No garbage collector, no implicit reference counting, no hidden copies: duplication is written `.clone()`, and sharing is a type the programmer chooses. Generics are monomorphized.
4. **Take the best from everything.** No loyalty to any paradigm.
5. **Safe by default, unsafe by opt-in.** The language is memory-safe and effect-checked. Raw pointers and unchecked FFI require `unsafe` and declared effects.

"Memory-safe" covers five properties. All five hold in safe code on every target.

| Property | Guarantee | Mechanism |
|---|---|---|
| **Temporal safety** | No use after free, no dangling references | Moves are checked at compile time (use after move is an error), references are decided by signatures, and views cannot escape their origins ([core-semantics.md §3, §5](core-semantics.md)). |
| **Type safety** | No type confusion | No implicit conversions except lossless numeric widening ([§5](#5-types)) and reading a `Copy` value through a reference ([core-semantics.md §5.10](core-semantics.md#5-references-and-views-c5)). `unsafe` is the only escape. |
| **Data race freedom** | No concurrent write with another access to the same memory | The branches of `par {}` and `par for` are verified not to conflict on places or resources, and a conflict is a compile error ([core-semantics.md §11.2](core-semantics.md)). `TaskGroup` tasks follow the borrow rules: while a task borrows a place, nothing else writes it ([core-semantics.md §9.5](core-semantics.md)). Only `CrossTask` values enter a task: owned values move in, `sync` handles are atomically counted, `frozen` values are read-only, and plain `shared` handles never cross ([core-semantics.md §6.3, §11.3](core-semantics.md)). |
| **Spatial safety** | No out-of-bounds reads or writes | Bounds checks in safe code, emitted so the optimizer can remove provably redundant ones; `unsafe { xs.get_unchecked(i) }` covers the rest. |
| **Integer overflow** | Defined behavior, never undefined | Overflow traps, which is a panic. Wrapping, saturating and checked arithmetic are named at the call site ([§5](#5-types)). |

### v1 scope

**v1 is the core language plus native command-line and compute programs and plain `wasm32`.** It has two pillars:

- **AI-first tooling.** Structured JSON diagnostics with machine-applicable fixes, `karac fix`, `karac explain`, the compiler query API, and per-phase diagnostic codes ([§17](#17-tooling-contract)).
- **Effect-verified concurrency.** Explicit in v1: `par {}` and `par for` are checked against effects, and a conflict is an error. `TaskGroup` runs dynamic tasks, such as a server's request handlers, which cannot race on memory but run in no fixed order ([§13](#13-concurrency)). Automatic where proven safe from M4: the compiler parallelizes loops and independent statements only when effects are fully known and sound, with no unknown callees and no unattributed drops. Automatic parallelism is the long-term differentiator.

**Panics abort the process** ([core-semantics.md §10](core-semantics.md)). There is no unwinding and no `catch_panic`. A service should run under a supervisor that restarts it; task-level failure isolation arrives with the services track, and the compiler's middle end already reserves the cleanup edges it needs.

**The v1 release follows the services and automatic-parallelism tracks** (M4a and M4b below). Parity with the old compiler (M3) is an internal milestone, not the release.

### Tracks

Features outside the v1 core return as named tracks. A track enters this specification when it has corpus programs; until then its design lives in [`deferred.md`](deferred.md).

| Track | Brings | When |
|---|---|---|
| **Services (M4a)** | Coroutines and the `suspends` effect, non-blocking networking, one scheduler and reactor, cooperative cancellation and deadlines, channel `select` with timeouts, task-level failure isolation, providers for test doubles, effect variables for stored callbacks, `dyn`, and effect groups if the services programs show they are needed. Gate: the services programs in the corpus, plus a load test of connections and tail latency. Blocking-I/O services already run in v1. | Before the v1 release |
| **Automatic parallelism (M4b)** | Loop and statement auto-parallelism as compiler passes under the proof bar, and `seq {}` to keep code in order. Gate: measured speedup with zero output changes across the whole corpus. | Before the v1 release |
| **Data (M4c)** | The `kara-data` package: Tensor, Column, DataFrame, statistics, autograd, Arrow. | After v1 |
| **Later tracks** | Systems (profiles, interrupts, volatile access, inline assembly, linker control, exported C ABI, SIMD), layout blocks, verification (predicates on distinct types, contracts), effect expressiveness (other named effect variables, parameterized resources), interactive (REPL, notebooks, playground), web (browser glue, component model), `comptime`, GPU, self-hosting. | Ordered in the roadmap |

### What Kāra is not

Deliberate design boundaries, not missing features.

**No named lifetime parameters.** No `'a`, no lifetime bounds. A returned `ref` borrows from `self` when the function has a borrowed `self` or `mut ref self` receiver, and otherwise from every borrowed parameter. A type that contains a reference is a view, and views cannot escape ([core-semantics.md §5](core-semantics.md)). No annotation is ever needed.

**No `async fn` and no `.await`.** When coroutines arrive with the services track, suspension is an inferred effect (`suspends`), declared on public functions like any other effect. A function that waits on the network looks like one that computes.

**No garbage collector and no implicit reference counting.** Values are destroyed at the end of their scope, in reverse declaration order ([core-semantics.md §7](core-semantics.md)). Sharing exists only where the programmer writes a `shared`, `sync` or `frozen` type.

**No cycle collector.** A cycle of strong `shared` handles leaks. Back-edges use `weak`.

**No algebraic effect handlers.** Effects are a static contract, not a control-flow mechanism.

**No field-level effect granularity.** `writes(self.cache)` names a value, not a field path inside it. Finer partitioning (parameterized resources, and field-level effects on `sync` types) is deferred to the [effects expressiveness](deferred.md#effects-expressiveness) track.

**No classes or inheritance.** Composition through structs, enums, traits and `impl` blocks.

**No exceptions and no unwinding.** Errors are values (`Result[T, E]` with `?`). A panic ends the process.

**No silent serialization of conflicting work.** Two `par` branches that conflict on a place or resource are a compile error, not quietly run in order.

**No universal-purpose pitch.** Kāra is for programs where correctness and performance both matter and much of the code is written by machines: services, command-line tools, data pipelines and libraries. It is not trying to replace Python or JavaScript for their core uses.

---

## 2. Specification layers

Every statement in this specification belongs to one of three layers. The layer decides what a conforming compiler must do, what it must be able to explain, and what it is free to change.

1. **Guaranteed semantics.** The program's meaning. The same across conforming compilers, compiler versions and targets. Code relying on a guaranteed statement is correct forever; a change that breaks it is a compiler bug. Everything in `core-semantics.md` is in this layer.
2. **Reported behavior.** Facts the compiler must be able to explain through `karac explain`, diagnostics or queries, but that are not part of the program's meaning. Stable within a compiler release; may change across releases. Tools that consume them must tolerate version skew. A change is not a bug, but it may need a changelog entry.
3. **Implementation freedom.** Choices the compiler may make however it likes, differently on every run. Not observable through any supported interface.

**Why reported behavior is unstable across releases.** Without that escape hatch, any fact the compiler exposes becomes guaranteed the moment a tool depends on it, and the compiler loses the freedom to improve.

**Rule for new sections.** Every new feature states its layer or cites the section that does. Older sections are classified when a real question forces it; until then, treat unclassified behavior as implementation freedom.

### Seed classification

**Guaranteed semantics:**

- **The effect set of a public function is part of its signature.** A caller that type-checks against a declared effect set keeps type-checking against it across compiler versions ([§12](#12-effects)). "Public" means `pub`, the visibility level that crosses the package boundary ([§4](#4-modules-and-packages)).
- **The declared parameter mode** of every parameter. Signatures always name the mode: a bare parameter borrows, `own T` is owned, and `mut ref T` borrows mutably ([§7](#parameter-modes)). Body edits that would need a different mode fail at the declaration, so the mode cannot drift.
- **`#[no_effect(...)]` and profile restrictions.** A function they cover never performs the excluded effect, transitively.
- **Ordering within one resource instance.** Two accesses to the same resource instance are observed in source order. Two distinct instances (two files, two connections) are ordered independently. Cross-resource ordering is not guaranteed.
- **The meaning of `shared`, `sync` and `frozen` types** ([core-semantics.md §6](core-semantics.md)).
- **When destructors run and the order of evaluation** ([core-semantics.md §7, §8](core-semantics.md)).

**Reported behavior:**

- **The inferred effect set of a private function**, as shown by `karac explain`. The set is real and affects compilation, but no external caller can see it, so inference may improve, under two rules:
  - Within an edition, inference may only tighten (infer fewer effects) or stay the same. Broadening within an edition would break public declarations with no source change.
  - Across an edition, inference may broaden. Every broadening is itemized in the edition's release notes, `karac explain` shows what changed, and the author resolves a warning-then-error before the new edition compiles.
  - A private function with an explicit effect declaration is guaranteed like a public one.
- **The would-be parameter mode** that `karac explain` shows beside the declared one, for example an `own` parameter whose body only reads it, reported as "could be borrowed".
- **Diagnostic wording, error codes and the shape of `karac explain` output.** Codes come from per-phase bands, so one code names one diagnostic: parse `E00xx`, resolve `E01xx`, typecheck `E02xx`/`W02xx`, effects `E04xx`, ownership `E05xx` (notes `N05xx`), and `E08xx` shared by target errors with disjoint numbers. A new code is allocated from its phase's band. A few codes predate the bands and keep their numbers, because renumbering a shipping code breaks every consumer keyed on it. Tests in the compiler hold both invariants: no number is minted by two phases, and every code is catalogued for `karac explain`.
- **Monomorphization counts**: how many instances the compiler produces for a generic, shown by `karac query monomorphization`. The rule is guaranteed (one instance per distinct tuple of type arguments); the count may shrink across releases as instances are merged, and may not silently grow.
- **The compiler queries report** (below).

**Implementation freedom:**

- Monomorphization strategy: which instances are shared, duplicated or outlined.
- The order and placement in which `par` branches and `par for` iterations run once they are proven not to conflict, and in which `TaskGroup` tasks run: in any order, in parallel or one after another.
- The runtime's thread pool and scheduling strategy.
- Stack or heap placement of locals.
- Inlining, unrolling, vectorization and other optimizations.

The layers say how much of a feature's behavior is fixed, not which features exist.

### Compiler queries

Each compile produces a **queries report** (`karac query queries`, JSON) listing optimization decisions the compiler hedged on. The author, usually a model working between compiles, reads it, decides which to resolve, and writes resolution attributes on the items concerned. The next compile drops resolved queries.

The channel carries intent, not distribution data: "is this the hot path?", "should this generic specialize for `i64`?". Profile-guided optimization answers the distribution questions and is deferred.

**Each query carries:**
- a stable id: the definition id of the item, plus a structural hash for a slot inside it, so the id survives edits that do not change the decision;
- the decision site, with its source span;
- the options considered, with the compiler's reason for each;
- the default the compiler chose and its confidence;
- the resolution surface: which attribute, written where, pins the answer.

Resolutions are source attributes, reviewed and versioned like code. A clean codebase has no open query its author cares about, not zero queries: queries report opportunities, not mistakes, which remain lints. Author claims are trusted; verifier-backed resolution is deferred.

**Orphans.** When an item changes so that a resolved query no longer fires, the next compile reports the resolution as orphaned instead of stripping it.

**Layer.** The report is reported behavior. Resolution attributes are language items: once committed, an attribute keeps parsing across versions even if its query no longer fires.

**v1 commitment.** v1 ships the channel even if no query is wired through:
1. stable definition ids on every item, with a package segment, and structural-hash slots inside items;
2. a `queries` list on every phase's result, empty by default;
3. the `karac query queries` command.

**Planned queries** (none implemented): generic specialization (`#[specialize(T = i64)]`), inlining and branch hints (`#[inline]`, `#[likely]`; the hint attributes are deferred with the systems track), and effect-set narrowing at a public boundary.

**What the channel is not.** Not compiler remarks (those have no response surface and no stable ids). Not a schedule language. Not a model inside the compile loop: the author acts between compiles and every resolution is committed source. Not a lint pass.

---

## 3. Lexical structure

The lexical grammar is in [`syntax.md` §1](syntax.md), which describes the current parser. Where the two differ, this section governs. It states the rules and the reasons for them.

### Identifiers and naming

Kāra distinguishes identifiers by their **case class**, which is set by the pattern of ASCII letters in the identifier. The case class is a grammatical property: the compiler uses it to tell path expressions from value and field access, and to keep naming consistent across the ecosystem.

**Layer:** guaranteed semantics ([§2](#2-specification-layers)). The case class of an identifier is the same in every conforming compiler and compiler version.

#### Case classes

| Class | Pattern | Used for |
|---|---|---|
| **Type** | The first letter is uppercase, and either a later letter is lowercase or the identifier is exactly one letter | Structs, enums, enum variants, traits, type aliases, distinct types, generic type parameters, effect resources |
| **Const** | Every letter is uppercase (digits and `_` allowed) | `const` items ([Constants](#constants)) and const generic parameters ([§8](#const-generic-parameters)) |
| **Value** | The first letter is lowercase, or the identifier begins with `_` | Functions, `let` bindings, parameters, fields, module file names |

```kara
// Type class:
struct UserAccount { ... }
enum IoError { InvalidUtf8, NotFound }
trait Iterator { ... }

// Const class:
const MAX_RETRIES: i32 = 3;
const PI: f64 = 3.14159;

// Value class:
fn read_to_string(path: String) -> String { ... }
let user_count = 0;
// module file names are Value class too: src/db_client.kara defines the `db_client` module
```

#### Rules

1. **CN-1 (type names).** Every `struct`, `enum`, `trait`, `type`, `distinct type`, generic type parameter and `effect resource` declaration introduces a Type-class identifier. `struct user_account { ... }` is a compile error.
2. **CN-2 (value names).** Every `fn`, `let`, parameter, field and module file name introduces a Value-class identifier. `fn ReadFile() { ... }` is a compile error. Module file names follow this rule because the directory tree defines modules: `src/DbClient.kara` is a compile error; rename it `src/db_client.kara`.
3. **CN-3 (constant names).** Every `const` item and const generic parameter introduces a Const-class identifier. `const max_retries: i32 = 3;` is a compile error. `let` and `let mut` introduce Value-class identifiers.
4. **CN-4 (acronyms as words).** Multi-word Type identifiers treat acronyms as words: `HttpClient`, not `HTTPClient`; `IoError`, not `IOError`. `HTTPClient` is formally Type class (it has lowercase letters in `lient`), but a reader sees `HTTP` first and reads it as all caps. The rule makes the class reliable at a glance.
5. **CN-5 (leading underscore).** A Value-class identifier may begin with `_` (`_unused`, `_tmp`). The class is unchanged. The underscore marks the binding as intentionally unused and suppresses unused-binding diagnostics. Type-class and Const-class identifiers may not begin with `_`.
6. **CN-6 (enum variants).** Enum variants are Type class: `enum Status { Active, Inactive }`, not `{ active, inactive }`.
7. **CN-7 (single-letter type parameters).** A single uppercase letter (`T`, `K`, `V`, `E`, `R`) is Type class, by the one-letter case of the table above. Constants are always longer than one letter (`MAX`, `PI`), so there is no confusion. A const generic parameter is exempt: in `const N: i64` the `const` already says that `N` is a value, so a one-letter name there is Const class.
8. **CN-8 (FFI exception).** Foreign functions and types may have any ASCII name at the FFI boundary, but the Kāra-visible name must follow the rules above. `#[kara_name("...")]` rebinds a non-conforming foreign name. Foreign declarations live in an `unsafe extern "ABI" { ... }` block ([§15](#15-unsafe-ffi-and-layout-control)):

   ```kara
   unsafe extern "C" {
       fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8;
       // `memcpy` is Value class: no rename needed.

       #[kara_name("GlxFbConfig")]
       type GLXFBConfig;
       // A vendor type, rebound to a Type-class name on the Kāra side.
   }
   ```

**Primitive type names.** `i8` … `i128`, `u8` … `u128`, `f32`, `f64`, `bool`, `char` and the other primitive type names of [§5](#5-types) are lowercase, but they are reserved type names, not Value-class identifiers. They cannot be declared or bound as names: `let i64 = 3;` is an error. As the first segment of a path they always name the type: `i64.MAX`, `u32.size_of()`, `char.try_from(n)`. `Fn`, `MutFn` and `OnceFn` are reserved type names too.

#### Rationale

1. **Paths are unambiguous by grammar.** Path segments and value or field access share the `.` separator (`Vec[i32].default()` against `user.name.trim()`). The class of the first segment decides how much work the compiler does:
   - **Type-class segment or primitive type name** (`TextColor.from_hex(s)`, `Map[K, V].new()`, `i64.MAX`): always a type path. Types are never local bindings, so no scope lookup is needed.
   - **Const-class segment** (`MAX + 1`, `PI * r * r`): never a module path, because module names are Value class (CN-2). It can only be a constant.
   - **Value-class segment** (`user.name.trim()`, `db.connect()`): the resolver looks it up. A module makes the expression a path (`std.fs.read_to_string`); a local binding or parameter makes it field or method access. Both read the same way to the parser.

   Without the case-class rule, even type paths would need a heuristic.
2. **The ecosystem reads the same everywhere.** Compiler-enforced casing means reviewers do not argue about it and readers moving between libraries do not adjust. Go shows this works at scale; warning-only conventions such as Python's PEP 8 leave lasting friction.

#### Unicode

Identifiers are ASCII. Non-ASCII identifiers are a parse error. Unicode identifiers (UAX #31) are deferred ([deferred.md](deferred.md#unscheduled-language-extensions)).

### Keywords

Kāra keeps its hard keywords few, so that common words stay usable as names.

**Hard keywords** are words that start an item, a statement or an expression, plus the operators spelled as words. They can never be identifiers except through `r#` ([Raw identifiers](#raw-identifiers)).

- Items: `fn`, `struct`, `enum`, `trait`, `impl`, `type`, `const`, `import`, `extern`, `unsafe`, `pub`, `private`.
- Statements: `let`, `mut`, `if`, `else`, `match`, `while`, `for`, `in`, `loop`, `return`, `break`, `continue`, `defer`, `errdefer`.
- Expressions: `ref`, `as`, `and`, `or`, `not`, `true`, `false`, `self`, `Self`.

**Contextual keywords** are keywords only in one position and ordinary identifiers everywhere else:

| Word | A keyword only when it |
|---|---|
| `par` | comes directly before `{`, `for` or `(` ([§13](#13-concurrency)) |
| `union` | starts an item ([§15](#15-unsafe-ffi-and-layout-control)) |
| `distinct` | comes directly before `type` |
| `marker` | comes directly before `trait` |
| `shared`, `sync` | come directly before `struct` or `enum` ([§11](#11-ownership-and-sharing)) |
| `weak`, `frozen` | prefix a type or a receiver ([§11](#11-ownership-and-sharing)) |
| `freeze` | prefixes an expression ([§11](#11-ownership-and-sharing)) |
| `escaping` | prefixes a function type ([§7](#7-functions-and-closures)) |
| `own` | comes directly before a parameter's type, before `self`, or before an argument type in a function type (`Fn(own T) -> R`); it marks an owned parameter ([§7](#parameter-modes)) |
| `host` | comes directly before `fn` at item position ([§15](#15-unsafe-ffi-and-layout-control)) |
| `test` | comes directly before a string literal at module scope ([§14](#14-testing)) |
| `where` | follows a signature or a type head |
| `with` | starts an effect clause ([§12](#12-effects)) |
| `effect`, `resource` | start an effect resource declaration ([§12](#12-effects)) |
| `reads`, `writes`, `sends`, `receives`, `allocates`, `panics`, `blocks` | appear inside an effect clause or an effect attribute such as `#[no_effect(...)]` ([§12](#12-effects)) |

So these are legal:

```kara
struct IoStats { reads: i64, writes: i64, blocks: i64 }
fn union(mut ref self, a: i64, b: i64) { ... }
fn create_server(host: String, port: u16) { ... }
```

`use` and `mod` are not keywords. Modules come from the directory tree and imports use `import` ([§4](#4-modules-and-packages)). Words that belong to deferred features (`lock`, `seq`, `layout`, `group`, `stable`, `alias`, `independent`, `providers`, `requires`, `ensures`, `invariant`, `suspends`) are not keywords in v1; they return as contextual keywords with their tracks.

The logical operators are the words `and`, `or` and `not`. The symbols `&&`, `||` and prefix `!` are rejected with a fix that rewrites them.

#### Reserved for future use

These words are reserved, so that a later feature can use them without breaking code. All but `pure` have no meaning yet. Using one as an identifier is `error[E_RESERVED_KEYWORD]` (`E0003`), and the fix is `r#name`.

| Keyword | Why reserved |
|---|---|
| `async`, `await` | Kāra marks suspension with the `suspends` execution effect (services track) rather than function colouring. Reserving these blocks accidental borrowing from the JS and Rust async tradition. |
| `become` | Guaranteed tail calls. |
| `box` | An owned heap-pointer primitive. Kāra uses owned values and `shared` types. |
| `comptime` | Compile-time evaluation (comptime track, [deferred.md](deferred.md#comptime)). |
| `do` | Block-expression sugar. |
| `dyn` | Trait objects (services track, [deferred.md](deferred.md#dyn)). |
| `final`, `override`, `virtual` | Subclass-style markers. Kāra has no inheritance, but the words are common in code ported from OOP languages. |
| `gen`, `yield` | Generators. |
| `asm`, `global_asm` | Inline and module-level assembly (systems track). |
| `move` | Rust's capture-by-value keyword. Kāra infers capture modes ([core-semantics.md §9.1](core-semantics.md#9-closures)). The parser reports `error[E_MOVE_NOT_USED]` and suggests `.clone()` before the closure. |
| `priv` | Short enough to be confused with `private`. |
| `pure` | Its one use in v1: before `fn` in an `extern` block, it declares that the foreign function has no effects (`pure fn strlen(s: CStr) -> i64;`). Only `extern` functions take it ([§15](#15-unsafe-ffi-and-layout-control)). |
| `try` | `try { ... }` blocks (deferred; see [Results and propagation](#results-and-propagation)). |
| `typeof` | Type-of queries. |

The list is closed for v1. Adding a hard or reserved keyword is a breaking change and needs an edition. Un-reserving a word is not. New features use contextual keywords where they can, because those take no name away.

### Raw identifiers

`r#` directly followed by an identifier-start character makes a keyword an ordinary identifier:

```kara
let r#async = compute_handle();             // `async` is reserved; r#async names a local
fn r#try() -> Result[i32, AnyError] { ... } // r#try is a function
struct Config { r#move: bool }              // r#move is a field
let h = handle.r#await;                     // a field, not the keyword
import std.collections.r#async_map;         // a raw module-path segment
```

**The escape is purely lexical.** The lexer strips `r#`. Symbol tables, mangled names, diagnostics and debug info all see the bare name. `r#foo` on a name that is not a keyword is legal and means `foo`, so code generators can escape every identifier without checking.

**Case class** is decided after the prefix: `r#async` is Value class, `r#Async` Type class.

**Where it can appear:** any binding position (`let`, `fn`, `struct`, `enum`, variants, generic parameters, parameters, fields, aliases, traits, methods), any reference position (`r#name`, `obj.r#name`, `module.r#name`, `r#Name[T]`), and an attribute name (`#[r#name]`). Attribute paths have one segment in v1: `#[karac::proto]` is an error ([§17](#attributes)).

**Where it cannot appear:**
- Inside comments and string literals, which hold text, not tokens.
- In a label. Labels have their own sigil (`'outer`, [§6](#6-expressions-and-statements)).
- On operators and punctuation. `r#` applies only to identifier-shaped tokens.

**Structural markers cannot be escaped**, because they carry positional meaning the parser depends on:

| Name | Why |
|---|---|
| `self`, `Self` | The receiver and the current type. |
| `_` | The wildcard pattern. |
| `super`, `crate` | Kāra does not use them, but if added they would carry path-resolution meaning the escape could not strip. |
| `pub`, `priv`, `private`, `mut`, `ref` | Visibility and parameter-mode markers. Escaping them would only produce a confusing diagnostic. |

Escaping one of these is `error[E_RAW_IDENT_NOT_ALLOWED]` (`E0004`). It is a different code from `E0003` because that error's fix, `r#name`, cannot help here.

**No conflict with raw strings.** The characters after `r` decide: `r#` followed by an identifier-start character is a raw identifier, while `r"`, `r#"`, `r##"` and so on start a raw string ([String literals](#string-literals)).

**Forward compatibility.** `r#name` is how code keeps using a name that a later edition makes a keyword. A `karac fix --edition` rewrite inserts the prefix mechanically. Without the escape, every new keyword would be a hard source break, which the edition guarantee ([§2](#2-specification-layers)) does not allow.

### Symbols

```
// Delimiters
(  )  {  }  [  ]

// Punctuation
:  ,  ;  .  ..  ..=  ->  =>  ?  ??  #  _  '

// Arithmetic
+  -  *  /  %

// Comparison
==  !=  <  <=  >  >=

// Bitwise
&  |  ^  ~  <<  >>

// Assignment
=  +=  -=  *=  /=  %=  &=  |=  ^=  <<=  >>=
```

- `#[` starts an attribute ([§17](#17-tooling-contract) lists them).
- `'` followed by an identifier and no closing `'` is a label (`'outer`, [§6](#6-expressions-and-statements)). With a closing quote it is a character literal.
- There is no `?.` and no `|>`. How `f()?.x` parses is in [Results and propagation](#results-and-propagation).

### Numeric, character and boolean literals

- **Integers** are decimal (`1_000_000`), hexadecimal (`0xFF`), binary (`0b1010`) or octal (`0o17`). `_` separates digits. A suffix fixes the type: `i8`, `i16`, `i32`, `i64`, `i128`, `u8`, `u16`, `u32`, `u64`, `u128` (`255u8`).
- **Floats** use IEEE 754 decimal notation: a decimal point, an exponent, or both (`3.14`, `1.5e-3`, `6.022e23`, `1e10`, `2.5E+6_f32`). A literal with neither a point nor an exponent is an integer, so `1f32` is not a float; write `1.0f32`. The exponent marker is `e` or `E`, optionally followed by `+` or `-`. `_` groups digits in the mantissa and the exponent (`1_000.000_5`, `1e1_000`). A float may carry the suffix `f32` or `f64`.
- Without a suffix, a numeric literal's type comes from inference, and it defaults to `i64` or `f64` ([§8](#8-type-inference-and-generics)). Its value is checked against the range of that type at compile time. How suffixes and expected types combine is in [§5 Numeric literals](#numeric-literals).
- **Characters** are `'a'`, `'\n'`, `'\u{1F600}'`, of type `char`. `b'A'` is a byte literal ([Byte and byte-string literals](#byte-and-byte-string-literals)).
- **Booleans** are `true` and `false`.

### String literals

| Form | Escapes | Notes |
|---|---|---|
| `"..."` | yes | The basic form. |
| `"""..."""` | yes | Multi-line, with dedent (below). |
| `r"..."`, `r#"..."#` | no | Raw: a backslash is an ordinary character (below). |
| `f"..."`, `f"""..."""` | yes | Interpolated ([Interpolated strings](#interpolated-strings)). |
| `b"..."` | byte escapes | `Array[u8, N]` ([Byte and byte-string literals](#byte-and-byte-string-literals)). |
| `c"..."` | byte escapes plus `\u{...}` | NUL-terminated, for FFI ([C-string literals](#c-string-literals)). |

**Escapes** in text literals: `\n`, `\t`, `\r`, `\\`, `\'`, `\"`, `\0`, `\u{...}` (a Unicode scalar value) and `\xHH` (a hex byte). A text literal must be valid UTF-8. There is no `\{` escape; in an f-string a literal brace is `{{` or `}}` ([Interpolated strings](#interpolated-strings)).

**Prefixes.** A prefix is recognized only when it touches the opening quote: `f"x"` is an interpolated string, while `f "x"` is the identifier `f` followed by a string. The prefixes are `f`, `r`, `b` and `c`, and `b` on a character literal. Combinations such as `rf`, `br`, `rb` and `cr` are not literal forms in v1, and the lexer reports them with a focused error rather than lexing an identifier and a string. No other prefix is reserved.

**Type.** A string literal is a `String` by default and a `Str` where a `Str` is expected, the same way an integer literal is `i64` by default and takes an expected type ([§8](#8-type-inference-and-generics)). The rule is the same in every scope.
- `let s = "hi";` makes a `String`. `let s: Str = "hi";` and a `Str` argument make a `Str`.
- A literal typed as `Str` is a view of static data. It allocates nothing and borrows nothing, so it may go anywhere, a `const` and a global included ([core-semantics.md §5.3](core-semantics.md#5-references-and-views-c5)).
- `const NAME: Str = "x";` is the recommended form for a string constant ([Constants](#constants)).

`String` and `Str` are specified in [§5](#strings) and [library/strings.md](library/strings.md).

**Multi-line strings.** `"""..."""` spans lines and processes escapes like `"..."`.
- The line break after the opening `"""` and the line break before the closing `"""` are not part of the value.
- The indentation of the closing `"""` is removed from every line (dedent).
- A non-blank line indented less than the closing `"""` is an error.

```kara
let query = """
    SELECT * FROM users
    WHERE age > 25
    ORDER BY name
    """;
// The value is "SELECT * FROM users\nWHERE age > 25\nORDER BY name", with no final line break.
```

**Raw strings.** `r"..."` processes no escapes: `\n` is two characters. Raw strings suit regular expressions and Windows paths. To contain a `"`, add `#` on both sides: `r#"say "hi""#`. More `#` contain more: `r##"..."##` may hold `"#`. A raw string never dedents and has no triple-quoted form. There is no `rf"..."`. Raw strings are not yet implemented.

```kara
let pattern = r"\d{3}-\d{4}";
let path = r"C:\Users\name\docs";
let quoted = r#"<a href="x">"#;
```

### Interpolated strings

`f"..."` embeds expressions in braces:

```kara
let name = "world";
let count = 3;
let msg = f"hello {name}, {count} items, {count + 1} total";
```

- **A language feature, not a library call.** The lexer recognizes `f"`. Each hole `{expr}` holds any expression. The compiler formats each hole with the value's `Display` implementation (`expr.to_string()`, [§9](#9-traits)) and concatenates the parts in one allocation. There is no user-visible concatenation function.
- **Order.** The holes are evaluated left to right ([core-semantics.md §8.1](core-semantics.md#8-evaluation-order)).
- **Full expressions.** The parser balances quotes, brackets and braces inside a hole, so `f"value: {map["key"]}"`, `f"item: {arr[idx]}"` and `f"pair: {(a, b)}"` work.
- **No nesting.** An f-string inside a hole is a compile error. Bind it first: `let inner = f"inner: {x}"; let outer = f"outer: {inner}";`.
- **Literal braces.** `{{` and `}}` produce `{` and `}`. In a plain `"..."` string a brace is an ordinary character.
- **Multi-line.** `f"""..."""` follows the `"""` rules above.

**Format specifiers.** A hole may end with `:` and a specifier:

```
spec   := [[fill] align] ['+'] ['0'] [width] ['.' precision] [type]
align  := '<' | '>' | '^'
width  := DIGIT+
prec   := DIGIT+
type   := 'x' | 'X' | 'o' | 'b' | 'd'
```

- `align` places the value left (`<`), right (`>`) or centered (`^`) within `width`. `fill` is any single character and needs an explicit `align` after it, so a bare `0` stays the zero-pad flag.
- `+` prints a sign on non-negative numbers too. `0` pads numbers with zeros after the sign.
- `.precision` sets the digits after the decimal point.
- `type` prints an integer in hexadecimal (`x`, `X`), octal (`o`), binary (`b`) or decimal (`d`).
- An unrecognized specifier is a compile error at the hole.

```kara
let pi = 3.14159;
let n = 42;
let name = "world";
let s1 = f"{pi:.2}";      // "3.14"
let s2 = f"{n:>5}";       // "   42"
let s3 = f"{n:05}";       // "00042"
let s4 = f"{n:x}";        // "2a"
let s5 = f"{n:+}";        // "+42"
let s6 = f"{name:*^9}";   // "**world**"
```

`{x:?}` and `{x:#?}` are reserved for the `Debug` form and its pretty-printed variant ([§9](#9-traits)). They are not yet implemented, and the compiler says so rather than reporting a syntax error.

### Byte and byte-string literals

Protocol code, register definitions and binary parsers need literal bytes that are not valid UTF-8, where `String` is the wrong type.

```kara
let magic: u8 = b'U';                              // 0x55
let eth_ether_type: Array[u8, 2] = b"\x08\x00";    // IPv4 ether type
let ascii_banner: Array[u8, 13] = b"hello world\n\0";
```

**Types.**
- `b'A'` has type `u8`.
- `b"..."` has type `Array[u8, N]`, where `N` is the byte count after escapes. The literal is an owned fixed-size array, and the length in the type is what protocol and register code needs at compile time. It passes as `Slice[u8]` wherever an `Array[u8, N]` does.

**Escapes.**
- Permitted: `\n`, `\t`, `\r`, `\0`, `\\`, `\'`, `\"` and `\xHH`. `b'\xFF'` and `b"\xDE\xAD\xBE\xEF"` are valid.
- Forbidden: `\u{...}`. A byte literal is not Unicode. A Unicode escape would either truncate code points above `U+00FF` or fail on some of them. The error says: `Unicode escapes are not permitted in byte literals; use \xFF for byte 0xFF.`

**Not text.** Passing a byte string where `String` or `Str` is expected is a type error. `String.from_utf8(bytes)` converts, returning `Result[String, Utf8Error]`.

### C-string literals

`c"..."` is a NUL-terminated byte sequence for FFI. It passes a constant string to a C function expecting `const char *` without the copy that `String.to_cstring()` makes.

- **Type.** `ref CStr`. `CStr` is a contiguous run of bytes ending in a NUL that the type's surface does not show. The literal points into read-only data: no allocation, no copy, no drop. It is `Copy`.
- **Layout.** A literal of `N` source bytes occupies `N + 1` bytes; the compiler adds the NUL. `c"hello".len()` is `5`.
- **Escapes.** The byte-string escapes, plus `\u{...}`, which is encoded as UTF-8: `c"café"` is the five UTF-8 bytes of `café` followed by a NUL.
- **No interior NUL.** A C string ends at its first NUL, so `\0`, `\x00` or `\u{0}` inside one would silently truncate it on the C side. This is `error[E_INTERIOR_NUL_IN_C_STRING]`, reported at the escape. Use a byte string if a NUL is needed.
- **No `c'...'`.** A C `char` is a byte; write `b'A'`.
- **No implicit conversion** to `String` or `Str`, even for valid UTF-8. `CStr`, the owning `CString` and their conversions are in [library/strings.md](library/strings.md).

```kara
unsafe extern "C" {
    fn puts(s: *const u8) -> i32 with writes(Stdout);
}

fn greet() with writes(Stdout) {
    let msg: ref CStr = c"hello, world";
    puts(msg.as_ptr());            // a pointer into read-only data, no copy
}
```

The pointer type is `*const u8`, not `*const i8`. Where C's `char` is signed, the cast happens at the FFI boundary like any other integer cast ([§15](#15-unsafe-ffi-and-layout-control)).

### Comments

- `// ...` runs to the end of the line.
- `/* ... */` is a block comment. Block comments nest.
- `///` and `//!` are doc comments (below).

### Doc comments

Documentation lives in two line-comment forms. The compiler keeps them on the syntax tree, and `karac doc` renders them to a static HTML site.

| Form | Attaches to |
|---|---|
| `/// text` | The next item or member |
| `//! text` | The enclosing file's module |

Both are line comments; there is no block form. Consecutive lines of the same form make one doc comment: the lines are joined with newlines, with one leading space stripped from each line if present. So CommonMark headings, lists, fenced code and paragraphs (separated by blank `///` lines) work as written. A plain `//` comment or a blank line ends the run.

```kara
/// Computes the user's display name.
///
/// Falls back to `email` when no display name is set.
pub fn display_name(u: User) -> Str { ... }
```

**Attachment.** A `///` run attaches to the item directly after it, with no blank line and no other token in between. Otherwise the run is discarded with a warning.
- **Items:** `fn`, `struct`, `enum`, `trait`, `const`, `type` aliases, `distinct type`, foreign functions. An `impl` block takes no doc comment; its methods document themselves.
- **Members:** struct fields, enum variants, the fields of a struct-like variant (`Variant { x: T, y: U }`), function parameters, and trait methods and their parameters.
- **Modules:** a run of `//!` lines at the top of a file, before any item. `//!` anywhere else is a parse error.

A `///` before `self` in a method signature is ignored: `self` is not a documented parameter.

**Rendering (`karac doc`).** The comments are CommonMark with tables, footnotes, task lists, strikethrough and fenced code.
- **Cross-references.** A reference-style link whose label is the name of a documented item (`[Vec]`, `[push]`) links to that item's page. Names resolve across the package by bare name; an ambiguous name resolves to the first match in module order, and an unresolved label renders as plain text.
- **Item pages.** Each documented item gets a page with its signature (with effects, for `pub fn`), its doc text, a parameters section when a parameter is documented, and field or variant sections when one is documented.
- **Module docs.** The package root's `//!` text renders above the index. Other modules' text renders before their items.

**Limits in v1.** No path-qualified cross-references (`[std.collections.Vec]`). Inferred effects of private functions are not shown. Code blocks in doc comments are not run as tests.

---

## 4. Modules and packages

### Modules

**The directory tree is the module tree.** Each `.kara` file under `src/` is a module, named by its path from `src/`: `src/db/connection.kara` is the module `db.connection`. There is no module declaration.

- **Entry points.** `main.kara` for an executable, `lib.kara` for a library. A package cannot have both. Items in `main.kara` or `lib.kara` belong to the package root: `fn start()` in `main.kara` is `start`, not `main.start`.
- **One file per module path.** `src/db.kara` defines `db`. `src/db/mod.kara` is not recognized. To put items directly in `db`, write them in `src/db.kara` or re-export them there.
- **Fully qualified paths.** Every type, function and effect resource is named by its module path. `db.UserDB` in one package and `otherlib.db.UserDB` from a dependency are distinct resources, and the effect checker treats them as non-conflicting.

**The package name in paths.**
- **Inside a package**, paths omit the package name. In package `myproject`, `src/db/connection.kara` is `db.connection`, and its items are imported as `db.connection.Connection`, never `myproject.db.connection.Connection`.
- **Across packages**, a path starts with the dependency's name. If `webserver` depends on `http`, it writes `import http.client.Connection` (from `http`'s `client` module) or `import http.Connection` (an item of `http`'s root). The resolver recognizes a dependency by matching the first segment against the dependency names in `kara.toml`.
- **A local module hides a dependency of the same name.** If a package has `src/db.kara` and a dependency named `db`, `db` means the local module. To reach the dependency, give it an alias in `kara.toml`:

  ```toml
  [dependencies]
  db = { version = "1.0", alias = "db_ext" }
  ```

  Then `import db_ext.Connection` names the dependency.
- **Definitions know their package.** A dependency's modules are rooted at the dependency's name, so two dependencies that both have a `utils` module stay distinct, and coherence ([§9](#9-traits)) can ask which package a definition belongs to. Items of the package being compiled carry no package segment.

**No circular module dependencies.** The compiler builds a module graph from each file's `import` statements (including `pub import` edges) and rejects any cycle before name resolution: `E0223 CircularModuleDependency`, listing the cycle and suggesting that shared items move to a lower module.

**Colocated tests.** A test file has the `_test.kara` suffix and sits beside the code it tests. `src/db/connection_test.kara` tests `db.connection` and can see its `private` items. `karac build` skips `_test.kara` files; `karac test` includes them and adds the test prelude ([§14](#14-testing)). There is no separate `tests/` root.

### Imports

Every reference to another module's item is brought into scope by an `import` at the top of the file. Paths are absolute: from the current package's root, or from a dependency's name. There is no `self.`, `super.` or relative form.

```kara
import db.connection.Connection;                       // one item
import db.connection.Connection as Conn;               // rename
import db.connection.{Connection, Pool};               // several items
import db.connection.{Connection as Conn, Pool as P};  // several, renamed
import db.connection;                                  // the module itself, as `connection`
import db.connection.*;                                // every accessible item of `db.connection`
import db.{connection.{Connection, Pool}, auth.Token}; // nested groups
```

**Binding rule.** The last segment of each path is bound in the current scope: `import a.b.c;` binds `c`, `import a.b.{c, d};` binds `c` and `d`, and `import a.b.c as X;` binds `X`. Modules and items follow the same rule: `import db.connection;` binds the module `connection`, so its items are `connection.Connection`.

**Groups and wildcards expand first.** `import a.{b.{c, d}, e};` is `import a.b.c; import a.b.d; import a.e;`. A wildcard expands to the single-item imports it stands for, so the forms compose (`import a.{b.*, c};`) and later phases see only single items. `*` must be the last segment; `import path.* as X;` is not a form.

**Wildcards** bring in exactly the items a single-item import of that module could bind ([Visibility](#visibility)). Submodules are not items; import them by path.
- An explicit import beats a wildcard. With `import http.Response;` and `import net.*;`, `Response` is `http.Response`.
- Two wildcards that both supply a name are not an error at the import. The name is ambiguous, and using it is `E0124 AmbiguousWildcardImport`; an explicit import resolves it. Other names from both wildcards work normally.
- Prelude names have the lowest priority: any import shadows them.

**Re-exports.** `pub import` re-exports an item from the current module. Users see it at the shorter path; the identity is unchanged, so `mylib.Connection` and `mylib.db.connection.Connection` are the same type.

```kara
// mylib/lib.kara
pub import db.connection.Connection;

// user code
import mylib.Connection;   // not mylib.db.connection.Connection
```

The original must be visible to the re-exporting module, so `pub import` of a `private` item from outside its directory is a compile error.

**Resolution.** For `import a.b.c.Item;` in module `R`:
1. Start at the package root (or at the dependency named by the first segment).
2. Descend one child module per segment except the last. A miss is `E0112 UnknownModule`, with "did you mean" suggestions.
3. Look up the last segment among the target module's items. A miss is `E0113 UnknownItemInModule`, with suggestions and, for a module with at most 10 exports, the list of exports. An item found but not visible from `R` is `E0111 PrivateItemAccess`.
4. Bind the name in `R`.

**The prelude.** Every file implicitly starts with `import std.prelude.*;`. It goes through the normal import machinery at the lowest precedence, so any import or local name shadows a prelude name; a lint flags the likely-unintended cases.
- **Primitive types** are built in and need no import.
- **Standard types, traits and functions** in the prelude are listed in [library/README.md](library/README.md). The prelude holds the core types and traits; everything else is under `std.*`.
- **Compiler builtins** (`todo`, `unreachable`, `panic`, `dbg`, `assert`, `assert_eq`, `assert_ne`) are prelude functions, usable in every file, production code included. They are declared in standard-library source under `#[compiler_builtin]`, so documentation and editors see them. The compiler implements them, because they need the `Never` type, source locations and release-mode elision.
- `_test.kara` files also get the test prelude ([§14](#14-testing)).

### Visibility

| Keyword | Visible to |
|---|---|
| `pub` | Users of the package, and every file in it |
| *(none)* | Every file in the package, not its users |
| `private` | Files in the same directory only |

- **`pub`** marks the public API. Public functions declare their effects ([§12](#12-effects)).
- **The default** is package-internal: most code is internal helpers that other files in the package call. Private functions' effects are inferred.
- **`private`** limits an item to its directory, for helpers shared by related files (`db/connection.kara` and `db/schema.kara` sharing `db/helpers.kara`). It does not extend to parent or child directories.

**Enforcement.** For a lookup of item `X`, defined in module `D`, from module `R`:

| `X` is | Allowed when |
|---|---|
| `pub` | always |
| default | `R` and `D` are in the same package |
| `private` | `R` and `D` are in the same directory. Entry files (`main.kara`, `lib.kara`) count as in `src/`, and a `_test.kara` file shares the directory of the module it tests. |

A non-`pub` type in a `pub` signature is `E0221 PrivateTypeInPublicSignature`. Direct access to an invisible item is `E0111 PrivateItemAccess`.

**Why the default is package-internal.** In languages where the default is private to the module (Java, C#), modules are files or classes. In Kāra, modules are directories, and a directory-private default would force a keyword onto nearly every call between directories. Annotating only the boundaries (`pub` for the API, `private` for directory helpers) covers the common case. Directories otherwise only organize names: a default-visibility function in `db/connection.kara` is visible to `ui/render.kara`.

**Struct fields** use the same three levels ([§5 Structs](#structs)).

### Packages and manifests

`kara.toml` is the package manifest: the single source of package metadata, dependencies and build settings.

**Layout.**

```
myproject/
  kara.toml              # manifest
  src/
    main.kara            # executable entry point (lib.kara for a library)
    greet.kara
    greet_test.kara      # colocated tests
    db/
      connection.kara
      connection_test.kara
      pool.kara
  examples/
    basic.kara           # runnable examples
```

The compiler finds every `.kara` file under `src/`. `examples/` is a separate root: each file is a program run with `karac run --example NAME`.

**Finding the package.** A package-mode command (`karac build`) walks up from the working directory to the first `kara.toml`; none is `E0227 NotInsideKaraProject`. `karac run file.kara` needs no manifest. For a single file it walks up from the file's own directory, not the working directory, so a script inside a package gets that package's dependencies wherever it is run from, and a script outside any package (`/tmp/foo.kara`) gets only the standard library. `--manifest path` forces a manifest; `--no-manifest` forces none.

**The manifest.**

```toml
[package]
name = "myproject"
version = "0.3.1"                  # semver
authors = ["Alice <alice@example.com>"]
edition = "2026"

[dependencies]
http = "1.2"                       # >=1.2.0, <2.0.0
json = { version = "0.8", git = "https://github.com/example/json-kara" }
logging = { path = "../logging" }  # local path dependency

[dev-dependencies]
proptest = "0.4"                   # only in tests and examples

[build]
target = "x86_64-linux"            # default manifest-overlay triple; --target overrides
```

- `[package].name` is required. It is the package's identity and the name dependents use as a path's first segment.
- `[package].edition` is optional and validated.
- `version` and `authors` are recognized. An unknown key in `[package]` is a warning; invalid TOML is an error.
- `[build].target` selects the default manifest overlay ([Conditional compilation](#conditional-compilation)); `[build].targets` lists the targets `karac check` verifies ([§16](#16-targets-and-compilation)).
- Dependency resolution is not yet implemented. Today the compiler reads `[package]` and `[build]` and ignores the other sections without error.

**Editions.**
- `"2026"` is the only edition. An unknown value is an error: `unknown edition "2027"; this compiler supports editions up to "2026"`.
- Without the field, the edition is `"2026"`: the default is always the earliest edition, never a silent upgrade.
- Editions are per package. A newer compiler compiles every earlier edition, and a library's edition does not affect its users.
- An edition gates breaking language changes. One example exists: an edition boundary may broaden private-function effect inference ([§2](#2-specification-layers)).
- **Migration pipeline.** An edition-gated change goes through three stages:
  1. **Warn.** A warn-by-default lint reports the coming change in the current edition, suppressible with `#[allow(<lint>)]`. The lint name is permanent ([§17](#17-tooling-contract)).
  2. **Deny.** Later in the edition, the lint becomes deny-by-default; code needs an explicit `#[allow(<lint>)]` to compile.
  3. **Error.** At the next edition boundary the behavior becomes a hard error under the new edition.

  The warning stage gives authors lead time, the deny stage forces awareness without forcing immediate migration, and the edition boundary is the final stop. `karac explain --edition <NEXT>` shows a package's pending migrations, `karac fix --edition <NEXT>` applies the mechanical ones, and lint attributes can escalate or allow a lint ([§17 Lint levels](#lint-levels)). A `[lints]` table in `kara.toml` comes with the tooling track ([deferred.md](deferred.md#tooling)).

**Versions and resolution.**
- **Semver.** `"1.2"` means `>=1.2.0, <2.0.0`; `"=1.2.3"` is exact; `">=1.0, <1.5"` is a range. A conflict is a compile error showing the constraint chain.
- **PubGrub resolver.** The newest compatible version is chosen unless `kara.lock` pins one, and a conflict is explained as a chain ("`A` requires `C >=1.0`, `B` requires `C >=2.0`") rather than "no solution". Rejected alternatives: npm-style duplicate copies (large builds, the same vulnerable package N times), Go-style minimum versions (ages dependencies by default), and leaving conflict resolution to the user.
- **Workspaces.** A root `kara.toml` may declare members that share one lockfile and one output directory. Shared versions go in `[workspace.dependencies]`, and a member opts in per dependency:

  ```toml
  # workspace root kara.toml
  [workspace]
  members = ["core", "cli", "web"]

  [workspace.dependencies]
  http = "1.2"
  json = "0.8"
  ```

  ```toml
  # core/kara.toml
  [dependencies]
  http = { workspace = true }    # version from [workspace.dependencies]
  json = { workspace = true }
  ```

  `workspace = true` for a name missing from `[workspace.dependencies]` is a compile error. A member may also declare its own dependencies with its own constraints.

**Lockfile.** `kara.lock` records every package in the resolved graph: name, exact version, source (proxy mirror or git URL), BLAKE3 content hash, and the dependency tree. One lockfile serves every target, so cross-compiling never skews versions. Executables commit it; libraries do not. `karac update` moves every dependency to the newest compatible version; `karac update <pkg>` moves only `<pkg>` and what it newly needs.

**Package identity and the proxy.** A package is identified by its git URL (`git+https://github.com/example/json-kara`). There is no central namespace and no publish step. The package proxy (`proxy.kara-lang.org` or a self-hosted equivalent) mirrors every URL and ref resolved through it, immutably, addressed by content hash. Resolution goes through the proxy by default; `--no-proxy` fetches directly. The proxy is what makes decentralized identity reproducible: a force-pushed branch, a deleted repository or a network failure cannot change a locked build. A central registry is not planned; the manifest accepts an optional `registry = "https://..."` so one can be added later without a break.

**Reproducible builds.** Given a committed `kara.lock`, a pinned toolchain and the same target triple, builds are bit-identical. The compiler must be deterministic: no timestamps, sorted symbol output, no host paths in binaries, no nondeterministic linking. Any nondeterminism is a compiler bug.

**Toolchain pinning and library MSRV.**
- A library states the oldest compiler it supports with `[package].kara-version = ">=0.42"`; the resolver enforces it.
- A project pins the exact compiler in an optional `karac-toolchain.toml` at the project or workspace root (`version = "0.42"`, optional `targets = [...]`). A toolchain manager installs that version before running `karac`.
- They are separate because the MSRV travels with the package and is read by the resolver, while the pin belongs to one checkout and is read by toolchain managers. This mirrors Rust's `rust-version` and `rust-toolchain.toml`.

**Per-target dependencies and profiles.**

```toml
[target.wasm32-wasip1.dependencies]
wasi-helpers = "0.2"

[target.wasm32-wasip1.profile]
opt-level = "z"
```

Each `karac build --target X` produces one artifact. Building a matrix of targets is a CI concern.

**Vendoring.** `karac vendor` copies the resolved dependencies into `vendor/`; `karac build --offline` then reads only `vendor/` and makes no network access.

**Build cache.** Each package's own output goes in `dist/` (in the default `.gitignore`; `karac clean` removes it). Compiled dependencies go in the machine-wide `~/.kara/cache/`, keyed by compiler version, package version, edition, profile and target triple. A compiler upgrade invalidates the cache; time never does. `karac clean --global` evicts it.

**`karac init`.** `karac init [<name>] [--bin | --lib] [--force]` scaffolds a package.
- Bare `karac init` uses the working directory; `karac init <name>` creates `./<name>/`, and refuses if it exists and is not empty.
- The directory name is the package name and the root module name. It must match `[a-z][a-z0-9_]*` and not be a keyword; otherwise nothing is written (no automatic rewriting of `my-project`).
- `--bin` (the default) and `--lib` are exclusive. A package is a binary or a library; a library plus a CLI is a workspace.
- It refuses if `kara.toml`, `src/main.kara` or `src/lib.kara` exists, unless `--force`. It never overwrites `README.md`, the test file or `.gitignore`, even with `--force`. It does not run `git init`.
- It writes five files: `kara.toml` (`name`, `version = "0.1.0"`, `authors = []`, `edition = "2026"`, an empty `[dependencies]`), `src/main.kara` with `fn main() { println("Hello, world!"); }` or `src/lib.kara` with a documented `pub fn add(a: i64, b: i64) -> i64 { a + b }`, a matching `_test.kara` with one test, a `README.md` holding the title, and a `.gitignore` holding `/dist/`.

**Command phasing.**

| Phase | Commands | Gate |
|---|---|---|
| First | `build`, `run`, `check`, `fmt`, `query`, `test`, `init`, `doc` | Current scope |
| With dependency resolution | `update`, `clean`, `install`, `vendor` | The resolver and lockfile |
| Later | `bench`, `publish`, `audit` | A bench harness, a publish protocol, a vulnerability database |

`karac fix` applies the machine-applicable fixes of `--output=jsonl` and is not tied to a phase.

### Constants

`const NAME: T = e;` declares a constant. It is the only value declared at module scope: Kāra has no module-level `let` or `let mut`.

**The initializer is a compile-time constant.** There is no module initialization: no code runs before `main`, so there is no initialization order, no startup effect without an owner, and no lazy first access. Constants exist in the binary as data, and at program start the runtime calls `main`.

**Allowed initializers:**
- Literals: `42`, `3.14`, `true`, `'x'`, and string literals at type `Str` (`"localhost"`), which view static data and allocate nothing ([String literals](#string-literals)).
- Arithmetic, comparison and boolean operations on constants: `60 * 1000`, `MAX_RETRIES + 1`.
- Enum variants with constant arguments: `Direction.North`, `Some(42)`.
- Struct and tuple literals of constants: `Point { x: 0, y: 0 }`, `(1, 2, 3)`.
- List literals and repeat forms at an `Array` type: `[1, 2, 3]`, `[0; 512]`.
- Other constants, from the same module or imported ones.

**Forbidden initializers:**
- Any expression with an effect, declared or inferred.
- Function calls. Calls to `const fn` come with the comptime track.
- Closures, which may capture state and carry effects.
- Anything that allocates. `String` is heap-allocated, so it cannot be a constant's type; use `Str`.

```kara
// allowed
pub const MAX_RETRIES: i32 = 5;
pub const TIMEOUT_MS: i64 = 60 * 1000;
pub const APP_NAME: Str = "karac";
const ORIGIN: Point = Point { x: 0, y: 0 };

// rejected: effectful initializer
const CONFIG: Config = load_config("app.toml");
//                     ^^^^^^^^^^^^^^^^^^^^^^^ error: effectful call in a constant

// rejected: String needs the heap
pub const HOST: String = "localhost";
//              ^^^^^^ error: String is heap-allocated; use Str for constant text
```

The diagnostic for an effectful initializer points at the effectful expression, names its effect, and suggests the `main`-and-context pattern below. Compile-time string concatenation (`"http://" + "localhost"`) also waits for `const fn`; build such strings at run time.

**Each use is a fresh value.** A use of a constant is a new value built from its initializer. For a `Copy` type this is a copy. For any other type (`const ORIGIN: Point` above, when `Point` is not `Copy`), every use builds its own value, which the user then owns and may move.

**Visibility.** Constants take the three levels of [Visibility](#visibility): `pub const PI: f64 = 3.14159;`, `const INTERNAL: i64 = 42;`, `private const LIMIT: i64 = 0;`. A `pub` constant is a semver commitment on its type and its value: changing either, or removing it, breaks users, as changing a `pub fn` signature does.

**Why no run-time initialization.** Eager initialization (module code at startup), lazy initialization (on first access) and a separate `static` form all run code at a point that has no named function, no declared effects and no caller to receive its failures. Kāra's effect discipline requires every effect to have a source. A module-scope `load_config("app.toml")` would produce `reads(FileSystem)` with no function to charge it to, no path for its error, and no answer to "which runs first" when two modules' initializers depend on each other. Constant initializers remove those questions by removing the run-time step.

**No mutable globals.** There is no module-level `let mut` and no `static mut`. Program-wide mutable state is a value created in `main` and passed down (below). Module-level cells (`LazyLock`, `OnceLock`, `OnceCell`) and `#[thread_local]` are deferred to the systems track ([deferred.md](deferred.md#systems)).

**Run-time values: the `main`-and-context pattern.** Values that need run-time initialization (configuration, database pools, compiled regular expressions, lookup tables) are built in `main`, or a function it calls, and passed down in a context struct with methods:

```kara
pub struct AppContext {
    pub config: Config,
    pub db_pool: DbPool,
    pub logger: Logger,
}

impl AppContext {
    pub fn new(config_path: String) -> Result[AppContext, AnyError]
        with reads(FileSystem) reads(Env) sends(Network) receives(Network) blocks
    {
        let config = load_config(config_path)?;
        let db_pool = DbPool.connect(config.database_url.clone(), config.max_connections)?;
        let logger = Logger.new(config.timeout_ms);
        Ok(AppContext { config, db_pool, logger })
    }

    pub fn handle_request(self, req: Request) -> Result[Response, AnyError]
        with reads(UserDB) writes(Cache)
    {
        let user = self.authorize(req.token)?;
        self.fetch_profile(user.id).map(Response.ok)
    }
}

fn main() -> Result[(), AnyError]
    with reads(FileSystem) reads(Env) reads(UserDB) writes(Cache) sends(Network) receives(Network) blocks
{
    let ctx = AppContext.new("app.toml")?;
    run_server(ctx, 8080)
}
```

1. **Every effect has a source.** `reads(FileSystem)` starts in `load_config`, passes through `AppContext.new`, and ends at `main`'s signature. `main`'s effects list everything the program can do.
2. **Failure is a value.** A failed initialization returns `Err` through `?`; `main` returns it and the runtime prints it and exits 1. Nothing can fail before `main` starts.
3. **No pervasive parameter.** Functions that use the context are methods that borrow `self`. Only the few functions at the top of the call graph (`main`, `run_server`) pass `ctx` explicitly. Pure leaf functions (`format_date`, `validate_email`) take neither.
4. **Testing is direct.** A test builds an `AppContext` and calls its methods; there is no global state to patch.

**Request-scoped state** (the logged-in user, a request id, a trace span) is a second, per-request context passed beside `self`:

```kara
fn handle_request(self, req: own Request) -> Result[Response, AnyError] {
    let rctx = RequestContext.from(req);
    self.process(rctx)
}
```

`AppContext` lives for the program and `RequestContext` for one request. Neither is a global.

### Conditional compilation

Most code needs none. A function compiles for a target when every resource its effects reach is provided by that target, so code that uses only portable effects builds everywhere with no annotation (effect-driven target gating, [§16](#effect-driven-target-gating)). Two mechanisms cover the rest: the `#[cfg]` attribute, and a platform suffix on a file name.

#### `#[cfg(...)]`

`#[cfg]` compiles the declaration it marks only when its condition holds.

```kara
#[cfg(target: "native")]                        // only when compiling for native
fn main() { ... }

#[cfg(not(target: "native"))]                   // for every target except native
fn platform_name() -> String { "wasm" }

#[cfg(target_os: "linux")]                      // only when the OS platform is Linux
fn create_poller() -> Poller { epoll_create() }

#[cfg(any(target_os: "linux", target_os: "macos"))]
fn unix_socket_path() -> String { "/tmp/app.sock" }
```

**Where it may appear.** On items, on impl members, on struct fields and on enum variants. Not on statements.

**Keys.**
- `target`: the compilation target, one of the target names of [§16](#the-v1-target-set) (`"native"`, `"wasm_wasi"`).
- `target_os`: the OS platform: `"linux"`, `"macos"`, `"windows"` or `"wasm"`, the same values as the file suffixes below.

An unknown key or value is an error.

**Grammar.** The condition is one predicate. A predicate is `key = "value"`, or a combinator around predicates:
- `all(p, ...)` holds when every `p` holds;
- `any(p, ...)` holds when at least one `p` holds;
- `not(p)` holds when `p` does not.

**Rules.**
- A declaration without `#[cfg]` exists on every target; effect-driven gating alone decides whether it compiles.
- A declaration whose condition does not hold does not exist on that target.
- Two `#[cfg]` attributes on one declaration are an error; combine them with `all(...)` or `any(...)`.
- Naming an item that does not exist on the current target is an error at the use, naming the item's condition: `cannot reference item 'foo': not available on target 'native'`.
- There is no run-time `if target == ...`. `#[cfg]` is the one mechanism.
- Under `karac check` with several targets, each target's pass sees exactly the declarations that target's build sees ([§16](#checking-several-targets)).

`#[cfg]` is not yet implemented.

#### Platform files

A file whose name ends in an OS suffix (`_linux`, `_macos`, `_windows`, `_wasm`) compiles only for that OS. It is the file-level form of `#[cfg(target_os: ...)]`.

```
net/
  poller_linux.kara    // compiled on Linux
  poller_macos.kara    // compiled on macOS
  poller_windows.kara  // compiled on Windows
```

**Recognizing a platform file.** The walker splits the file stem at its last underscore. If the tail is exactly one of the four suffixes, the file is a platform file. Otherwise it is an ordinary module whose name is the whole stem: `foo_linux_x86_64.kara` is the module `foo_linux_x86_64` and always compiles. This leaves the rest of the underscore namespace free for later extensions.

**A platform file replaces a shared file.** When `poller_linux.kara` and `poller.kara` both exist, they are the same module `poller`, and exactly one of them compiles for any target: the platform file on Linux, the shared file elsewhere. They do not merge. A `pub fn` that exists only in `poller.kara` does not exist on Linux, and importing it there is `E0113 UnknownItemInModule`, listing the platform file's items. Common helpers therefore go in a third module that both import. Two files that both survive for one target under one module path (two shared files, or two with the same suffix) are a duplicate module, rejected before any other phase. A symbol-level conflict between the files of one module cannot arise, so it has no diagnostic: `E0226 ConflictingPlatformModule`, reserved for it in earlier drafts, is retired and is not reused.

**Missing platforms.** If platform files cover some operating systems and no shared file exists, the module does not exist on the others, and importing it there is `E0112 UnknownModule` with a note listing the platforms that define it. `karac check --platform=all` checks the package once per platform and fails if any fails, so one machine can verify every OS. There is no compiler check that every OS is covered; that is a CI concern.

**Three meanings of "target".**
- The **compilation target** of [§16](#16-targets-and-compilation), which `#[cfg(target: ...)]` tests. `--target=<name>` chooses it; the default is `native`.
- The **manifest overlay triple**, which selects the `[target.<triple>]` blocks to merge. `--target=<triple>` (any value that is not a target name) chooses it, else `[build].target`, else the host triple.
- The **OS platform**, which `#[cfg(target_os: ...)]` and the file suffixes test. `karac check --platform=<name>` chooses it. Otherwise a wasm compilation target selects `wasm`, and every other target selects the host OS. `[build].target` never affects it.

`--platform` is accepted only by `karac check`. Analysing another OS's half from any machine is what keeps a platform split maintainable; emitting it is not possible, because code generation targets the host triple. `karac build` and `karac run` refuse `--platform` and name `check`. Per-OS artifacts come from a CI matrix. Test binaries are built for the host OS.

#### No feature flags

Kāra has no Cargo-style `[features]`, in `kara.toml` or as `#[cfg(feature: ...)]`. Feature unification across a dependency graph is Cargo's largest practical pain ("compiles in `cargo test` but not `cargo build`"), and Kāra avoids it rather than copying it. The usual uses of feature flags are covered one by one:
- **Test-only dependencies:** `[dev-dependencies]`.
- **Target-dependent source:** `#[cfg]` and platform files.
- **Build configuration:** profiles ([§16](#16-targets-and-compilation)) are the one configuration axis.
- **Per-target dependencies:** `[target.<triple>.dependencies]`.
- **Optional library surface:** publish separate packages (`foo`, `foo-blocking`). This is an established pattern and avoids the unification bugs, at the cost of more maintenance for the package author.

A constrained feature axis (per-package flags without transitive unification) is held for a later RFC, to be opened if separate packages prove widely painful.

---

## 5. Types

Kāra has no classes and no inheritance. Data is structs and enums, built from primitives, tuples and arrays; behaviour is `impl` blocks and [traits](#9-traits). A method call and its qualified form are the same call: `user.validate()` is `User.validate(user)`. Generic types are written with square brackets (`Vec[i32]`, `Map[String, i64]`) and are monomorphized ([§8](#8-type-inference-and-generics)).

How values of these types are copied, moved, borrowed and destroyed is set by [core-semantics.md §1–§3](core-semantics.md#1-values) and [§7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0). This section says what each type is, how its values are written, and how they are matched.

### Primitive types

| Type | What it is |
|---|---|
| `i8` `i16` `i32` `i64` `i128` | Signed two's-complement integers. `i64` is the default integer type and the index type. |
| `u8` `u16` `u32` `u64` `u128` | Unsigned integers. |
| `isize` `usize` | Pointer-sized integers for FFI only (C's `ptrdiff_t` and `size_t`). Kāra code uses `i64` for every size, length, count and index, I/O included. |
| `f32` `f64` | IEEE 754 binary floats. `f64` is the default float type. |
| `bool` | `true` or `false`. |
| `char` | One Unicode scalar value: `0..=0xD7FF` or `0xE000..=0x10FFFF`. |
| `()` | The unit type. Its one value is also written `()`. |
| `Never` | The bottom type, with no values. It is the type of `return`, `break`, `continue` and diverging calls (`panic()`, `unreachable()`, `todo()`), and it coerces to any type, so it is valid in any expression position. |

Every primitive is `Copy` ([core-semantics.md §1.1](core-semantics.md#1-values)). Every scalar primitive has `to_string(self) -> String`, which renders it the way f-string interpolation does, and `clone(self) -> Self`, which copies it. The numeric primitives have a closed method surface: calling a method a numeric type does not have is an error, never a silently accepted no-op.

`String` and `Str` are under [Strings](#strings); `F32` and `F64` are under [Floats](#floats).

### Numeric semantics

#### Numeric literals

How numeric literals are written (bases, `_`, exponents, suffixes) is in [§3](#numeric-character-and-boolean-literals). This section says what type they get.

**Default types.** Integer literals (`42`) default to `i64`. Float literals (`3.14`) default to `f64`. An annotation overrides the default: `let x: i32 = 42;`. Literal types are settled by inference over the whole function body ([§8](#8-type-inference-and-generics)).

**Literal suffixes.** A type suffix forces the type of a literal where inference cannot reach it from a binding: `1i32`, `1u8`, `1.0f32`. Use an annotation when there is a binding, and a suffix when the literal is inline, for example an argument: `Vec.filled(n, 0i32)`. A suffix that matches the default type (`42i64`, `3.14f64`) is valid, but the compiler warns; `#[allow(redundant_suffix)]` silences it.

**A suffix cannot contradict a destination the compiler already knows.** Where the destination type is fixed (a binding annotation, a parameter, a field, a sequence element, a return type, or a generic payload the expected type seeds), a float suffix that names a wider width than the destination is an error, not a silent override. The reason is the one behind the widening table below: narrowing needs an explicit `as`. `let d: f32 = 0.1f64` is rejected. Drop the suffix and let `f32` type the literal, or write `0.1f64 as f32` if the rounding is intended. Widening stays implicit, so `let w: f64 = 0.1f32` is accepted and holds the `f32` value widened: a suffix narrower than its destination chooses a coarser constant, which loses nothing. Integer suffixes are governed by the range check instead: `let d: u8 = 300i64` is out of range, and `let d: i32 = 5i64` fits, so the annotation types the binding.

**A literal in a binary operator takes the other operand's type**, even when that differs from the literal's default. This holds for the arithmetic operators (`+`, `-`, `*`, `/`, `%`) and the comparisons (`<`, `<=`, `>`, `>=`, `==`, `!=`). Elsewhere (arguments, patterns, field initializers) the literal takes the expected type, which is already precise.

```kara
let i: u8 = 0;
let n: i32 = 10;
let x: f32 = 1.5;

let a = i + 1;       // u8: the literal takes the type of `i`
let b = n * 2;       // i32
let c = x * 2;       // f32
let d = n < 100;     // the comparison is done in i32
```

#### Integer overflow

**Integer arithmetic traps on overflow.** `+`, `-`, `*`, `/`, `%`, unary `-` and `abs()` panic when the exact result does not fit the type ([core-semantics.md §10.1](core-semantics.md#10-panics-and-errors-c8)). Arithmetic that can overflow contributes `panics` to the function's effects ([§12](#12-effects)). The behaviour is fully defined and the same in every build; no flag turns the check off.

```kara
let x: i64 = i64.MAX;
let y = x + 1;                  // panics: integer overflow
let z = x.wrapping_add(1);      // i64.MIN: the wrap is named at the call site
```

**Why trap by default.** The check costs a little on arithmetic-heavy code, and two reasons make it worth paying. First, silent two's-complement wrapping is a live class of logic bugs and security holes: an overflowed `count * price` or `w * h * bytes_per_px` yields a plausible wrong number, or an undersized allocation that feeds a later heap overflow. Second, machine-written code is the code most likely to carry a latent overflow, because a model rarely reasons about the bounds of `iN` on an arithmetic line. Under a wrapping default that mistake is silent. Under a trap it fails loudly on the first run that reaches it, and becomes a diagnostic that a program can act on. So the escape hatch is narrow and local: the named method families ([below](#checked-wrapping-saturating-and-overflowing-arithmetic)), at the call site. A project-wide switch would remove the guarantee invisibly.

Attribute regions that change the default for a function or block (`#[wrapping]`, `#[checked]`) are deferred ([deferred.md](deferred.md)).

#### Division, remainder and shifts

- **Division `/` truncates toward zero.** `-7 / 2 == -3`, `7 / -2 == -3`, `-7 / -2 == 3`. This matches C, Java, Go and Swift and the instruction every integer CPU issues. `/` is a machine-level primitive; for Euclidean division call `x.div_euclid(n)`.
- **Remainder `%` takes the sign of the dividend.** `-7 % 2 == -1`, `7 % -2 == 1`, `-7 % -2 == -1`. Once `/` truncates, the identity `(a / b) * b + (a % b) == a` forces this. The always-non-negative remainder is `x.rem_euclid(n)`; the explicit name settles "which kind of modulo is this?" at the call site.
- **`iN.MIN / -1` traps as overflow**, in the same family as `iN.MAX + 1`: the exact result does not fit `iN`. `x.wrapping_div(-1)` wraps to `iN.MIN`. **Division by zero** is a separate trap with its own `"division by zero"` message, so the diagnostic stays specific.
- **Shift operands.** In `x << n` and `x >> n`, `n` may have any integer type, and the result has the type of `x`. The operands are not widened to a common type ([Mixed-width operands](#mixed-width-operands)).
- **Right shift `>>` on a signed integer is arithmetic** (it extends the sign): `(-8i32) >> 1 == -4`. A logical shift of a signed value needs explicit casts: `((x as u32) >> 1) as i32`. Left shift `<<` has no signed variant.
- **A shift amount that is negative, or at least the bit width, traps.** `(x: i32) << 32` and `x << -1` panic with `"shift amount out of range"` ([core-semantics.md §10.1](core-semantics.md#10-panics-and-errors-c8)); `(x: i32) << 31` is legal whether or not it changes the sign bit. The same holds for `>>`. `wrapping_shl` and `wrapping_shr` mask the shift amount modulo the bit width and never panic, so the masking that C performs silently must be named.
- **Signedness follows the operand type.** `/`, `%`, `>>` and the ordered comparisons (`<`, `<=`, `>`, `>=`) use the source-level type of their operands. Unsigned types get unsigned division, unsigned remainder, logical right shift and unsigned comparison; signed types get the forms above. So `0xFFFF_FFFFu32 < 1u32` is `false`; reading the bits as signed would invert the answer. For `/`, `%` and the comparisons, when the two operands have different types, [Mixed-width operands](#mixed-width-operands) decides the common type first; a shift uses the type of its left operand.

#### Checked, wrapping, saturating and overflowing arithmetic

Every integer primitive (`i8` to `i128`, `u8` to `u128`, `isize`, `usize`) has four method families, `checked_`, `wrapping_`, `saturating_` and `overflowing_`, which cover every arithmetic operation that can overflow or shift out of range. They are listed in [library/core-types.md](library/core-types.md#numbers).

**`abs()`** is the magnitude of a signed integer or a float, returning `Self`. `iN.MIN.abs()` has no representable result and traps as integer overflow, like unary `-iN.MIN`. Float `abs` follows IEEE 754. Unsigned integers have no `abs`: calling it is a `no method 'abs' on type 'uN'` error.

#### Mixed-width operands

**Lossless widening is implicit; anything that can lose information needs `as`.**

| Conversion | Implicit? | Reason |
|---|---|---|
| `i8` → `i16` → `i32` → `i64` → `i128` | Yes | Always lossless |
| `u8` → `u16` → `u32` → `u64` → `u128` | Yes | Always lossless |
| `u8` → `i16`, `u16` → `i32`, `u32` → `i64`, `u64` → `i128` | Yes | An unsigned value fits the next signed size |
| `i8`, `i16`, `i32` → `f64` | Yes | Up to 32 bits fit `f64`'s 53-bit mantissa |
| `u8`, `u16`, `u32` → `f64` | Yes | Up to 32 bits fit `f64`'s 53-bit mantissa |
| `f32` → `f64` | Yes | Always lossless |
| `i64`, `u64` → `f64` | **No**: write `as f64` | Can lose precision above 2^53 |
| Any narrowing | **No**: write `as` | Data loss is possible |

Widening composes: `u8` → `i64` is lossless because `u8` → `i16` → `i64` is.

**At a binding or an argument, the whole table applies.** A value widens implicitly wherever its destination type is known, for example a `let` annotation or an argument: `let b: i64 = a_i32;`, `let f: f64 = a_i32;` and `g(a_i32)` all widen.

**In a binary operator, the narrower operand widens within its kind.** When the two operands have different numeric types of the same kind, and one converts losslessly to the other, that operand widens and the result has the other operand's type:
- integer to integer, by the integer rows of the table;
- float to float, `f32` to `f64`.

The result is always one of the two operand types, never a third: `i32 + u32` and `u64 + i64` are errors, because neither type converts losslessly to the other. Write `as` to choose.

**Integer to float needs `as` in an operator**, even where the table calls it lossless. A mixed-kind operator would hide integer division done earlier in the expression: `a_i32 / b_i32 + c_f64` divides in `i32`, truncating, before anything widens, which is a well-known bug in C and Java. Writing `(a_i32 as f64) / (b_i32 as f64) + c_f64` or `(a_i32 / b_i32) as f64 + c_f64` makes the choice visible.

**Which operators.** The rule covers the arithmetic operators `+ - * / %`, the bitwise operators `& | ^` and the comparisons. It does not cover the shifts ([Division, remainder and shifts](#division-remainder-and-shifts)).

**How it dispatches.** Widening is a coercion applied to primitive operands before the operator is dispatched. The primitives' operator impls all take `Self`-typed operands, and generic code (`T: Add`) never widens ([§9 Operator traits](#operator-traits)).

**Each operation runs in its own operands' type**, so arithmetic in a narrow type traps before anything widens. In `a_i32 * b_i32 + c_i64`, the product is computed and checked in `i32`, then widened to `i64` for the addition. `let c: i64 = a_i32 * b_i32;` behaves the same way.

```kara
let a: i32 = 7;
let b: u8 = 200;
let c: i64 = 1_000;
let d: u32 = 5;
let r: f64 = 0.5;
let h: f32 = 0.25;

let s = a + c;        // i64: `a` widens
let t = b + c;        // i64: u8 widens losslessly to i64
let u = c * d;        // i64: u32 widens to i64
let v = a + d;        // ERROR: neither i32 nor u32 converts losslessly to the other
let w = a * a + c;    // `a * a` is computed and checked in i32, then widened
let x = a + r;        // ERROR: integer to float in an operator; write `a as f64 + r`
let y = h * r;        // f64: f32 widens to f64
let z: f64 = a;       // OK: a binding widens i32 to f64
```

**Compound assignment.** `p op= e` computes `p op e` by the same rule, and the result must then convert implicitly to the type of `p`. So `total_i64 += n_i32` is legal, and `n_i32 += total_i64` is an error.

#### `as` casts

**Every numeric `as` cast is fully defined.** No numeric cast has undefined or implementation-defined behaviour, and none panics. Each source and target pair has one deterministic meaning:

| Source | Target | Meaning of `as` |
|---|---|---|
| `iN` / `uN` | `iM` / `uM`, lossless | Implicit, so `as` is not needed (see the lint below). |
| `i8` … `i32`, `u8` … `u32` | `f64` | Implicit at a binding or an argument. **`as` required** as an operand of an arithmetic or comparison operator ([Mixed-width operands](#mixed-width-operands)). |
| `iN` | `uM` | **`as` required.** The source bits are read as two's complement, then sign-extended or truncated to width M. `(-1i8) as u8 == 255`, `(-1i32) as u64 == 0xFFFFFFFF_FFFFFFFFu64`. |
| `iN` / `uN` | narrower `iM` / `uM` | **`as` required.** Keeps the low M bits; the sign of the result follows the target type. `0x1FFi32 as u8 == 0xFFu8`, `(-1i32) as u8 == 0xFFu8`, `300i32 as i8 == 44i8`. |
| `iN` / `uN` | `f32` / `f64`, lossy | **`as` required.** IEEE 754 round-to-nearest-even. `i64.MAX as f64` rounds to `9.223372036854776e18`, just above `i64.MAX`. |
| `f64` | `f32` | **`as` required.** IEEE 754 round-to-nearest-even; overflow gives `±Infinity`. (`f32` to `f64` is implicit.) |
| `f32` / `f64` | `iN` / `uN` | **`as` required. Saturates**; see [Float to integer conversion](#float-to-integer-conversion). |
| `bool` | `iN` / `uN` | **`as` required.** `false` is `0`, `true` is `1`. The reverse (`iN as bool`) is rejected; write `n != 0`. |
| `char` | `u32` / `i32` | **`as` required.** The Unicode scalar value (at most `0x10FFFF`, which fits either target). |
| `char` | `u8` / `u16` / `i8` / `i16` | **Rejected**: `error[E_CHAR_AS_NARROW_INT]: 'char as uN/iN' for N < 32 is rejected because it truncates the Unicode scalar value's low bits, which is not a meaningful operation; write 'c as u32 as uN' for explicit two-step truncation, or 'c.encode_utf8(buf)' for proper UTF-8 encoding`. The fix offers both. |
| `iN` / `uN` | `char` | **Rejected**: `error[E_INT_AS_CHAR]: cannot cast integer to char via 'as': not every integer is a Unicode scalar value (range '0..=0xD7FF \| 0xE000..=0x10FFFF'); use 'char.try_from(n) -> Result[char, CharError]'`. |
| `f32` / `f64` | `bool` | **Rejected**: meaningless. |
| `bool` | `bool` | Identity; the `redundant_cast` lint flags it. |

**The `lossless_cast` lint flags an `as` the program does not need**: one whose conversion would happen implicitly at that position (`let b: i64 = a_i32 as i64;`). Its fix removes the `as`. So an `as` in source marks a conversion that may change the value, or an integer-to-float conversion inside an operator, where the `as` is required.

`as` has no other meanings. Pointer casts are covered in [§15](#15-unsafe-ffi-and-layout-control); a [distinct type](#distinct-types) converts with its constructor and `.raw()`. There are no trait-object or subtype casts.

#### Float to integer conversion

**`f as iN` saturates.** For every `f32` or `f64` cast to any signed or unsigned integer type:

| Source value | Result |
|---|---|
| Finite, within `[iN.MIN, iN.MAX]` (or `[0, uN.MAX]`) | Truncated toward zero. `3.7f64 as i32 == 3`, `(-3.7f64) as i32 == -3`. |
| Finite, above `iN.MAX` (or `uN.MAX`) | `iN.MAX` (or `uN.MAX`). `1e30f64 as i32 == i32.MAX`, `1e30f64 as u8 == 255`. |
| Finite, below `iN.MIN` (or below `0` for unsigned) | `iN.MIN` (or `0`). `(-1e30f64) as i32 == i32.MIN`, `(-1.0f64) as u8 == 0`. |
| `+Infinity` | `iN.MAX` (or `uN.MAX`). |
| `-Infinity` | `iN.MIN` (or `0` for unsigned). |
| `NaN` (any payload) | `0`, for signed and unsigned targets alike. |

So: round toward zero; if the result is outside the target's range, saturate to the nearest end; NaN maps to zero. This is bit-identical to LLVM's `fptosi.sat` and `fptoui.sat`, so it compiles to one instruction.

**The checked conversion lives on the target type.** `iN.checked_from(f) -> Option[iN]` is `None` for NaN or an out-of-range value, and otherwise `Some` of the value truncated toward zero. It exists for every integer target (`i32.checked_from(f)`, `u8.checked_from(f)`). These two are the only float-to-integer spellings: `as` for the saturating conversion, `checked_from` for the checked one. A conversion that panics when out of range is `iN.checked_from(f).unwrap()`.

**Why `as` saturates instead of trapping.** `checked_from` already gives the checked behaviour, and `.unwrap()` on it the trapping one, so a trapping `as` would add one more spelling for something a call site can already ask for. Saturating is the most defensive default that is not a method: it never panics and never wraps silently. A trapping rule would also have to panic on NaN, which would put a `panics` effect into every function that does numeric work. Saturating with NaN mapped to `0` handles every edge case with one rule.

#### Floats

**`f32` and `f64` follow IEEE 754 exactly.** `NaN != NaN`. They implement `PartialEq` and `PartialOrd` but **not** `Eq`, `Ord` or `Hash`: NaN breaks the reflexivity `Eq` requires (`a == a` for every `a`) and the totality `Ord` requires. So `f64` cannot be a `Map` or `Set` key, or appear anywhere `Ord` is required. This keeps FFI simple: `f32` and `f64` cross `extern` boundaries and behave the same on both sides.

**Printing.** A float prints as the shortest decimal that reads back as the same value, always with a `.` or an exponent, so `1.0` never prints as `1`. `to_string()`, `println` and f-string interpolation all use this rendering; the exponent and special-value rules are in [library/core-types.md](library/core-types.md#printing).

**`F32` and `F64`: total-order floats.** The standard library provides `F32` and `F64` for contexts that need `Eq`, `Ord` or `Hash`. They store the same bits as `f32` and `f64` and order them by the IEEE 754 **totalOrder** predicate, with NaN normalized on the way in:

- **Ordering:** `-Infinity < ... < -0.0 < 0.0 < ... < +Infinity < NaN`. `-0.0` and `0.0` are **distinct, adjacent** values here: the wrapper compares bit patterns, not IEEE values, so `-0.0 == 0.0` is `false` (on `f64` it is `true`).
- **Equality is bit equality.** That is what makes the `Hash` impl sound: equal keys have identical bits, so they hash alike, with no normalization step that `Eq` and `Hash` could do differently.
- **NaN is one value.** Every NaN bit pattern is canonicalized to one quiet NaN when a wrapper is constructed (`F64.from(x)` and `F64 { value: x }` both do it). So `NaN == NaN` is `true`, and NaN sorts after every other value whatever sign and payload it arrived with.
- They implement `Eq`, `Ord` and `Hash`, so they work as `Map` and `Set` keys, in `SortedMap`, and wherever `T: Ord` is required.

Canonicalizing NaN is not cosmetic. Raw totalOrder distinguishes NaNs by sign, which places `-NaN` before `-Infinity` and `+NaN` after `+Infinity`, and nothing in the source chooses between them: on x86 a runtime `z / z` yields a negative NaN, while the same expression folded at compile time yields a positive one. Canonicalizing at construction makes the order a property of the program, not of how it was compiled.

The naming follows Kāra's convention: lowercase primitives (`f64`) have a minimal trait surface, and PascalCase library types (`F64`) carry richer semantics, like Java's `double` and `Double`.

```kara
Map[f64, String]                 // compile error: f64 does not implement Hash
Map[F64, String]                 // OK: F64 has a total order, NaN sorts last

let mut scores: Vec[f64] = [1.0, f64.NAN, 2.0];
scores.sort();                   // compile error: f64 has no total order

let mut ranked: Vec[F64] = scores.iter().map(|x| F64.from(*x)).collect();
ranked.sort();                   // OK: 1.0, 2.0, NaN

let a = F64.from(f64.NAN);
let b = F64 { value: 0.0 / 0.0 };
let same = a == b;               // true: every NaN canonicalizes to one value
```

**Diagnostic.** When `f64` appears where `Eq`, `Ord` or `Hash` is required, the compiler names the fix:

```
error: f64 does not implement Hash
  f64 follows IEEE 754: NaN != NaN, so NaN keys could never be retrieved.
  Use F64 instead: it defines a total order where NaN sorts after all finite values.

  Map[f64, String]
      ^^^
  help: change to Map[F64, String]
```

**FFI.** `f32` and `f64` cross FFI boundaries directly, IEEE on both sides. `F32` and `F64` must be unwrapped to `f32` and `f64` before they are passed to an `extern` function; the compiler rejects an implicit conversion.

**Serialization.** An `f32` or `f64` field in a `#[derive(Serialize, Deserialize)]` type triggers the `float_in_serialized_type` lint: JSON has no NaN, and MessagePack and Protobuf carry IEEE bits that consumers may read differently. The lint is a warning; `#[allow(float_in_serialized_type)]` silences it per field.

The reduced-precision floats `f16` and `bf16` belong to the data track ([deferred.md](deferred.md#m4c-data)).

### Tuples

A tuple groups a fixed number of values whose types may differ. The type and the value are written the same way:

```kara
let entry: (i64, f64, bool) = (3, 0.5, true);
let count = entry.0;              // elements are read by position
let (n, ratio, ok) = entry;       // or destructured with a pattern
```

- `()` is the unit type ([Primitive types](#primitive-types)).
- A tuple is `Copy` when every element is ([core-semantics.md §1.1](core-semantics.md#1-values)).
- Elements are evaluated left to right ([core-semantics.md §8](core-semantics.md#8-evaluation-order)) and dropped last to first ([core-semantics.md §7.8](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).
- An element of a tuple held in an owned local may be moved out on its own (a partial move, [core-semantics.md §3.6](core-semantics.md#3-moves)).
- A tuple of only shared handles and `Copy` values is a handle aggregate: using it counts its handles instead of moving it ([core-semantics.md §6.1](core-semantics.md#6-sharing-c7)).

### Arrays

`Array[T, N]` is a fixed-size array of `N` elements of type `T`, stored inline. `N` is a compile-time constant (const generics: [§8](#8-type-inference-and-generics)).

An array is written as a list literal with an `Array` expected type, or with the repeat form `[v; n]` ([Collection literals](#collection-literals)).

- Indexing is bounds-checked and uses `i64` ([Indexing](#indexing)).
- An array is `Copy` when `T` is ([core-semantics.md §1.1](core-semantics.md#1-values)). Its elements drop first to last ([core-semantics.md §7.8](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).
- Writing every slot of an uninitialized array one by one does not initialize it for [definite assignment](#variable-binding-rules); use a literal or `[v; n]`.
- An `Array[T, N]` argument passes as a `Slice[T]` ([Slices](#slices)).

### Collection literals

**List literals.** `[a, b, c]` is a `Vec` by default and follows the expected type: with an `Array[T, N]` or `Set[T]` annotation it builds that type. The repeat form `[v; n]` always builds an `Array`, and `n` must be a constant. A `Vec` of `n` copies is `Vec.filled(n, v)`.

```kara
let v = [1, 2, 3];                    // Vec[i64]
let a: Array[i64, 3] = [1, 2, 3];     // Array[i64, 3]: the expected type decides
let s: Set[i64] = [1, 2, 3];          // Set[i64]
let zeros = [0u8; 256];               // Array[u8, 256]
let filled = Vec.filled(n, 0u8);      // a Vec of runtime length `n`
```

- A length mismatch between a literal and its `Array[T, N]` annotation is a compile error.
- `let v: Vec[i64] = [0; 8];` is an error, because `[v; n]` is an `Array`. Its fix is `Vec.filled(8, 0)`.

**Map literals.** `[k1: v1, k2: v2]` is a `Map` by default, and a `SortedMap` where one is expected. `[:]` is the empty map.

```kara
let scores = ["alice": 10, "bob": 7];                  // Map[String, i64]
let ranked: SortedMap[String, i64] = ["bob": 7];       // SortedMap[String, i64]
let empty: Map[String, i64] = [:];
```

An empty literal, `[]` or `[:]`, takes its element types from the expected type.

There is one way to write each collection: `[a, b]`, `[v; n]`, `[k: v]`, `[:]`, `Vec.new()` and `Vec.filled(n, v)`, plus the `from` constructors (`Set.from([1, 2, 3])`, `Map.from([(k, v)])`) where no expected type is available. There is no `vec!` and no `Vec[...]` or `Set[...]` prefix literal. `Vec`, `Map`, `Set` and the other collections are library types ([library/collections.md](library/collections.md)).

### Structs

A struct is a named record of fields:

```kara
struct User {
    pub name: String,        // visible to users of the package
    pub email: String,
    password_hash: String,   // visible in the package, not to its users
}

impl User {
    pub fn new(name: own String, email: own String, password: String) -> User {
        User { name, email, password_hash: hash(password) }
    }
}
```

- **Field visibility** takes the three levels of [§4 Visibility](#visibility): a field is visible in its package by default, to users of the package with `pub`, and in its directory only with `private`. Code that cannot see a field cannot name it, in a field access or a struct literal; the diagnostic suggests a constructor or an accessor.
- **Derived impls** (`#[derive(Eq)]`) see every field regardless of visibility, because the generated code lives in the struct's module ([§9](#derive)).
- **Fields are mutable only through the binding.** A field of a plain struct can be assigned when the value is held in a `let mut` binding or reached through `mut ref`: a `mut ref S` can write every field, a `ref S` none. `mut port: i64` on a plain struct field is a compile error. A `mut` on a field is valid only in a `shared` type, whose values are aliased, so `mut` marks the fields that may change ([§11](#shared-types)). The fields of a `sync` type are never `mut`; they change through `Atomic` and `Mutex` methods ([§11](#sync-types)).
- **`shared struct` fields** combine visibility and `mut`:
  - `pub mut field: T`: visible to users. Inside the package it may be assigned directly. Users of the package change it only through `pub fn` methods.
  - `pub field: T`: visible to users, never assigned after construction.
  - `mut field: T`, `field: T`: package-internal, mutable or not.
  - `private mut field: T`, `private field: T`: directory-only, mutable or not.
- **Unit struct.** A struct with no fields is declared and built with empty braces: `struct Marker {}`, `struct PhantomData[T] {}` and `let m = Marker {};`. It has no runtime data, and suits type tags and stateless trait implementors. The form `struct Marker;` is not valid.
- **A struct with a `ref`, `Str` or `Slice` field is a view** and borrows like a reference ([core-semantics.md §5.2](core-semantics.md#5-references-and-views-c5)).
- Fields drop in reverse declaration order, after the type's own `Drop` body ([core-semantics.md §7.8](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).
- `shared struct` and `sync struct` have reference semantics, and `weak` fields break cycles between them ([§11](#11-ownership-and-sharing)).

**Struct literals.** Every field is given a value, in any order. The values are evaluated in written order ([core-semantics.md §8.1](core-semantics.md#8-evaluation-order)) and move into the struct.

```kara
let x = 1.0;
let y = 2.0;
let p = Point { x: x, y: y };
let q = Point { x, y };                          // shorthand: the variable has the field's name

let alice = User { name: load_name(), email: load_email(), password_hash: hash(pw) };
let bob = User { name: other_name(), ..alice };  // email and password_hash come from alice
```

The update form `..base` must come last. Every field not listed is taken from `base`, which must have the same struct type. Fields taken from `base` move out of it, or are copied when they are `Copy`, by the partial-move rules of [core-semantics.md §3.6](core-semantics.md#3-moves). Above, `alice.name` is still usable after the update.

Derived behaviour (`#[derive(...)]`) is covered in [§9](#9-traits).

### Enums

An enum is a closed set of variants. A variant has no payload, a tuple payload, or named fields:

```kara
enum Token {
    Eof,                                    // unit variant
    Integer(i64),                           // tuple variant
    Identifier(String),                     // tuple variant
    Error { message: String, line: i64 },   // struct variant
}

let t = Token.Integer(42);
let e = Token.Error { message: describe(), line: 7 };
```

A variant is built with its qualified name. In a pattern, the enum name may be left out when the scrutinee's type is known: `Circle { radius }` and `Shape.Circle { radius }` match the same values. The prelude variants `Some`, `None`, `Ok` and `Err` are always written bare.

**A payload may be any type, another enum included.**

```kara
enum Command { Start, Stop, Restart { delay_ms: i64 } }
enum Step { Do(Command), Wait(i64), Done }

fn run(step: Step) {
    match step {
        Step.Do(Command.Restart { delay_ms }) => restart(delay_ms),
        Step.Do(cmd) => execute(cmd),
        Step.Wait(ms) => pause(ms),
        Step.Done => {}
    }
}
```

**An enum is `Copy` only with `#[derive(Copy)]`**, even when no variant has a payload ([core-semantics.md §1.1](core-semantics.md#1-values)). So adding a payload to a variant later does not silently change how callers' values move.

**Recursive enums require `shared enum`.** A plain enum whose payload contains the same enum directly would have infinite size, and is a compile error:

```
error: enum `Expr` is recursive without indirection
  --> src/query.kara:3:5
   |
3  |     And(Expr, Expr),
   |         ^^^^ contains `Expr`, which transitively contains `And`, which contains `Expr`...
   |
   = help: use `shared enum Expr` for counted tree nodes
   = help: or wrap the recursive field in `Vec` for spine-only recursion
```

The idiomatic fix for tree-shaped data (query trees, expression trees, JSON values) is `shared enum`:

```kara
shared enum Expr {
    And(Expr, Expr),    // OK: each payload holds a counted handle to a child
    Or(Expr, Expr),
    Eq { column: String, value: Value },
    Literal(Value),
}
```

A `shared enum` gives each node reference semantics, so every payload is a fixed-size handle to its children ([§11](#11-ownership-and-sharing)). A cycle of strong handles is never freed ([core-semantics.md §6.5](core-semantics.md#6-sharing-c7)).

- The active variant's payload drops last to first ([core-semantics.md §7.8](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).
- Explicit discriminants and `#[repr(intN)]` are covered in [§15](#15-unsafe-ffi-and-layout-control); `#[non_exhaustive]` in [§11](#11-ownership-and-sharing); `#[derive(Display)]` on enums in [§9](#9-traits).

### `Option` and `Result`

Absence and failure are ordinary enums from the prelude:

```kara
enum Option[T] { Some(T), None }
enum Result[T, E] { Ok(T), Err(E) }
```

- Their variants are always written bare: `Some(x)`, `None`, `Ok(v)`, `Err(e)`.
- `Option[T]` is `Copy` when `T` is ([core-semantics.md §1.1](core-semantics.md#1-values)). An `Option` of shared handles is a handle aggregate ([core-semantics.md §6.1](core-semantics.md#6-sharing-c7)), and `Option[ref T]` is a view ([core-semantics.md §5.2](core-semantics.md#5-references-and-views-c5)).
- `?` propagates `None` and `Err` ([§10](#results-and-propagation)), and `??` supplies a default in their place ([`?` and `??`](#-and-)). Error handling, the `Error` trait and `AnyError` are in [§10](#10-errors-and-panics). The methods are in [library/core-types.md](library/core-types.md).

### Strings

Two types cover text:

| Type | What it is | When to use |
|---|---|---|
| `String` | Owned UTF-8 text on the heap | The default: owning and building text |
| `Str` | A borrowed view of UTF-8 text: a pointer and a length | Parsing, splitting, and passing text without copying |

A string literal is a `String` by default and a `Str` where a `Str` is expected; a literal typed as `Str` views static data and borrows nothing ([§3 String literals](#string-literals)).

**`String`, the default.** Owned, growable, UTF-8 encoded.

```kara
fn greeting(name: String) -> String {
    "Hello, " + name
}
```

**`Str`, a zero-copy view.** A `Str` views UTF-8 bytes that something else owns: a pointer and a length, with no allocation. It is `Copy`, and it borrows exactly like a `ref` ([core-semantics.md §5](core-semantics.md#5-references-and-views-c5)), so it cannot outlive the text it views. A function returns it by value; there is no `ref Str`.

**A `String` passes as `Str`.** A parameter of type `Str` accepts a `String` place, borrowed, the way a `Slice[T]` parameter accepts a `Vec[T]` ([Slices](#slices)). The caller writes the string and the compiler builds the view. As with slices, this happens at call boundaries, a method receiver included: a `String` receiver also finds `Str`'s methods ([§9 Method resolution](#method-resolution)). Elsewhere write `s.as_str()`.

```kara
fn first_word(s: Str) -> Str {
    let end = s.find(' ').unwrap_or(s.len());
    s[0..end]                         // no allocation: a view into the caller's text
}

let line = "alice,30,engineer";
let w = first_word(line);             // Str: borrows from `line`
let owned: String = w.to_string();    // an owned copy, to keep beyond `line`
```

`first_word` has one view parameter, so its result borrows from it ([core-semantics.md §5.4](core-semantics.md#5-references-and-views-c5)). Returning a view of a `String` the function owns is an error, because that `String` is dropped when the function returns. To keep text beyond the borrow, call `.to_string()`.

Splitting methods (`split`, `lines` and the like) return lazy iterators of `Str` views into the original text ([library/strings.md](library/strings.md)). There is no copy-on-write and no hidden retention: the programmer always knows whether they hold a view or a copy.

**`Str` is not `Slice[u8]`.** The UTF-8 guarantee is carried by the type, not by the element type, and a byte slice would lose it. `Slice[T]` covers every other sequence; `Str` covers text.

**Characters.** `char` is one Unicode scalar value. `for c in s` iterates over characters, decoding UTF-8. Byte access is explicit: `s.bytes()` gives a `Slice[u8]`.

**Indexing text.**

- **`s[a..b]` yields a `Str`** over the **byte** range `[a, b)`: the same offsets that `len()`, `find` and `bytes()` use, not character indices. It panics if a bound is out of range, or if a cut lands **inside** a multi-byte character. The alternatives would be to hand back raw bytes, which would let text hold invalid UTF-8 and break the guarantee this section rests on, or to substitute U+FFFD, which silently returns a plausible value of a different length than the caller asked for. Find a valid cut with `find`, or work in characters with `chars()`.
- **`s[i]` is a compile error.** The syntax looks O(1), but finding the i-th character of UTF-8 text is O(n). The diagnostic names both alternatives:

```
error: String does not support indexing with []
  s[i] would hide an O(n) scan: Strings are UTF-8 encoded and characters
  are variable-width.
  help: use s.char_at(i) for the i-th character (O(n)),
        or s.bytes()[i] for raw byte access (O(1))
```

- `s.char_at(i)` returns `Option[char]`: `None` when out of range, never a panic. For iteration, prefer `for c in s` (O(n) in total) over repeated `s.char_at(i)` (O(n²)). When repeated indexed access is really needed, convert once: `let chars: Vec[char] = s.chars().collect();`, after which `chars[i]` is honestly O(1).
- `s.bytes()[i]` returns a `u8` and panics when out of range, like any slice. Use it for protocols and binary formats, not text.

**Equality is byte equality.** `==` on text compares raw UTF-8 bytes, and `Hash` hashes the same bytes, so two strings that look identical in different Unicode normalization forms are not equal. Normalize both sides first when that matters ([library/strings.md](library/strings.md)).

### Slices

`Slice[T]` is a borrowed view of contiguous elements: a pointer and a length (`{ ptr: *const T, len: i64 }`). It is the general counterpart of `Str`, and it lets one function work over a `Vec[T]`, an `Array[T, N]`, or part of either.

```kara
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc += x; }     // `x` is a `ref i64`, read as an `i64`
    acc
}

let v: Vec[i64] = [1, 2, 3, 4];
let a: Array[i64, 3] = [10, 20, 30];

let s1 = sum(v);          // a Vec[i64] argument passes as Slice[i64]
let s2 = sum(a);          // so does an Array[i64, 3]
let s3 = sum(v[1..3]);    // a sub-range is a Slice[i64] too
```

**Coercion at call boundaries.** A `Slice[T]` parameter accepts a `Vec[T]` or `Array[T, N]` place, borrowed, or a `Slice[T]`. The caller writes the collection and the compiler builds the slice header. This keeps signatures to one type instead of three, without a dereference chain users cannot name. The conversion happens only at call boundaries, a method receiver included: a `Vec[T]` or `Array[T, N]` receiver also finds `Slice[T]`'s methods ([§9 Method resolution](#method-resolution)). `let` and assignment do not convert. Where a slice must be named directly (inside a closure, in a `match` arm), call `.as_slice()`:

```kara
let window = v.as_slice();    // Slice[i64] as a first-class value
```

**Mutable slices.** An in-place view is written with the `mut` modifier, as with `ref T` and `mut ref T`:

```kara
fn sort_in_place[T: Ord](xs: mut Slice[T]) {
    // …
}

let mut v = [3, 1, 4, 1, 5];
sort_in_place(mut v);            // a `mut` Vec argument passes as `mut Slice[i64]`
sort_in_place(mut v[1..4]);      // a mutable view of part of `v`
```

A `mut ref Vec[T]` or `mut ref Array[T, N]` passes as `mut Slice[T]` at call boundaries, by the same rule as the read-only case. Mutability is a modifier on one type, not a second type.

**Borrowing.** `Slice[T]` borrows like `ref T`: it cannot outlive the collection it views, and many read-only views or one mutable view may exist at a time, never both ([core-semantics.md §5.6](core-semantics.md#5-references-and-views-c5)). `Slice[T]` is `Copy`, so passing it does not invalidate the caller's binding. `mut Slice[T]` is not `Copy`: a mutable view is unique, like `mut ref T`.

**`split_at_mut` splits one mutable borrow into two disjoint ones.**

```kara
fn split_at_mut(mut ref self, mid: i64) -> (mut Slice[T], mut Slice[T])
```

The first half covers indices `[0, mid)` and the second `[mid, len)`. It panics if `mid > self.len()`. The halves cannot overlap, so both may be live at once without breaking the one-mutable-view rule. The collection itself may not be used while either half is live. This is the basic tool for parallel writes into one buffer: give each task one half, and their write regions are disjoint by construction.

### Indexing

**Indices are `i64`.** Sizes, lengths and indices all use `i64`, so no cast is needed between an index and other integers. `usize` exists only for FFI.

- **`v[i]` is bounds-checked.** An index outside `0..v.len()` panics ([core-semantics.md §10.1](core-semantics.md#10-panics-and-errors-c8)). A negative index is out of bounds like any other: there is no Python-style wrap-around, because `-1` as an index is always a bug.
- **Accessors return `Option` instead of panicking.** `v.get(i)`, `v.first()` and `v.last()` answer `None` where `v[i]` would panic ([library/collections.md](library/collections.md)).
- **Indexing does not move.** `v[i]` is a place. Binding a non-`Copy` element by value is an error; borrow it or `.clone()` it ([core-semantics.md §3.7](core-semantics.md#3-moves), and the index operator in [§7](#7-functions-and-closures)).
- **Range indexing yields a view.** `v[a..b]` is a `Slice[T]`, or a `mut Slice[T]` where a mutable view is needed, and the half-open forms work too ([Loops and labels](#loops-and-labels)). On text, `s[a..b]` is a `Str` and `s[i]` is an error ([Strings](#strings)).
- What `[]` accepts on a type is set by its `Index` and `IndexMut` implementations ([§9](#9-traits)). The multi-index form `e[i, j]` is described under [Operators and precedence](#operators-and-precedence).

### Type aliases

A type alias names an existing type, and may add generic parameters with bounds. An alias is transparent: every use behaves exactly as if the aliased type were written there, except that **bounds on the alias's generic parameters are checked at every use site**:

```kara
type Counts[T: Eq + Hash] = Map[T, i64];
type SortedVec[T: Ord] = Vec[T];
type Cache[K: Eq + Hash, V: Clone] = Map[K, V];

let by_name: Counts[String] = Map.new();     // OK: String is Eq + Hash
let by_score: Counts[f64] = Map.new();       // ERROR: f64 does not implement Eq or Hash
```

**Bounds are enforced, not ignored.** Bounds on an alias's generic parameters are part of its interface. A program that writes `type X[T: Ord] = Vec[T]` gets the bound it wrote: every use of `X[U]` checks `U: Ord`.

**Bounds add to the aliased type's own bounds.** `type SortedVec[T: Ord] = Vec[T]` adds a requirement that `Vec` does not have. `SortedVec[String]` checks `String: Ord`, and for every other purpose the alias is `Vec[String]`. When the aliased type already requires the bound (`type SortedKeyMap[T: Ord] = SortedMap[T, i32]`, where `SortedMap` already needs `K: Ord`), the bound is redundant but accepted, with `warning[redundant_alias_bound]: bound 'T: Ord' on alias 'SortedKeyMap' is implied by the underlying type 'SortedMap[T, i32]'; consider removing`. `#[allow(redundant_alias_bound)]` keeps it when the author wants to document the requirement.

**Diagnostic.** When a use site does not satisfy an alias's bound:

```
error[E_TYPE_ALIAS_BOUND_NOT_SATISFIED]: 'f64' does not satisfy 'T: Eq + Hash' required by alias 'Counts'
  --> src/lookup.kara:42:26
   |
42 |     let by_score: Counts[f64] = Map.new();
   |                          ^^^ this argument requires 'Eq + Hash'
   |
note: alias 'Counts' declared at src/lookup.kara:5:1 with bound 'T: Eq + Hash'
help: 'f64' implements neither 'Eq' nor 'Hash' (NaN != NaN); use the total-order type 'F64' as the key
```

**Across packages.** The bounds of a `pub type` are part of its published interface. Removing or weakening a bound is not a breaking change, because callers passing tighter types still satisfy it. Adding or tightening a bound is breaking, because callers passing types that met the old bound may not meet the new one. This matches how bounds evolve on `pub fn` signatures and `pub struct` parameters.

**Why enforce the bounds instead of rejecting them.** The alternative is to forbid bounds on aliases, so they have to live on the aliased type or at each use. That was rejected for two reasons:

1. **The bound documents the alias.** `type Counts[T: Eq + Hash] = Map[T, i64]` says "keys must be `Eq + Hash`" at the place a reader looks first.
2. **Aliases that add constraints are useful.** `type SortedVec[T: Ord] = Vec[T]` says the alias is for sorted contents. The requirement is real; rejecting it would scatter it across every use site.

### Distinct types

A distinct type is a zero-cost wrapper that keeps structurally identical but semantically different values apart:

```kara
distinct type UserId = i64;
distinct type PostId = i64;
distinct type Meters = f64;
distinct type Seconds = f64;

fn get_user(id: UserId) -> User {
    load_user(id.raw())
}

let uid = UserId(42);
let pid = PostId(42);
get_user(pid);    // COMPILE ERROR: expected UserId, got PostId
get_user(uid);    // OK
```

**No operation carries over by default.** A distinct type is opaque: no arithmetic and no comparison unless it opts in with `#[derive]`:

```kara
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Display)]
distinct type UserId = i64;

#[derive(PartialEq, Eq, PartialOrd, Ord, Arithmetic)]
distinct type FloorNum = i64;  // + - * / % and unary - work, but only FloorNum with FloorNum
```

A distinct type does not inherit `Copy` from its base either ([core-semantics.md §1.1](core-semantics.md#1-values)); derive it when wanted.

**Conversion:**
- Wrap with the constructor: `UserId(42)`.
- Unwrap with `uid.raw()`, which returns the base type.

**Generics.** `fn find[T: Eq](items: Slice[T], target: T) -> Option[i64]` works on a `Vec[UserId]` when `UserId` derives `Eq`.

For matching, a distinct type has its base type's values ([Pattern exhaustiveness](#pattern-exhaustiveness)). Value predicates on a distinct type (`distinct type Port = u16 where …`) belong to the verification track ([deferred.md](deferred.md#verification)).

### Pattern matching

`match` tests a value against patterns in order and runs the first arm that matches:

```kara
const PI: f64 = 3.14159;

enum Shape {
    Circle { radius: f64 },
    Rectangle { width: f64, height: f64 },
    Triangle { a: Vec2, b: Vec2, c: Vec2 },
}

fn area(shape: Shape) -> f64 {
    match shape {
        Circle { radius } => PI * radius * radius,   // `shape` is borrowed; `radius` is `Copy`, so it binds a copy
        Rectangle { width, height } => width * height,
        Triangle { a, b, c } => triangle_area(a, b, c),
    }
}
```

An arm is `PATTERN [if GUARD] => BODY`. A body is an expression, a block, or an assignment or compound assignment (`x => total += x,`). A comma is required after an arm whose body is not a block, and optional after a block. A `match` must be exhaustive ([Pattern exhaustiveness](#pattern-exhaustiveness)).

**Pattern forms.**

| Pattern | Matches |
|---|---|
| `_` | Anything; binds nothing |
| `name` | Anything; binds it to `name` |
| `ref name`, `mut ref name` | Anything; binds a borrow of it ([binding modes](#binding-modes-in-patterns)) |
| `42`, `'a'`, `true`, a `const` | That value |
| `lo..=hi` and the other [range patterns](#range-patterns) | A value in the range |
| `name @ P` | What `P` matches; also binds the whole to `name` ([`@` bindings](#-bindings)) |
| `Point { x, y: 0, .. }` | A struct or struct variant, field by field |
| `Some(p)`, `Token.Integer(n)` | A tuple variant |
| `Token.Eof` | A unit variant |
| `(p, q)` | A tuple |
| `[p, .., q]` | An array or slice, by position ([Slice and array patterns](#slice-and-array-patterns)) |
| `P \| Q` | Either alternative ([Or-patterns](#or-patterns)) |

In a field pattern, `field` alone is short for `field: field`: the field name is both the selector and the new binding. Field patterns nest (`Outer { inner: Inner { b } }`). `{ field, .. }` matches some fields and skips the rest; `..` must come last, and `{ .. }` alone skips everything. Inside a pattern's braces, `..` is always the rest marker, never a range. A bare `..` is not a pattern on its own; use `_`.

**Binding modes.** Whether a binding moves, copies or borrows is decided by the pattern and the scrutinee's type, never by how the arm uses it. The rules are in [§7 Binding modes in patterns](#binding-modes-in-patterns).

**There is no `mut name` pattern.** `mut` before a whole `let` pattern makes every binding mutable ([Variable binding rules](#variable-binding-rules)). In a `match`, `if let` or `while let` arm, rebind with `let mut x = x;` inside the body. `mut ref name` is a mutable borrow, not a mutable binding.

**Pattern guards.** An arm may add `if EXPR` between the pattern and `=>`. The guard runs only when the pattern matches; if it is false, matching continues with the next arm. A guarded arm does not count toward exhaustiveness, so an unguarded arm or a wildcard is still required. A guard's effects count toward the enclosing function's effects like any other subexpression, and its temporaries drop at the end of the guard ([core-semantics.md §7.4](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).

```kara
match score {
    Ok(n) if n >= 90 => "A",
    Ok(n) if n >= 70 => "B",
    Ok(_)            => "C or below",   // unguarded: required
    Err(_)           => "invalid",
}
```

#### Or-patterns

An arm may list several patterns separated by `|`. It fires if any of them matches.

```kara
match event {
    MouseDown { x, y } | TouchStart { x, y } => handle_click(x, y),
    _ => {}
}
```

- Every alternative must bind the **same names with the same types**. `A(x) | B(y)` is an error, and so is an `x` whose type differs between alternatives.
- Or-patterns may be nested: `Foo(A | B)` means `Foo(A) | Foo(B)`.
- A guard applies after either alternative matches: `P1 | P2 if guard => body`. It runs once per match, not once per alternative.
- Or-patterns work in `if let` and `while let`: `if let Left(x) | Right(x) = val { … }`.
- For exhaustiveness, an or-pattern arm counts for all its alternatives at once.

#### Range patterns

A range pattern matches a contiguous range of integer or `char` values:

| Form | Matches |
|---|---|
| `lo..=hi` | `[lo, hi]` |
| `lo..hi` | `[lo, hi)` |
| `lo..` | `[lo, ∞)` |
| `..=hi` | `(-∞, hi]` |
| `..hi` | `(-∞, hi)` |

```
RANGE_PATTERN  = CONST_PAT_BOUND ".."  CONST_PAT_BOUND   // exclusive,    [lo, hi)
               | CONST_PAT_BOUND "..=" CONST_PAT_BOUND   // inclusive,    [lo, hi]
               | CONST_PAT_BOUND ".."                    // unbounded above, [lo, ∞)
               |                  ".."  CONST_PAT_BOUND  // exclusive end, (-∞, hi)
               |                  "..=" CONST_PAT_BOUND  // inclusive end, (-∞, hi]

CONST_PAT_BOUND = LITERAL                  // integer or char literal
                | QUALIFIED_PATH           // a module-level `const` of integer or char type
```

```kara
match c {
    'a'..='z' => "lowercase",
    'A'..='Z' => "uppercase",
    '0'..='9' => "digit",
    _ => "other",
}

match code {
    200..=299 => "success",
    400..=499 => "client error",
    500..=599 => "server error",
    _ => "other",
}

match n {
    ..=-1   => "negative",
    0       => "zero",
    1..=9   => "single digit",
    10..    => "large",
}
```

- When both bounds are present they must have the same type, integer or `char`. Float ranges are not supported, because of NaN ordering.
- A bounded inclusive range needs start ≤ end; `9..=0` is a compile error. The check runs after named bounds are resolved.
- Range patterns compose with or-patterns (`0..=9 | 100..=199 => …`) and `@` bindings (`n @ 0..=9 => …`).
- For exhaustiveness, a range pattern covers exactly its values. A match over integers or `char` still needs a wildcard arm: the compiler does not check whether a set of ranges partitions the whole type.

**Named bounds.** A bound may be a literal or a path to a module-level `const` of integer or `char` type. The path is resolved during type checking and the constant's value is used as the bound:

```kara
const MIN_AGE: i32 = 18;
const MAX_AGE: i32 = 65;

match age {
    ..MIN_AGE         => "minor",
    MIN_AGE..=MAX_AGE => "working age",
    _                 => "retired",
}
```

A named bound follows the same type, ordering, exhaustiveness and composition rules as a literal; it only names a value. A path that does not resolve to such a constant is `error[E_RANGE_PATTERN_BOUND_NOT_CONST]: range pattern bound 'PATH' must resolve to a module-level integer or char const`, which says whether the path was not found, not a const, of the wrong type, or not module-level. Two bounds of different resolved types fall under the same-type rule, and the diagnostic names both types. Calls and other constant expressions are not accepted in a pattern, only literals and bare paths, so that patterns stay simple enough for the exhaustiveness algorithm.

#### `@` bindings

`name @ P` tests `P` and also binds the whole value at that position to `name`:

```kara
match val {
    x @ Some(_) => f"got: {x}",
    None => "nothing",
}

match age {
    n @ 0..=12 => f"child, age {n}",
    n @ 13..=19 => f"teenager, age {n}",
    _ => "adult",
}

match response {
    Response { status: code @ 500..=599, body } => log_error(code, body),
    _ => ok(),
}
```

- The binding has the type of the scrutinee at that position.
- `@` composes with or-patterns: `x @ (A | B)` is valid when `A` and `B` have the same type, and `x @ Some(_) | x @ None` is valid because both alternatives bind `x` with the same type.
- For exhaustiveness, `x @ P` is the same as `P`.

**Ownership.** `name` is an ordinary binding, so its mode follows [§7 Binding modes in patterns](#binding-modes-in-patterns). Under a borrowed scrutinee, `name` and every binding inside `P` borrow, except that a binding of a `Copy` part copies it. Under an owned scrutinee, a plain `name` moves the whole value (or copies it, if it is `Copy`). `P` may then not bind any non-`Copy` part of that value, by value or by `ref`, because the whole has already moved. To bind both the whole and a part, borrow both: `ref x @ Some(ref y)`. The same rule applies to nested `@` bindings (`outer @ Foo { field: inner @ Bar(value) }`): under an owned scrutinee, an outer binding that takes a value conflicts with any inner binding of a non-`Copy` part of it, and `ref` resolves the conflict.

**Refutability.** `@` itself never fails, so `name @ P` is refutable exactly when `P` is. `let p @ Point { x, y } = point;` is accepted (the fields are `Copy`, so binding them does not conflict with `p`); `let x @ Some(y) = opt;` is rejected because `Some(_)` can fail, so use `if let` or `let…else`.

#### Slice and array patterns

A slice pattern matches a sequence by position and may name a remainder. Each `pᵢ` is a pattern and `..` is the rest marker:

| Form | Matches |
|---|---|
| `[p₁, …, pₙ]` | Exactly n elements |
| `[p₁, …, pₖ, ..]` | A k-element head, tail ignored |
| `[.., q₁, …, qⱼ]` | A j-element tail, head ignored |
| `[p₁, …, pₖ, .., q₁, …, qⱼ]` | Head and tail, middle ignored |
| `[p₁, …, pₖ, ..rest]` | Head, with the tail named |
| `[..rest, q₁, …, qⱼ]` | Tail, with the head named |
| `[p₁, …, pₖ, ..rest, q₁, …, qⱼ]` | Head and tail, with the middle named |

```kara
fn describe(items: Slice[i64]) -> String {
    match items {
        []                                  => "empty",
        [only]                              => f"one: {only}",
        [first, .., last] if first == last  => f"same at both ends: {first}",
        [first, ..rest]                     => f"{first}, then {rest.len()} more",
    }
}
```

**Types matched.** Slice patterns apply to `Array[T, N]`, `Vec[T]` and `Slice[T]`. They do not apply to `String` or `Str`: UTF-8 characters vary in width, so positional byte patterns would cut characters. Match `s.bytes()` (a `Slice[u8]`) or a collected `Vec[char]` instead.

**Element bindings.**
- **Over a `Vec[T]` or a `Slice[T]`, the pattern sees a view.** Every element binding is a `ref`, or a `mut ref` when written `mut ref name` against a mutable place, and nothing moves out. A `Vec[T]` moves whole or not at all.
- **Over an owned `Array[T, N]`, the pattern destructures like a tuple.** Element bindings follow [§7 Binding modes in patterns](#binding-modes-in-patterns), so a plain binding may move an element out.

**The rest binding borrows, never moves**, even under an owned scrutinee; it is valid for the arm's body. An `Array[T, N]` is stored inline, so moving out a contiguous range would leave a half-initialized array.
- On `Vec[T]` and `Slice[T]`, `..rest` binds a `Slice[T]`: a view, with no copy and no allocation. Written `..mut ref rest` against a mutable place (a `let mut` owner, a `mut ref` or a `mut Slice[T]`), it binds a `mut Slice[T]`.
- On `Array[T, N]` with a k-element head and a j-element tail, `..rest` covers `K = N − k − j` elements, computed by the same constant arithmetic as const generics. If `K < 0`, the pattern is rejected with a length-mismatch diagnostic. Over an owned array, `..rest` is an `Array[T, K]`; over a `ref Array[T, N]`, a `ref Array[T, K]`; and written `..mut ref rest` over a `mut ref Array[T, N]`, a `mut ref Array[T, K]`.

**At most one rest marker per slice pattern.** `[a, .., b, .., c]` is a parse error, because the element positions would be ambiguous. Nested slice patterns (`[[..a, b], [c, ..d]]` over `Array[Array[T, M], N]`) each have their own single rest marker.

**Exhaustiveness.**
- An `Array[T, N]` match is exhaustive when its fixed-position patterns cover all `N` positions, or when a rest marker covers the trailing positions. The rest counts as a wildcard whether or not it is named.
- A `Vec[T]` or `Slice[T]` match needs an arm that covers every length. `[x, ..rest]` covers every non-empty sequence, so `[]` or `_` is still needed for the empty one.

**Refutability.** On `Vec[T]` and `Slice[T]` a slice pattern is refutable, since the length may not match: it is allowed in `match`, `if let`, `while let` and `let…else`, never in a plain `let`. On `Array[T, N]` it is refutable only if an element pattern is, and otherwise a plain `let` accepts it.

### Pattern exhaustiveness

Kāra uses **Maranget's usefulness algorithm** ("Warnings for pattern matching", JFP 2007), the decidable algorithm that Rust, OCaml and Scala use. It is specified here so implementers need not chase the paper, and so later extensions have a fixed baseline.

**Two judgments.** The algorithm is built on one primitive, **usefulness**:

$$U(P, q) = \text{there exists a value } v \text{ matched by } q \text{ that is not matched by any row of } P$$

where `P` is a *pattern matrix* (the rows of a match, each a tuple of patterns) and `q` is a candidate pattern. Two checks follow:

- **A match is exhaustive** iff `U(P, _)` is **false**: the wildcard is not useful against `P`, so every value of the scrutinee type is matched by some row. A non-exhaustive match produces a diagnostic naming one uncovered pattern, found by the same recursion.
- **Arm `i` is reachable** iff `U(P[0..i], P[i])` is **true**: the arm is useful against the earlier arms. An unreachable arm is a warning.

**How `U` is computed**, by recursion on the head of `q`:

1. **`q = _` and every column is empty**: `U` is true iff the matrix has no rows. This is the base case.
2. **`q` starts with a constructor `c(q1, …, qa)`**: *specialize* `P` to `c`. Drop the rows whose first column does not match `c`, replace the first column of the others with the constructor's fields, and recurse on the specialized matrix and the specialized `q`.
3. **`q` starts with `_`**: if every constructor of the column's type is covered by some row's head, recurse on the *default matrix* (the rows whose first column is `_`, with that column dropped). Otherwise `q` is useful, because a missing constructor witnesses a value `P` does not match.
4. **`q` starts with an or-pattern `q1 | q2`**: `U(P, q1 | q2) = U(P, q1) ∨ U(P_after_q1, q2)`, where `P_after_q1` is `P` with a row for `q1` added. Specialization recurses through or-patterns without expanding them.

This is the full algorithm for finite constructor spaces. The rules below cover types whose constructor space is not finite (integers, floats, strings, `Vec`) and types that wrap others (distinct types, `Array[T, N]`).

#### Type-specific rules

**`bool`** has two constructors, `true` and `false`. A match with both arms is exhaustive without a wildcard:

```kara
match flag {
    true  => "yes",
    false => "no",   // exhaustive: no wildcard needed
}
```

**Enums.** The constructors are the declared variants. Adding a variant breaks every match over the enum that has no wildcard arm; `#[non_exhaustive]` declares that a public enum's variant set is expected to grow ([§11](#11-ownership-and-sharing)).

**Structs and tuples** have one constructor. Specialization always applies, and exhaustiveness and reachability carry through to the fields.

**Integers, floats, strings and `char`.** The constructor space (2^64 values for `i64`, unbounded for `String`) is not enumerated. A match over one of these types is exhaustive **only** if some row matches anything: `_`, a plain binding, or a half-open range that covers the remaining values. Listing every `i64` literal is still non-exhaustive; the algorithm does not try. Range patterns cover contiguous integer or `char` values (not floats).

**`Array[T, N]`.** `N` is known, so an array pattern `[p1, p2, …, pN]` specializes like a tuple: `[a, b, c]` is exhaustive on `Array[T, 3]` iff each of `a`, `b` and `c` is exhaustive on `T`. A rest pattern counts as a wildcard for the positions it covers ([Slice and array patterns](#slice-and-array-patterns)).

**`Vec[T]`, `Map[K, V]`, `String`.** Variable length. A match over these is exhaustive only with a wildcard arm, except that on `Vec[T]` the slice patterns `[]` and `[x, ..rest]` together cover every length. `Map[K, V]` has no positional pattern; match it as a whole and use `.get()` or iteration in the arm. `String` has no slice patterns.

**`Never`.** `match x { }` on a scrutinee of type `Never` is exhaustive, because the empty matrix misses nothing. This is the only type for which an empty match is well formed.

**Distinct types.** `distinct type UserId = i64` is an opaque nominal wrapper with exactly its base type's constructor space. Two `UserId(5)` patterns are the same literal, `UserId(0)` and `UserId(1)` are different literals, and a wildcard is needed to cover every `UserId`. Distinct types stop values crossing between types in the type checker; they are transparent to the pattern algorithm.

**`shared struct` and `shared enum`** follow the same rules as their plain counterparts: the handle is transparent to matching. A shared scrutinee counts as a `ref` scrutinee for binding modes, so a binding of a part is a `ref` and a binding of the whole copies the handle ([§7 Binding modes in patterns](#binding-modes-in-patterns)).

#### Or-patterns and guards

**Or-patterns do not multiply rows.** An arm `P1 | P2 | P3 => body` is three matrix rows `[P1; i]`, `[P2; i]`, `[P3; i]` that share body `i`. Specialization walks through or-patterns one alternative at a time (step 4 above) and caches intermediate results, so `Foo(A | B | C)` deep in the matrix does not produce `3^d` rows.

**Guards do not count toward exhaustiveness.** The algorithm treats a guarded arm `P if G => body` as if `G` were never true, so it can never be the arm that covers a remaining value. A match is exhaustive iff its unguarded arms are.

**Even a trivially true guard does not count.** `if true` and `if 1 == 1` do not satisfy exhaustiveness either: guards are not evaluated for this check. Accepting "obviously true" guards would create a cliff between the guards a prover handles and guards one character away from them, and the workaround is trivial: write an unguarded `_` arm.

**A guard on an or-pattern runs once per match.** In `P1 | P2 if G => body`, the or-pattern decides which alternative matched and `G` runs once against the bindings. The alternatives bind the same names with the same types, so the guard always sees one well-typed set of bindings. Neither compile time nor run time grows with the number of alternatives.

#### `let`, `if let`, `while let`

**`let PAT = expr` needs an irrefutable pattern**: the one-row matrix `[PAT]` must cover every value of the type, that is `U([PAT], _)` is false. A refutable pattern is rejected with a diagnostic suggesting `if let` or `let…else`.

**`if let` and `while let` are not checked for exhaustiveness.** They test one pattern, which is expected to be refutable. An irrefutable pattern there is a warning (the `else` branch is unreachable, or the loop never ends by a failed match), not an error.

**`let…else`** takes a refutable pattern. Its `else` block must diverge, which the type checker verifies by giving the block type `Never`. No exhaustiveness check is needed, because the `else` block handles every other value.

#### Empty and unreachable arms

**An empty match on a type other than `Never` is non-exhaustive.** `match x { }` with `x: T` and `T ≠ Never` is rejected with the full-type-uncovered diagnostic. Add arms that cover `T`, or a wildcard arm that calls `unreachable()`.

**An unreachable arm is a warning, not an error.** The reachability judgment finds an arm whose pattern earlier arms already cover. The arm is kept in the generated code rather than silently dropped, so changing an earlier arm later cannot bring unrelated code back to life.

#### Worked example

```kara
enum Event {
    MouseDown { x: i64, y: i64 },
    MouseUp   { x: i64, y: i64 },
    KeyDown   { code: i64 },
    Tick,
}

match evt {
    MouseDown { x, y } | MouseUp { x, y } => handle_mouse(x, y),
    KeyDown { code }                      => handle_key(code),
    Tick                                  => tick(),
}
```

The exhaustiveness check runs as follows:

1. Start with the three rows `R0`, `R1`, `R2` and the candidate `q = _`.
2. `q` is a wildcard, so check whether every constructor of `Event` heads some row. All four do: `R0` expands through its or-pattern to cover `MouseDown` and `MouseUp`, `R1` covers `KeyDown`, and `R2` covers `Tick`.
3. Recurse into each constructor's specialized matrix. For `MouseDown` it has one row `[_, _]` (the `{ x, y }` bindings) and the candidate is `[_, _]`. That row covers every pair of `i64`s, so `MouseDown` is exhausted, and likewise `MouseUp`, `KeyDown` and `Tick`.
4. No branch is useful, so `U(P, _)` is false: the match is exhaustive.

Without the `Tick` arm, step 2 finds `Tick` uncovered and `U(P, _)` is true with the witness `Tick`; the compiler reports `non-exhaustive match: missing Tick`. With a duplicate `MouseDown { x, y } => …` arm after `R0`, the reachability check computes `U([R0], duplicate)`, finds `MouseDown` already covered, and warns that the arm is unreachable.

---

## 6. Expressions and statements

Kāra is expression-oriented: blocks, `if`, `match` and `loop` produce values. Operands and arguments are evaluated left to right ([core-semantics.md §8](core-semantics.md#8-evaluation-order)), and every value is destroyed at the end of its scope ([core-semantics.md §7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).

### Statements and blocks

A block is a sequence of statements optionally followed by a final expression, the block's value. A block with no final expression has type `()`.

```kara
fn example() -> i64 {
    let a = 1;          // statement
    let b = 2;          // statement
    a + b               // the block's value: no `;`
}
```

The statements are `let`, `let…else`, assignment, `defer`, `errdefer` and expression statements. `return`, `break` and `continue` are expressions of type `Never`, so `return x;` is an expression statement.

**`;` is required after every expression statement that does not end in a block.** The block-like expressions need none: `if`, `match`, `while`, `for`, `loop`, a plain or labeled block, `unsafe { }`, `par { }` and `par for`. A missing `;` anywhere else is an error; the parser never inserts one.

**A block-like expression in statement position drops its value** at the end of the statement. The `must_use` lint applies as usual ([§11](#must_use)), so a discarded `par { … }` whose value is a tuple of `Result`s warns.

**A block-like expression in statement position ends at its `}`.** The next token starts a new statement, even one that could otherwise apply to it (`.`, `(`, `[`, `?`), so a `while` loop followed by a line starting `[1, 2]` is two statements. To apply an operator to a block-like expression in statement position, parenthesize it: `(if c { a } else { b }).len()`.

**Other separators.** Match arms are separated by `,`, which is optional after an arm whose body is a block. Fields and parameters are separated by `,`, and a trailing comma is allowed. Top-level items have no separator.

### Variable binding rules

**Immutable by default.** `let` declares an immutable binding. Reassigning it, calling a `mut ref self` method on it, or passing it as a `mut` argument ([§7](#7-functions-and-closures)) is a compile error. `let mut` opts in to mutability:

```kara
let x = 5;
x = 10;              // ERROR: x is immutable

let mut y = 5;
y = 10;              // OK

let seen = Map.new();
seen.insert(k, v);   // ERROR: insert takes mut ref self, and seen is immutable

let mut seen = Map.new();
seen.insert(k, v);   // OK
```

**`let mut` with a destructuring pattern** makes every binding the pattern introduces mutable. There is no per-binding `mut` inside a pattern:

```kara
let mut (a, b) = (1, 2);                    // a and b are both mutable
let mut Point { x, y } = p;                 // x and y are both mutable
let mut Ok(v) = result else { return; };    // v is mutable
```

When only some bindings need to be mutable, shadow them after destructuring:

```kara
let (a, b) = (1, 2);   // both immutable
let mut a = a;         // shadow a as mutable; b stays immutable
```

**Shadowing** (`let x = …` again) is always allowed, whatever the old binding's mutability. It introduces a new binding; it does not mutate the old one, and the old value stays alive until its scope ends ([core-semantics.md §7.3](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)). Shadowing works across scopes too:

```kara
let x = "42";
let x = x.len();          // OK: shadows with a different type

let y = 10;
if condition {
    let y = 20;           // OK: shadows the outer y
}
// y is still 10 here
```

`let mut` does not affect move and borrow analysis, which is independent of local mutability.

**Explicit initialization is required.** A variable must be assigned before it is used. There are no zero values and no default initialization:

```kara
let x: i64;          // declared, not yet usable
print(x);            // ERROR: x is not initialized
x = 5;
print(x);            // OK
```

**The first assignment initializes; it does not reassign.** The first assignment to a `let x: T;` binding needs no `mut`; later assignments do. This applies only to the form without an initializer.

```kara
let x: i64;
x = 5;     // OK: initialization, no `mut` needed
x = 10;    // ERROR: x is immutable; write `let mut x: i64;`

let mut y: i64;
y = 5;     // OK: initialization
y = 10;    // OK: reassignment, allowed by `mut`
```

**Definite assignment is flow-sensitive.** The compiler tracks whether each such binding is definitely assigned on every path that reads it:

- **In sequence**, reading `x` before any assignment is an error.
- **Branches that all assign**: if every branch of an `if` assigns `x`, `x` is assigned after it. `if cond { x = 1; } else { x = 2; }` assigns `x`.
- **Branches that do not all assign**: if only some branches assign `x`, it is possibly uninitialized afterwards, and using it is an error.
- **`while` and `for`**: the body may run zero times, so assignments inside it do not count after the loop, even if the loop always runs in practice.
- **`loop`**: a body that assigns `x` on every path before every `break` makes `x` assigned after the loop, because the body runs at least once.
- **Struct fields**: assigning fields one by one (`p.x = 1; p.y = 2;`) does not assign the struct as a whole; use a struct literal. The same holds for the slots of an [array](#arrays).

```kara
let x: i64;
if condition { x = 5; }
print(x);              // ERROR: x is possibly uninitialized (condition may be false)

let y: i64;
if cond_a { y = 1; } else { y = 2; }
print(y);              // OK: both branches assign y

let z: i64;
for _ in 0..5 { z = 42; }
print(z);              // ERROR: the loop may run zero times
```

**A loop variable is a fresh binding in each iteration.** A closure created in the loop captures that iteration's value, not one shared variable:

```kara
let mut callbacks = Vec.new();
for i in 0..3 {
    callbacks.push(|| { print(i); });
}
for f in callbacks {
    f();
}
// prints 0, 1, 2 (not 2, 2, 2)
```

### Assignment

`p = v` stores `v` into the place `p`. `p op= v` is `p = p op v` with the place evaluated once.

**Places.** The target of an assignment must be a *place expression*:

```
PLACE_EXPR = VALUE_IDENT                              // a `let mut` binding
           | PLACE_EXPR "." IDENT                     // a field or tuple position
           | PLACE_EXPR "[" EXPR { "," EXPR } "]"     // an index
           | "*" EXPR                                 // a dereference of a `mut ref T`
           | "(" PLACE_EXPR ")"
```

Call results and literals are not places; they may appear on the right of `=`, never on the left. Nothing is written through a `ref`: a place reached through a `ref` is read-only ([core-semantics.md §5.9](core-semantics.md#5-references-and-views-c5)). Assigning through a `mut ref` needs an explicit `*`: `*counter = 0;`.

**Order.** The operands of the place are evaluated once, left to right, then the value; then the old value is dropped and the new one stored ([core-semantics.md §7.5](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0), [§8.2](core-semantics.md#8-evaluation-order)).

**Compound assignment.** The operators are `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=` and `>>=`. Each needs a mutable target, and its result type follows [Mixed-width operands](#mixed-width-operands).

**Parallel assignment.** Several places can be assigned several values in one statement:

```
MULTI_ASSIGN = PLACE_EXPR { "," PLACE_EXPR } "=" EXPR { "," EXPR } ";"
```

```kara
a, b = b, a;                       // swap two locals
v[i], v[j] = v[j], v[i];           // swap two slots of a Vec of a Copy type
x, y, z = z, x, y;                 // rotate three locals
```

Every value on the right is evaluated, left to right, before any target is written; that is what makes `a, b = b, a` a swap. Both sides must list the same number of items (a mismatch is a parse error), and each target follows the place rules above. This is the in-place idiom that swap-based sorts and permutation generators rely on.

Assignment is a statement, not an expression: it has no value. It may also be the body of a `match` arm ([Pattern matching](#pattern-matching)).

### Conditionals

`if` is an expression. It produces a value and can appear wherever an expression can:

```kara
let max = if a > b { a } else { b };
```

**Conditions must be `bool`.** There is no truthiness: integers, `Option` and other types do not convert to `bool`. Write the predicate: `x != 0`, `x.is_some()`, `not items.is_empty()`. Conversions are always explicit.

**`if` without `else` has type `()`.** Its block must then produce `()` too. A block that produces a value with no `else` branch is a type error, even in statement position, so a value is never silently discarded:

```kara
if cond { side_effect(); }           // OK: the block produces ()
if cond { 5 }                        // TYPE ERROR: the block produces i64, and there is no else
let x = if cond { 5 };               // TYPE ERROR: same reason
let y = if a > b { a } else { b };   // OK: both branches produce i64
```

**No postfix conditional.** `val if cond else other` (Python style) does not exist. The prefix form is enough and avoids a precedence question.

**A struct literal in a condition must be parenthesized**, as in Rust. In the condition of `if`, `while`, `if let` and `while let`, and in the collection of a `for`, a `{` after a name starts the body: `if p == (Point { x: 0, y: 0 }) { … }`.

### `if let` and `let…else`

**`if let`** runs a block when a pattern matches, with the pattern's bindings in scope inside that block only:

```kara
if let Some(u) = user.find(id) {
    process(u);
}

if let Some(u) = user.find(id) {
    process(u);
} else {
    log("not found");
}
```

It is the one-shot counterpart of `while let`, and saves a full `match` for a single pattern. `else if let` is valid, because any `if` may follow `else`:

```kara
if let Some(a) = try_a() {
    observe(a);
} else if let Some(b) = try_b() {
    observe(b);
} else {
    fallback();
}
```

Or-patterns work as in `match` ([Or-patterns](#or-patterns)):

```kara
if let Left(x) | Right(x) = val {
    observe(x);
}
```

**`let…else`** destructures at the top of a scope and exits early when the pattern does not match:

```kara
let Ok(config) = load_config(path) else {
    return Err(ConfigError.Missing);
};
// config is in scope here
```

- The `else` block **must diverge**: `return`, `break`, `continue`, or a call to a diverging function (`panic()`, `unreachable()`). The type checker enforces this.
- The pattern's bindings are in scope **after** the statement, not inside the `else` block.
- `let…else` is a statement, not an expression; it has no value.

`let…else` is the idiomatic guard at the top of a function. It keeps the main path unindented.

**Scrutinee temporaries.** Temporaries created while evaluating the expression after `=` drop where [core-semantics.md §7.4](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0) says:
- an `if let` scrutinee lives to the end of the whole construct, `else` branch included. To release a lock or a lease before the `else` branch runs, bind the needed value with `let` first;
- a `while let` scrutinee lives to the end of each iteration's body;
- a `let…else` initializer's temporaries drop before the `else` block runs, or at the `;` when the pattern matches.

`if let` chains (several `and`-joined `let` tests in one condition) are deferred ([deferred.md](deferred.md)).

### Loops and labels

There are four loop forms:

- **`while condition { … }`** runs while a condition holds.
- **`while let PATTERN = expr { … }`** runs while a pattern matches, for example `while let Some(job) = queue.pop() { … }`.
- **`for PATTERN in e { … }`** iterates. The pattern may destructure (`for (key, value) in map`). When `e` is an `Iterator`, the loop consumes it, so `for x in c.into_iter()` moves `c` and each item. Otherwise the loop borrows `e` through `Iterable`, and `x` is the iterator's item, `ref T` for the standard collections ([core-semantics.md §4.6](core-semantics.md#4-parameters-calls-and-patterns)). To loop over an iterator without consuming it, write `for x in it.by_ref()`. The iteration traits are in [§9](#iterator-and-iterable).
- **`loop { … }`** runs until a `break`.

Each iteration is its own scope: what it creates drops at the end of the iteration, and `continue` ends it the same way ([core-semantics.md §7.2](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).

**Ranges.** The range operators `..` and `..=` build values of six library types, depending on which ends are given:

| Syntax | Type | Meaning | Iterable? |
|---|---|---|---|
| `lo..hi` | `Range[T]` | exclusive end, `[lo, hi)` | Yes |
| `lo..=hi` | `RangeInclusive[T]` | inclusive end, `[lo, hi]` | Yes |
| `lo..` | `RangeFrom[T]` | no upper end, `[lo, ∞)` | Yes; the loop must end by `break`, `take` or the like |
| `..hi` | `RangeTo[T]` | exclusive end only, `(-∞, hi)` | No |
| `..=hi` | `RangeToInclusive[T]` | inclusive end only, `(-∞, hi]` | No |
| `..` | `RangeFull` | no ends | No |

Only the forms with a start can be iterated. `RangeTo`, `RangeToInclusive` and `RangeFull` appear mostly as slice indices (`items[..n]`, `items[..=n]`, `items[..]`) and as [range patterns](#range-patterns). All six implement a common `SliceIndex[T]` contract, so any sequence that supports `items[Range]` supports the half-open forms with no extra surface.

```kara
for i in 0..10 { print(i); }       // 0, 1, …, 9
for i in 0..=10 { print(i); }      // 0, 1, …, 10
let middle = items[2..5];
let tail = items[2..];
let head = items[..n];
```

Prefer `..=` to `lo..(hi + 1)` when the end must be visited, and the half-open forms (`items[2..]`, `items[..n]`) to `items[2..items.len()]` or `items[0..n]`. Both remove an off-by-one risk from the expression.

**`loop` is an expression** when it exits with a value:

```kara
let result = loop {
    let attempt = try_something();
    if attempt.is_ok() {
        break attempt;   // the loop's value
    }
};
```

**Labels.** Any loop may carry a label, written with a leading `'`, so that `break` and `continue` can target it from inside nested loops:

```kara
'outer: for row in matrix {
    for val in row {
        if *val == target {
            break 'outer;       // leaves the outer loop
        }
    }
}
```

`break` leaves the innermost loop and `continue` starts its next iteration; `break 'label` and `continue 'label` target the named loop instead, and `break 'label value` leaves it with a value. The sigil keeps a label apart from a value: `break name` always means a value named `name`, never a label.

**Labeled blocks.** Any block may carry a label, which makes it an early exit with a value:

```kara
let result = 'found: {
    for row in matrix {
        for cell in row {
            if *cell == target {
                break 'found *cell;     // leaves the block with this value
            }
        }
    }
    -1                                  // the block's value when nothing broke out
};
```

A labeled block is an ordinary block with one extra ability: code inside it may write `break 'label value` (or a bare `break 'label`) to leave it early. Without a label there would be no way to name a block as a `break` target.

- The block's type is the least upper bound ([§8](#8-type-inference-and-generics)) of every reachable `break 'label value` and the final expression, the same rule as for `loop` and for the branches of an `if`. A block whose final expression is `()` and whose only breaks are bare has type `()`.
- A label that is never targeted is allowed; the block is then a plain block.
- `continue 'label` is **rejected** when `'label` names a block, because there is nothing to continue: `continue label refers to a labeled block; continue is only valid for loops`, with a fix pointing at the label.

**Label scope.** A label is in scope only in the body of its loop or block, and it shadows an enclosing label of the same name. An unknown label in `break` or `continue` is a compile error. A `break` inside a closure cannot target a label outside the closure, because the closure is not part of that control flow.

**`break value` is valid only in `loop` and in labeled blocks.** `while` and `for` loops always have type `()`. A `break` with a non-`()` value that targets a `while` or `for` loop, directly or by label, is a compile error. A plain `break` works in every loop.

**The type of a `loop`** comes from its `break`s:
- **No reachable `break`**: the type is `Never`. The loop runs forever or leaves only by `return` or a panic.
- **One or more `break value`**: the type is the least upper bound of the break values; values whose types do not unify are an error.
- **A conditional `break`** still contributes its type. The loop may never reach it at run time, but its type is the break value's type: the compiler does not infer `Option` or `Never` because a `break` is guarded.

```kara
// A conditional break: the type is i32, not Option[i32]
let x = loop {
    if ready { break 5i32; }
};

// Two breaks of the same type: OK
let y = loop {
    if cond_a { break 1i32; }
    if cond_b { break 2i32; }
};

// Two breaks of different types: compile error
let z = loop {
    if cond_a { break 5i32; }
    if cond_b { break "hello"; }  // ERROR: break type mismatch (i32 vs String)
};
```

### Operators and precedence

From loosest to tightest:

| Level | Operators | Associativity |
|---|---|---|
| 1 (loosest) | default `??` | Left |
| 2 | range `..` `..=` | None |
| 3 | logical `or` | Left |
| 4 | logical `and` | Left |
| 5 | comparison `==` `!=` `<` `<=` `>` `>=` | Left (no chaining) |
| 6 | bitwise or `\|` | Left |
| 7 | bitwise xor `^` | Left |
| 8 | bitwise and `&` | Left |
| 9 | shift `<<` `>>` | Left |
| 10 | additive `+` `-` | Left |
| 11 | multiplicative `*` `/` `%` | Left |
| 12 | cast `as` | Left |
| 13 | unary `not` `-` `~` `*` | Prefix |
| 14 | error propagation `?` | Postfix |
| 15 (tightest) | field access, call and index: `.` `()` `[]` | Left |

So `a + b as f64` is `a + (b as f64)`, `-x as u8` is `(-x) as u8`, and `x as u32 as u8` casts twice. Ranges do not chain: `a..b..c` is an error.

**Logical operators are words.** `and`, `or` and `not` are the logical operators; `and` and `or` evaluate their right side only when needed ([core-semantics.md §8.3](core-semantics.md#8-evaluation-order)). The symbols `&&`, `||` and `!` are rejected with a fix that substitutes the word. `~` is bitwise not, and prefix `*` dereferences a `ref T` or `mut ref T`.

**`not` binds tighter than comparison**, so `not x == y` is `(not x) == y`, as with `!` in C-family languages. Parenthesize for `not (x == y)`; the `ambiguous_not_comparison` lint warns about `not` directly before a comparison.

**No automatic dereference in operators.** Field access and method calls look through `ref T` and `mut ref T`, so `r.field` needs no `*`. Operators do not: comparing `a: T` with `b: ref T` is a type error; write `a == *b`. Use `*` for comparisons, arithmetic and assignment through a reference (`*r = v`).

**Operators are trait calls.** Each operator calls the method of its operator trait for the operand types ([§9](#9-traits)); on numeric primitives the rules of [Numeric semantics](#numeric-semantics) apply.

**Multi-index.** `e[i, j, k]` is sugar for `e[(i, j, k)]`: the indices form a tuple, and `Index` or `IndexMut` is called with it. A matrix type that implements `Index[(i64, i64)]` is indexed as `m[r, c]`.

### Temporaries

A temporary is a value an expression creates without binding it to a name: an unbound call result, an intermediate `Option`, the lease returned by `pool.acquire()`. Where each one is dropped is fixed by its position, in the table of [core-semantics.md §7.4](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0). Several temporaries that drop at the same point drop in reverse order of creation.

There is no temporary lifetime extension ([core-semantics.md §5.5](core-semantics.md#5-references-and-views-c5)). A reference or view into a temporary cannot outlive the statement that created it, so `let r = ref make_value();` is an error. Bind the value first: `let v = make_value(); let r = ref v;`.

A fixed table, rather than "the longest reasonable scope", lets a reader (human or model) look up where a temporary drops instead of reasoning about implicitly extended scopes.

### `?` and `??`

**`e?`** unwraps an `Ok` or `Some`, and on `Err` or `None` returns from the enclosing function. Its rules, including the `From` conversion of the error, are in [§10 Results and propagation](#results-and-propagation).

**`a ?? b`** supplies a default. It gives the success value inside `a` when `a` is `Some` or `Ok`, and `b` otherwise. The error in an `Err` is discarded; to inspect it, use `match` or `.unwrap_or_else(|e| ...)`.
- `b` is evaluated only when `a` is `None` or `Err` ([core-semantics.md §8.3](core-semantics.md#8-evaluation-order)).
- `b` must have the type of the value inside `a`, and so does the result. One exception: when `a` is an `Option[ref V]` and `V: Copy`, `b` may be a `V`, and the result is a `V`. So `counts.get(word) ?? 0` works. For other `V`, write `m.get(k).cloned() ?? d`.
- `??` has the loosest precedence of all operators.

```kara
let port = config.port ?? 8080;     // config.port: Option[i64]
```

### `defer` and `errdefer`

`defer` schedules cleanup for the end of the current scope; `errdefer` schedules cleanup for an error exit only. Both are statements:

```
DEFER_STATEMENT    = "defer" ( EXPR ";" | BLOCK )
ERRDEFER_STATEMENT = "errdefer" ( EXPR ";" | BLOCK )
                   | "errdefer" "(" IDENT ")" BLOCK      // binds the error value by `ref`
```

When they run, what `errdefer(e)` binds, and what a cleanup block may contain are in [§10 Cleanup with `defer` and `errdefer`](#cleanup-with-defer-and-errdefer).

---

## 7. Functions and closures

This section covers the surface of functions: how they are declared, how parameters and returns are written, and how function values and closures work. The rules for what a call moves, borrows and drops belong to [core-semantics.md](core-semantics.md); this section states each rule briefly and links it.

### Declarations

A function declaration has this shape. Every part after the parameter list is optional.

```kara
pub fn name[T: Bound](param: Type, other: own Other) -> Return
    with reads(Db)
    where T: Display
{
    body
}
```

- **Visibility.** `pub` exports the function from its package; see [§4 Visibility](#visibility).
- **Generic parameters** go in `[...]` after the name; see [§8 Generic parameters and arguments](#generic-parameters-and-arguments).
- **Return type.** A missing `-> T` means the function returns `()`.
- **Effect clause.** `with ...` comes after the return type. A trailing `with` there is always the function's own clause ([Function types and kinds](#function-types-and-kinds)). Private functions may omit it, because their effects are inferred. See [§12 Effects](#12-effects).
- **`where` clause** comes after the effect clause; see [§8 `where` clauses](#where-clauses).
- **Body.** The body is a block. Its tail expression is the result; `return e;` returns early ([§6 Expressions and statements](#6-expressions-and-statements)).
- **Every parameter has a name or a pattern.** The type-only form `fn f(i32)` is a parse error. Write `_: i32` for a parameter the body does not use.
- **Methods** are functions declared in an `impl` block. Their first parameter may be a receiver (`self`, `own self` or `mut ref self`); see [§9 Traits](#9-traits).

```kara
fn add(a: i64, b: i64) -> i64 { a + b }

pub fn save(user: User) -> Result[(), DbError] with writes(UserDB) {
    db_insert(user)
}
```

### Parameter modes

Every parameter has one declared mode, written in its type. Modes are never inferred from the body. A bare parameter borrows; `own` makes it owned.

| Form | Mode | What the callee may do |
|---|---|---|
| `x: T` | borrowed (a copy if `T` is `Copy`) | Read it. For a non-`Copy` `T`, it may not write through it, move it, or store or return it as an owned value; a handle is counted instead (below). |
| `x: own T` | owned | Anything: it belongs to the callee. The argument moves into the callee, or is copied if `T` is `Copy`. The callee drops it at its scope end unless it moves it on. |
| `x: mut ref T` | exclusive borrow | Read and write through it. It may not move out of it. |
| `x: Slice[T]`, `x: Str` | shared view | Read the elements or the text. A `Vec[T]` argument passes as `Slice[T]`, and a `String` passes as `Str`. |
| `x: mut Slice[T]` | exclusive view | Read and write the elements. |

Receivers follow the same rule: `self` is borrowed, `own self` is owned (consuming), and `mut ref self` is an exclusive borrow. Methods of a `shared` type take `self` only.

- **`ref T` is a redundant spelling.** In a parameter, `x: ref T` means the same as `x: T`. It compiles with a warning, and `karac fix` removes the `ref`. `ref` is still written in return types, fields and locals ([Returns, `ref` returns and views](#returns-ref-returns-and-views)).
- **A reference passes to a borrowed parameter.** A bare `x: T` accepts a `T`, a `ref T` or a `mut ref T`, so a function holding `r: ref Config` calls `show(r)` directly ([core-semantics.md §4.1](core-semantics.md#4-parameters-calls-and-patterns)).
- **An `escaping` parameter is owned**, and `own` is not written beside it ([Escaping function values](#escaping-function-values)).
- **Function types use the same forms.** `Fn(own T) -> R` takes an owned argument; a bare argument type borrows ([Function types and kinds](#function-types-and-kinds)).
- **`frozen T`** has its own section ([§11 Ownership and sharing](#11-ownership-and-sharing)).

**A bare parameter is read-only.** For a non-`Copy` type it borrows the caller's value, and the caller keeps it. The callee may read it, pass it to another parameter that borrows, and return or store a reference to it under the view rules ([Returns, `ref` returns and views](#returns-ref-returns-and-views)). Nothing is written through it ([core-semantics.md §5.9](core-semantics.md#5-references-and-views-c5)), and it cannot be moved. Storing it as an owned value (in a field or a collection), passing it to an `own` parameter, or returning it as an owned result is an error in the callee, at that line, and the diagnostic names the declaration. The fix is to declare the parameter `own T`, or to `.clone()` it at that line.

```kara
fn greet(name: String) -> String {
    f"hello, {name}"                    // only reads `name`: bare is enough
}

fn remember(names: mut ref Vec[String], name: own String) {
    names.push(name);                   // stores `name`, so it is `own`
}
```

**Handles are counted, not moved.** A `shared` or `sync` handle passed to a bare parameter is borrowed and not counted. Using it as a value from there (storing it, returning it, passing it to an `own` parameter) counts it, so a bare handle parameter may be stored ([core-semantics.md §6.1](core-semantics.md#6-sharing-c7)).

**Why borrowing is the default.** In the real-world programs of `corpus/apps`, 70% of non-`Copy` parameters are only read and 23% are stored, so the common case needs no word. A missing `own` is a compile error in the callee, at one line, rather than a `.clone()` at every call site that keeps its value. Swift and Hylo also borrow parameters by default.

The semantics are in core-semantics:
- the parameter forms: [§4.1](core-semantics.md#4-parameters-calls-and-patterns);
- what moves and when: [§3](core-semantics.md#3-moves);
- owned parameters belong to the callee, which drops them: [§4.2](core-semantics.md#4-parameters-calls-and-patterns);
- nothing is written through a borrowed parameter or a `ref`: [§5.9](core-semantics.md#5-references-and-views-c5);
- moving out of a borrowed or `mut ref` place is an error: [§3.7](core-semantics.md#3-moves).

**Editing a body never changes a signature.** The body is checked against the declared mode, never the reverse, so a function's contract with its callers is stable.

**An impl may declare weaker modes than its trait.** An impl method may take a parameter, the receiver included, with the trait's mode or a weaker one, in the order `own`, `mut ref`, borrowed. See [core-semantics.md §4.7](core-semantics.md#4-parameters-calls-and-patterns) and [§9 Traits](#declarations-and-impls).

**Match guards only read.** A `match` guard may not move a place. A guard can fail, and the next arm still needs the value.

**`karac explain` reports the would-be mode.** For each parameter it reports the weakest mode the body needs, so a parameter declared `own` but only read can drop the `own`. This is a diagnostic aid. It never changes the signature.

### Binding modes in patterns

The mode of each binding in a `match`, `if let`, `while let`, `let` or `for` pattern comes from the pattern and the scrutinee's type, never from how the arm uses it. [core-semantics.md §4.6](core-semantics.md#4-parameters-calls-and-patterns) is normative. In short:

- Over an **owned** scrutinee (an owned local, an `own` parameter or a temporary), a plain binding of a non-`Copy` part moves that part out. `ref name` borrows it instead, and `mut ref name` borrows it mutably.
- Over a **borrowed** scrutinee (a bare parameter of a non-`Copy` type, a `ref` or a view), a binding of a non-`Copy` part is a `ref` into it, and a binding of a `Copy` part copies it.
- Over a **`shared`** value, a plain binding of a part is a `ref`; moving it is an error whose fix is `.clone()`.
- `_` and `..` never bind and never move. A pattern that binds nothing by value leaves the scrutinee intact.
- A pattern over a type with a `Drop` body may bind the whole value but may not move a part out.
- A **slice pattern** over a `Vec` or a `Slice` sees a view: its element bindings are `ref`, or `mut ref` against a mutable place, and nothing moves out. `..rest` binds a `Slice[T]`, or a `mut Slice[T]` when the binding is written `mut ref rest` against a mutable place. A pattern over an owned `Array[T, N]` destructures like a tuple and may move elements. The forms are in [§5 Slice and array patterns](#slice-and-array-patterns).

```kara
fn f(val: own Foo) {
    match val {
        Foo { field, .. } => use_owned(field),   // moves `field` out of `val`
    }
    // `val` is partially moved: it cannot be used as a whole
}

fn g(val: Foo) {
    match val {
        Foo { field, .. } => use_ref(field),     // `field: ref FieldType`
    }
    // `val` is still usable
}

fn h(val: own Foo) {
    match val {
        Foo { ref name, score, .. } => { log(name); rank(score); }   // `name` borrowed, `score` moved
    }
}
```

A read-only arm followed by a later use of an owned scrutinee is a use after move. `karac fix` adds the `ref`.

### Call-site `mut` markers

Signatures declare the mode; call sites mark mutation. The marker makes every mutation visible at both ends of a call.

**Rule.** Every argument passed to a `mut ref T` or `mut Slice[T]` parameter is written with a `mut` prefix, whatever the argument's root: a local, a field or element of a `mut ref` binding, a temporary, or the result of a call.

```kara
fn sort_in_place(xs: mut Slice[i64]) { ... }
fn reset(s: mut ref State) { ... }

let mut v = [3, 1, 4, 1, 5];
sort_in_place(mut v);

fn refresh(s: mut ref Session) {
    sort_in_place(mut s.scores);    // a field of a `mut ref` binding: still marked
    reset(mut s.state);
}
```

- **A missing marker is an error**, with a fix-it that adds it: "function `sort_in_place` takes `xs: mut Slice[i64]`; write `sort_in_place(mut v)`".
- **A marker on any other argument is an error.** `mut` is legal only where the parameter is `mut ref T` or `mut Slice[T]`.
- **The marker does not grant mutability.** The place must already be writable: rooted at a `let mut` binding, at a `mut ref` binding, or at a `mut` field of a `shared` value. `let v = ...; sort_in_place(mut v);` is the same error as assigning to `v` ([§6 Variable binding rules](#variable-binding-rules)). A place reached through a bare parameter or a `ref` cannot be marked ([core-semantics.md §5.9](core-semantics.md#5-references-and-views-c5)).
- **`ref` is never written at a call site.** A bare parameter accepts `f(v)` unmarked, and so does an `own` parameter, which moves `v`. `f(ref v)` and `f(mut ref v)` are parse errors that point at the callee's signature.
- **Method receivers are not arguments.** `v.push(x)`, `s.f = 5` and `v[i] = x` carry no marker; the receiver or the assigned place is already visible. A UFCS call is a free-function call, so its receiver argument is marked: `Counter.bump(mut c)` and `c.bump()` are the same call.
- **Calling a function value carries no marker.** A `MutFn` closure that mutates its captures is called as `f()`, from a `let mut` binding ([Function types and kinds](#function-types-and-kinds)); the mutation was announced where the closure captured the place (see [Captures](#captures)).

Because the rule is uniform, checking a marker needs only the callee's signature.

### Named and default parameters

A parameter list may end with **named parameters**, after a `;`. Named parameters are always passed with their label. Only named parameters may have default values.

```kara
fn connect(host: String; port: i64 = 443, timeout_ms: i64 = 5000) -> Connection { ... }

connect("db");                                  // port 443, timeout 5000
connect("db", timeout_ms: 1000);                // port 443
connect("db", timeout_ms: 1000, port: 8443);    // named arguments in any order
```

**Rules.**
- **Positional parameters** come before the `;`. They are passed by position and are never labeled. `connect(host: "db")` is an error.
- **Named parameters** come after the `;`. Each is passed as `label: value`, where the label is the parameter's name. There is no separate external label.
- **Order.** Named arguments come after all positional arguments, in any order. They are evaluated in the order they are written at the call site ([core-semantics.md §8.1](core-semantics.md#8-evaluation-order)).
- **Defaults.** A named parameter may have a default, written `= expr` after its type. A named parameter without a default is required, and omitting it is an error that names it.
- **Defaults are constant expressions**: whatever a `const` initializer allows ([§4 Constants](#constants)). A function name is one; a closure literal is not. A default may not refer to another parameter. Each call that omits the argument gets a fresh value of the default.
- **Labels must match.** A label that names no named parameter, or names one twice, is an error.
- **Receivers are positional.** In a method, `self` is the first positional parameter, so `fn get(self, key: K; fallback: i64 = 0)` is called as `m.get(k, fallback: 7)`.
- **As a value**, a function with named parameters takes only its positional parameters ([Function values](#function-values)).

`Option[T] = None` is the idiom for an optional setting whose absence means something different from any value:

```kara
fn find[T: PartialEq](items: Slice[T], target: T; start: Option[i64] = None) -> Option[i64] { ... }
```

**Why labels are opted into at the declaration.** A named parameter's name is part of the function's public contract, so its author chooses it. Positional names stay private to the function: renaming one never breaks a caller. Real APIs (HTTP clients, CSV readers, server configuration) have many options, and named arguments stop callers, people and models alike, from mis-ordering arguments of the same type.

### Destructuring parameters

Any irrefutable pattern may stand in parameter position, followed by `:` and the type, in functions and closures alike:

```kara
fn add((a, b): (i64, i64)) -> i64 { a + b }

fn distance(Point { x: x1, y: y1 }: Point, Point { x: x2, y: y2 }: Point) -> f64 {
    ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
}

fn y_only((_, y): (i64, i64)) -> i64 { y }

let sums = pairs.into_iter().map(|(a, b)| a + b);
```

**Rules.**
- **Irrefutable patterns only.** `Some(x)` and enum variants are not allowed in parameter position. Use `if let` or `match` in the body.
- **The type carries the parameter's mode**, as for any parameter. Bare `T` borrows, `own T` is owned, and `mut ref T` borrows mutably.
- **Bindings follow the pattern rules** of [Binding modes in patterns](#binding-modes-in-patterns). Over an `own` parameter, a plain binding moves its part and `ref name` borrows it. Over a bare parameter of a non-`Copy` type, a binding of a non-`Copy` part is a `ref`, and a binding of a `Copy` part copies it. So a pattern that moves parts out of a parameter needs `own`.

```kara
struct Request { headers: Headers, body: Vec[u8] }

fn handle(Request { headers, body }: own Request) {
    log(headers);     // `log` borrows `headers`, so it stays usable
    process(body);    // `process` takes `own Vec[u8]`: moves `body` on
}

fn inspect(Request { headers, .. }: Request) {
    log(headers);     // `headers: ref Headers`
}
```

- **Unbound parts drop at the end of the function.** An `own` destructured parameter is a local of the callee ([core-semantics.md §4.2](core-semantics.md#4-parameters-calls-and-patterns)), and its pattern partially moves it. The parts no binding moves out, such as those under `_` or `..` or bound by `ref`, drop at the end of the function, as for any partially moved value ([core-semantics.md §7.9](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).
- **Effects are unaffected.** Effects belong to the function; patterns belong to parameters. The `:` after a struct pattern is unambiguous, because `{` there opens a pattern, not a block.

### Returns, `ref` returns and views

**Functions return owned values by default.** The result moves to the caller ([core-semantics.md §4.4](core-semantics.md#4-parameters-calls-and-patterns)). Most functions need nothing else:

```kara
fn longest(a: own String, b: own String) -> String {
    if a.len() > b.len() { a } else { b }      // returns a parameter as owned, so both are `own`
}
```

**A function may return a reference** when copying would cost too much. `ref T` and `mut ref T` may be the return type or appear inside it (`Option[ref T]`, a tuple):

```kara
fn name(user: User) -> ref String {
    ref user.name
}

fn longer(a: String, b: String) -> ref String {
    if a.len() > b.len() { a } else { b }
}

impl Inventory {
    fn first(self) -> Option[ref Item] {
        if self.items.is_empty() { None } else { Some(ref self.items[0]) }
    }
}
```

**The `ref` is written.** A function that returns a reference writes `ref` on the place it returns, as `name` does with `ref user.name`. The return type does not borrow for you: a tail `user.name` would move a non-`Copy` field out of a borrowed place ([core-semantics.md §3.7](core-semantics.md#3-moves)). A bare parameter, such as `a` or `b` in `longer`, is already a borrow and is returned as it is.

**The signature decides what the result borrows** ([core-semantics.md §5.4](core-semantics.md#5-references-and-views-c5)):
- with a `self` or `mut ref self` receiver, the result borrows from `self` only;
- otherwise, the result borrows from every bare parameter of a non-`Copy` type, every `mut ref` parameter and every view parameter;
- with no such parameter, returning a reference or a view is an error.

The body is checked against this rule. The compiler never traces a body to narrow it, so a function's borrow contract cannot change when its body changes. No lifetime annotations exist.

**Views.** A type that contains a `ref` after generic substitution is a *view*: `Slice[T]`, `Str`, an iterator over a borrowed collection, `Option[ref T]`, `Vec[ref T]`, a closure with a `ref` capture, and a struct with a `ref` field ([core-semantics.md §5.2](core-semantics.md#5-references-and-views-c5)). A view borrows exactly like a reference, and the signature rule above covers functions that return one.

```kara
struct Parser {
    source: ref String,    // a Parser cannot outlive the String it borrows
    position: i64,
}

fn make_parser(s: String) -> Parser {
    Parser { source: s, position: 0 }
}

struct Joiner { left: ref String, right: ref String }

fn make_joiner(a: String, b: String) -> Joiner {
    Joiner { left: a, right: b }    // borrows from both `a` and `b`
}
```

Rules for views, all in [core-semantics.md §5](core-semantics.md#5-references-and-views-c5):
- **Borrows root at named places.** A reference whose origin is a temporary must not outlive the statement that created it. There is no temporary lifetime extension. Bind the temporary to a variable first:
  ```kara
  let n = name(make_user());     // error: the result borrows a temporary
  let u = make_user();
  let n = name(u);               // OK: borrows `u`
  ```
- **Where a view may not go:** a field of a `shared`, `sync` or `frozen` type, a global, a channel, an escaping closure, or the function's result except as the signature rule allows.
- **`Vec[ref T]` is valid.** `v.push(r)` adds `r`'s origins to `v`'s ([§5.3](core-semantics.md#5-references-and-views-c5)). Usually an iterator of `ref T` items is what is wanted, not a stored `Vec[ref T]`.
- **Views in generic code** are checked on each monomorphised instance ([§5.8](core-semantics.md#5-references-and-views-c5)).

When borrowing outgrows views, the escape hatch is a `shared` type ([§11 Ownership and sharing](#11-ownership-and-sharing)), not annotations.

### `*` and `ref` in expressions

References are created implicitly when an argument is passed to a bare parameter of a non-`Copy` type. Two operators handle them in expressions.

**`*r` dereferences.** `*r` is the place a `ref T` or `mut ref T` points at.

```kara
fn contains[T: PartialEq](haystack: Slice[T], needle: T) -> bool {
    for x in haystack {
        if *x == needle { return true; }    // `x` is a `ref T`; `==` borrows both sides
    }
    false
}

fn bump(counter: mut ref i64) {
    *counter = counter + 1;      // the write needs `*`; the read does not, since `i64` is `Copy`
}
```

- **Auto-deref happens only at `.`**: field access, tuple positions and method calls. Binary operators, comparisons, arithmetic and indexing on the reference itself do not auto-deref, with one exception: a `ref T` or `mut ref T` whose `T` is `Copy` is read as a `T` wherever a `T` is expected, an operator operand included ([core-semantics.md §5.10](core-semantics.md#5-references-and-views-c5)). So `total += x` with `x: ref i64` needs no `*`. For a non-`Copy` `T`, `a == b` with `a: T` and `b: ref T` is a type error with the fix-it `*b`.
- **Using `*r` as a value copies it**, so it needs `T: Copy`. For a non-`Copy` `T` that would be a move out of a reference, an error ([core-semantics.md §3.7](core-semantics.md#3-moves)). Operators that borrow their operands, such as `==` and `<`, borrow `*r` and need no copy.
- **`*r` is assignable when `r: mut ref T`.** `*r = v` drops the old value and stores the new one ([core-semantics.md §7.5](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)). Through a `ref T` it is read-only.
- **Compound assignment through a `mut ref` binding writes through it.** `x += 1` with `x: mut ref i64` means `*x = *x + 1` and updates the caller's value. Plain `=` does not auto-deref: write `*x = v`.
- **There is no user `Deref` trait and no `&` operator.**

**`ref place` borrows a place.** `ref v[i]` and `ref s.field` produce a `ref` to that place. This is how a read that must not move or copy is written:

```kara
let t = ref heap.items[i];       // borrow an element
let first = Some(ref self.items[0]);   // build an `Option[ref T]` in a method body
```

A local bound this way is a reference, with the origins of [core-semantics.md §5.3](core-semantics.md#5-references-and-views-c5).

**A non-`Copy` field read through a reference is an error.** `let m = s.r;` with `s: ref S`, or a bare parameter `s: S`, and a non-`Copy` `r` would move out of a borrowed place ([core-semantics.md §3.7](core-semantics.md#3-moves)). Write `s.r.clone()` for an independent value, or `ref s.r` to borrow it. The same holds for an element read through an index ([`Index` and `IndexMut`](#index-and-indexmut)).

### Function values

**Named functions are values.** The name is the value; no wrapper is needed.

```kara
fn add(a: i64, b: i64) -> i64 { a + b }
fn sub(a: i64, b: i64) -> i64 { a - b }
fn mul(a: i64, b: i64) -> i64 { a * b }
fn apply(f: Fn(i64, i64) -> i64, a: i64, b: i64) -> i64 { f(a, b) }

let f = add;                                   // f: Fn(i64, i64) -> i64
let ops: Vec[Fn(i64, i64) -> i64] = [add, sub, mul];
let result = apply(add, 3, 4);
```

- **One function type.** There is no separate function-pointer type. A named function and a closure both have a function type (`Fn`, `MutFn` or `OnceFn`, below); a named function is an `Fn`.
- **Effects carry into the type.** `fn save(u: User) with writes(UserDB)` gives `let f = save;` the type `Fn(User) with writes(UserDB)`. Calling `f` has the same effect as calling `save`. See [§12 Effects](#12-effects).
- **Unbound methods are function values.** `User.validate` is a function whose first parameter is the receiver. With `fn validate(self) -> Result[(), ValidationError]`, `let check = User.validate;` has type `Fn(User) -> Result[(), ValidationError]`.
- **There are no bound method values.** `user.validate` without parentheses is a field access. To capture the receiver, write a closure: `|| user.validate()`.
- **Named parameters.** A function type has no labels. A function with named parameters can be a value only if every named parameter has a default. The value takes the positional parameters, and each call through it uses the defaults: with `connect` from [Named and default parameters](#named-and-default-parameters), `let c = connect;` has type `Fn(String) -> Connection`.
- **Generic functions as values** take their type arguments from an annotation:
  ```kara
  fn sort[T: Ord](list: own Vec[T]) -> Vec[T] { ... }
  let f: Fn(own Vec[i64]) -> Vec[i64] = sort;     // T = i64 from the annotation
  ```
  Where no annotation is available, a closure with an annotated parameter pins the type: `|v: own Vec[i64]| sort(v)`. There is no `sort[i64]` form; type arguments are never written at a use site ([§8](#8-type-inference-and-generics)).

There is no pipe operator `|>`. Method chains cover pipelines, and UFCS covers free functions. The pipe design is kept in [deferred.md](deferred.md#unscheduled-language-extensions) and may return.

### Closures

A closure is written `|params| expr` or `|params| { block }`:

```kara
|x| x + 1
|x, y| x * y
|(a, b)| a + b                  // destructuring, as for function parameters
|item: String| item.len()
|| println("no parameters")
```

There are no capture prefixes. How a closure captures each place is inferred ([Captures](#captures)).

**Parameter types** come from the expected function type, from an annotation, or from inference within the enclosing function body ([§8](#8-type-inference-and-generics)).

**Parameter modes.**
- **With an expected type**, each parameter's mode is the expected type's. For an expected `Fn(T) -> U`, `x` borrows, and a body that moves `x` is an error at the closure. For an expected `Fn(own T) -> U`, `x` is owned.
- **An annotation** such as `|x: own T| body` sets one parameter's mode.
- **With neither**, the parameter borrows, like a bare parameter in a declaration. Modes are never inferred from the body ([core-semantics.md §4.1](core-semantics.md#4-parameters-calls-and-patterns)).

### Function types and kinds

A function type has one of three **kinds**, which say how often and through what access the value may be called ([core-semantics.md §9.6](core-semantics.md#9-closures)):

| Type | Calls | Callable through |
|---|---|---|
| `Fn(A) -> R` | any number; a call only reads the captures | any access, a `ref` or a bare parameter included |
| `MutFn(A) -> R` | any number; a call may mutate the captures | a unique, mutable access only: a `let mut` binding or a `mut ref`, never a bare parameter |
| `OnceFn(A) -> R` | once; the call moves the value | an owned value; a second call is a use after move |

- **A closure literal gets the most permissive kind its body allows**: `Fn` if it only reads its captures, `MutFn` if it mutates one, `OnceFn` if it moves one out.
- **A named function is an `Fn`.**
- **A `MutFn` or `OnceFn` parameter is declared `own`** (a `MutFn` may also be `mut ref`). A bare parameter is a shared borrow, and a shared borrow can call only an `Fn` ([core-semantics.md §9.6](core-semantics.md#9-closures)).
- **Calling a local `MutFn` needs `let mut`**, like any mutation of a binding ([§6 Variable binding rules](#variable-binding-rules)). An `own` parameter is rebound first: `let mut f = f;`.
- **An `Fn` closure can be `Copy`.** A closure of kind `Fn` whose captures are all `Copy`, `ref` captures included, is `Copy` ([core-semantics.md §1.1](core-semantics.md#1-values)), so passing it to an `own` parameter copies it and leaves it usable. A `MutFn` or `OnceFn` closure never is.
- **Subsumption.** An `Fn` may be passed where a `MutFn` or `OnceFn` is expected, and a `MutFn` where a `OnceFn` is expected. The reverse is an error.
- **Orthogonal parts.** The kind, the `escaping` bit and the effect clause are independent. A parameter may be declared `h: escaping MutFn(Event) with writes(Log)`.
- **Syntax.** `Fn(T) -> U`, `Fn(T)` (returns `()`), `Fn(T) -> U with writes(R)`. Several effects are space-separated after `with`, as in a declaration. `Fn`, `MutFn` and `OnceFn` are built-in type names.
- **Argument modes** are written as in a parameter list: a bare argument type borrows, `own T` is owned, and `mut ref T` and the slice forms are unchanged. `Fn(own Request) -> Response` takes ownership of its argument; `Fn(Event)` borrows it.
- **Where a `with` attaches.** In a parameter, field or element type, a `with` after a function type belongs to the type, since nothing else can follow it there. In return position a trailing `with` is the function's own clause, so a function-type return with its own clause is parenthesized: `fn make_logger() -> (Fn(Str) with writes(Log)) with allocates(Heap)`.

```kara
fn repeat(n: i64, f: own MutFn()) {
    let mut f = f;                  // calling a `MutFn` mutates it
    for _ in 0..n { f(); }
}

let mut count = 0;
repeat(3, || { count += 1; });      // a `MutFn`: it mutates `count`
println(count);                     // 3

fn run_once(f: own OnceFn() -> String) -> String { f() }

let name: String = "ada";
let greeting = run_once(|| name);   // a `OnceFn`: the body moves `name` out
```

**Calling through a collection.** `handlers[0]()` borrows the element, so it works when the element type is `Fn`. For `MutFn` elements, call through a `mut ref`. For `OnceFn` elements, take the value out first: `handlers.remove(0)()`.

**Diagnostics for a kind mismatch.** Passing a closure where a more permissive kind is expected, or calling a `OnceFn` twice, is an error. The diagnostic must:
1. point at the capture that makes the closure `MutFn` or `OnceFn`, and at the use in the body that mutates or moves it;
2. show the parameter or the first call that requires the more permissive kind;
3. offer concrete fixes: `.clone()` the value before the closure, or inside the body, or restructure to avoid the second call.

### Captures

A closure captures each place from its enclosing scope in the weakest mode its body needs ([core-semantics.md §9.1](core-semantics.md#9-closures)):
- **by `ref`** if the body only reads the place;
- **by `mut ref`** if the body mutates it;
- **by move** if the body moves it, or if the closure escapes ([Escaping function values](#escaping-function-values)).

`Copy` places are copied. Any closure may capture a `shared`, `sync` or `frozen` handle. Capturing a `shared` or `sync` handle counts it ([core-semantics.md §6.1](core-semantics.md#6-sharing-c7)). A `frozen` handle is never counted, so a closure that captures one is a view: it may be called, passed down, and enter `par` branches and `TaskGroup` tasks, but it may not escape ([core-semantics.md §9.1](core-semantics.md#9-closures), [§5.7](core-semantics.md#5-references-and-views-c5)). A closure with any `ref` or `mut ref` capture is a view too: it can be called and passed down, but not stored where a view may not go ([core-semantics.md §9.2](core-semantics.md#9-closures)). To give a closure its own copy of a value, `.clone()` it before the closure.

**Disjoint capture: closures capture places, not whole bindings.** A closure that mentions `user.name` and `user.age` captures those two fields, each in its own mode. The rest of `user` stays usable outside the closure.

```kara
struct User { name: String, age: i64, history: Vec[Event] }

fn split_capture(user: own User) {
    let greet = || println(f"hi, {user.name}");   // captures `user.name` by `ref`
    let next_age = || user.age + 1;                // copies `user.age`
    archive(user.history);                         // moves `user.history` to an `own` parameter: a disjoint place
    greet();
    println(next_age());
}
```

**Where a captured path stops.** The analysis walks out from each use of a captured name through `.field` accesses. It captures the root whole when it reaches:
- an index (`v[i]`): the index is a runtime value, so `v` is captured whole;
- a method call (`user.update()`): `user` is captured whole, in the mode the method's receiver needs;
- a dereference of a captured reference (`(*p).field` with `p: ref Foo`): `p` is captured whole;
- an argument to a function (`f(user)`): `user` is captured whole, in the mode `f`'s parameter needs.

Any chain of `.field` accesses from a captured local is its own path: `user.address.city` and `user.address.zip` are two paths, and the fields nobody mentions are not captured.

**After the closure is created**, the root is checked against the captured paths under the ordinary borrow rules ([core-semantics.md §5.6](core-semantics.md#5-references-and-views-c5)):
- an unmentioned sibling path may be used freely;
- a path captured by move is moved;
- a path captured by `ref` may be read but not written;
- a path captured by `mut ref` may not be used until the closure's last use;
- the whole root may not be moved while any path under it is captured, and may not be read while a path under it is captured by `mut ref`.

**Captures drop with the closure**, in reverse capture order ([core-semantics.md §9.4](core-semantics.md#9-closures)).

**Capturing is not an effect.** Capturing a `ref Database` gives the closure no `reads(Database)`; calling `db.query()` in the body does. See [§12 Effects](#12-effects).

### Escaping function values

A closure **escapes** when it, or a value containing it, is returned, stored in a field, a collection or a global that is not a local view, passed to an `escaping` parameter, or sent through a channel ([core-semantics.md §9.3](core-semantics.md#9-closures)).

- **An escaping closure captures every place by move.** Using a captured place afterwards is a use after move. Clone the value before creating the closure if the enclosing function still needs it.
- **Function-typed parameters are non-escaping by default.** The callee may call such a parameter, or pass it to another non-escaping parameter, and nothing else.
- **`escaping` is written only on parameters**, the one place where a function type is non-escaping by default. A function type in a return type, a field or a collection element is escaping by nature and is not marked: `make_counter` below returns a `MutFn() -> i64`.
- **An `escaping` parameter is owned.** The argument moves into the callee, and `own` is not written beside `escaping`.
- **To store or return a parameter**, declare it `escaping`:

```kara
fn make_counter() -> MutFn() -> i64 {
    let mut n = 0;
    || { n += 1; n }               // escapes by being returned: `n` is moved in
}

fn subscribe(bus: mut ref Bus, handler: escaping MutFn(Event)) {
    bus.handlers.push(handler);    // stores the parameter, so it is declared `escaping`
}
```

- **Tasks never take escaping closures.** There is no free `spawn`. `par {}`, `par for` and `TaskGroup.spawn` take non-escaping closures that may borrow ([§13 Concurrency](#13-concurrency)).
- **Effects.** A call through a non-escaping parameter has the effects of the argument passed at each call site. A call through an escaping value has the effects its type declares, or every effect if its type declares none. See [§12 Effects](#12-effects) and [core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10).

---

## 8. Type inference and generics

### Generic parameters and arguments

Generic parameters are written in square brackets: `Vec[i32]`, `fn sort[T: Ord](list: own Vec[T]) -> Vec[T]`. `[` is not a comparison operator, so there is no ambiguity with less-than (prior art: Go 1.18). Const parameters use the `const` prefix: `fn rotated[T, const N: i64](arr: own Array[T, N]) -> Array[T, N]` ([Const generic parameters](#const-generic-parameters)).

**`[T]` names and declares types. It is never applied at a call site.** `[` is also the index operator, and `f[x](args)` is already a legal index followed by a call (an element of a `Vec[Fn(i64) -> i64]`, invoked). A call-site `f[T](args)` could only be told from indexing by asking whether the bracket holds a type and the callee is generic, which breaks as soon as a binding shadows a type name. So types at a call come from one of two places:
- **A qualified receiver:** `Vec[i64].new()`, `u32.size_of()`. A type in receiver position cannot be an index, because a type cannot be indexed.
- **An annotation:** `let v: Vec[i64] = Vec.new();`, `let p: *const u32 = ptr.null();`.

**Generic arguments against indexing.** The parser decides by syntax alone:
- **In a type position** (annotations, return types, field types, bounds), `Name[T1, ..., Tn]` is always a generic type.
- **In an expression**, `e[i]` is always an index, except that a Type-class identifier or a primitive type name followed by `[` always begins a type path (`Map[String, i64].new()`), because such a name is never a value.
- **There is no generic function value by brackets.** `sort[i32]` alone is an index into `sort`. For a function at a chosen type, annotate the binding (`let f: Fn(own Vec[i32]) -> Vec[i32] = sort;`) or write a closure with an annotated parameter (`let f = |v: own Vec[i32]| sort(v);`), as [§7](#function-values) shows.

### Type inference

Type inference is **bidirectional**: two judgments, `synthesize` and `check`, with fixed rules for which applies where. Type variables that neither judgment can settle at once are solved by unification within one function body.

**Why this algorithm.** Global Hindley-Milner inference was rejected because effect sets (subset inclusion, not equality) combine awkwardly with unification-only solving, and because a function's types must not depend on its callers. Pure bottom-up synthesis was rejected because it cannot type a closure's parameters from context. Bidirectional typing is what Dotty and Swift use for a similar feature set.

**The two judgments.**
- **Synthesize:** `Γ ⊢ e ⇒ τ`. From the context `Γ` and expression `e`, produce a type `τ`. Used when no expected type is known.
- **Check:** `Γ ⊢ e ⇐ τ`. Verify that `e` has type `τ` or a subtype of it ([Subtyping and variance](#subtyping-and-variance)). Used when an expected type is known.

**Subsumption.** In check mode, `e ⇐ τ` succeeds if `e ⇒ τ'` and `τ' ≤ τ`. This is the one rule connecting the judgments. When both a synthesis and a check rule apply, check mode wins because it carries more information.

**Which mode applies.**

| Context | Mode |
|---|---|
| Expression with no binding and no expected type | synthesize |
| `let x = e` (no annotation) | synthesize `e`, bind `x` to the result |
| `let x: T = e` | check `e ⇐ T` |
| Function body with declared return type `R` | check the body `⇐ R` |
| `return e` in a function returning `R` | check `e ⇐ R` |
| Call `f(arg)` where `f`'s parameter type is `P` | check `arg ⇐ P` |
| `if c { t } else { e }` with an expected type | check both branches |
| `if c { t } else { e }` with no expected type | synthesize both branches, join at their [least upper bound](#least-upper-bound) |
| `match` with an expected type | check every arm |
| `match` with no expected type | synthesize each arm, join at their least upper bound |
| Closure `\|x\| body` against an expected function type `Fn(A) -> B` (or `MutFn`, `OnceFn`) | `x : A`; check `body ⇐ B`; the closure's kind and effects must fit the expected type ([§7](#7-functions-and-closures), [§12](#12-effects)) |
| Closure `\|x: A\| body` with no expected type | synthesize `body ⇒ B`; the closure's kind and effects come from its body ([core-semantics.md §9.6](core-semantics.md#9-closures)) |
| Struct literal `S { f: e, ... }` | check each field against its declared type |

**Core rules.**
- **Literals.** An integer or float literal starts as a type variable that later uses may fix. If nothing fixes it by the end of the function body, it defaults to `i64` or `f64`. In check mode against a numeric type, a literal takes that type, and its value is range-checked at compile time. A string literal is `String`, or `Str` in check mode against `Str` ([§3](#string-literals)). `true` and `false` are `bool`. Operands of different numeric types follow [§5 Mixed-width operands](#mixed-width-operands).
- **Variables.** `Γ ⊢ x ⇒ Γ(x)`. Variables synthesize and then subsume.
- **`let`.** `let x = e` synthesizes and binds. `let x: T = e` checks `e ⇐ T` and binds at `T`.
- **`if`.** The condition checks against `bool`. In check mode against `τ`, both branches check against `τ`. In synthesis mode, the result is the branches' least upper bound, and it is a compile error if there is none: Kāra has no `Any` type and does not widen arbitrary pairs.
- **`match`.** Like `if`, over all arms. Pattern bindings take their types from the scrutinee. Exhaustiveness is a separate check ([§5](#5-types)).
- **Calls.** Synthesize the callee's type `Fn(P1, ..., Pn) -> R`, check each argument against its parameter type, and synthesize `R`. The callee's effects join the caller's ([§12](#12-effects)).
- **Method calls.** `e.m(args)` synthesizes `e ⇒ τ`, resolves `m` ([§9](#9-traits)), and continues as a call.
- **Closures.** In check mode against `Fn(A1, ..., An) -> R`, each parameter takes its `Ai` and the body checks against `R`. In synthesis mode, each parameter needs an annotation or a type that later uses in the same function body fix; a parameter still unknown at the end of the body is `cannot infer closure parameter type; annotate or call in a context with an expected type`.
- **Struct literals.** In check mode, each field checks against its declared type. In synthesis mode, the literal names the struct. **Field shorthand:** `Point { x, y }` means `Point { x: x, y: y }`, and the forms mix: `User { name, email, password_hash: hash(password) }`.
- **Casts.** `e as T` synthesizes `T`. Only the numeric casts of [§5](#5-types) are legal outside `unsafe`.

**Type variables are solved within one function body.** A generic call, a constructor whose parameters are unknown (`Vec.new()`), a numeric literal and an unannotated closure parameter each introduce a type variable. Any later use in the same function body may solve it by unification:

```kara
fn sum(xs: Vec[i32]) -> i32 { ... }
fn take_i32(n: i32) { ... }

fn demo() -> i32 {
    let x = 5;               // x: an integer type, not yet fixed
    take_i32(x);             // fixes it: x is i32
    let mut v = Vec.new();   // v: Vec[?T]
    v.push(7);               // ?T is an integer type
    sum(v)                   // fixes it: v is Vec[i32]
}
```

- **Literals default last.** A numeric type variable takes its default (`i64`, `f64`) only at the end of the body, after every use has had its chance to fix it. `Vec.filled(5, 0)` with no other use is `Vec[i64]`; `let x = identity(3.14);` is `f64`.
- **No generalization at `let`.** A `let`-bound value, a closure included, has one type.
- **An unsolved variable is an error at the end of the body**: `cannot infer type ...; annotation needed`, pointing at the expression that introduced it and naming the unsolved parameters.
- **Inference never crosses a function boundary.** Signatures are always written, so a body never changes the types its callers see, and a caller never changes a callee's.

**`Never` never silently widens to `()`.** When a `Never` value is the only thing that fixes a type variable, the variable is `Never`:

```kara
fn id[T](x: own T) -> T { x }

let y = id(panic("never"));     // y: Never, not ()
let f = || panic("nope");       // f: Fn() -> Never, not Fn() -> ()
let v = Some(todo());           // v: Option[Never], not Option[()]
```

Two cases: a type variable whose only constraint is a `Never` value is `Never`, and a closure, function or block whose body always diverges has result type `Never`. Both follow from `Never ≤ T` for every `T`: `Never` is the only solution that meets every constraint. Falling back to `()` would make a value's type depend on whether the caller annotated it, and that would change monomorphizations silently.

When a variable has both a `Never` constraint and a concrete one, the concrete type wins by the least-upper-bound rule (`LUB(Never, T) = T`):

```kara
fn pick[T](x: own T, y: own T) -> T { if cond() { x } else { y } }

let a = pick(42, panic());      // a: i64
let b = pick(panic(), panic()); // b: Never
```

Where the expected type is already known, a `Never` value simply converts to it. The rule above governs only what a variable solves to when nothing else fixes it.

### Least upper bound

`LUB(A, B)` joins two branch types in synthesis mode (`if`, `match`). The first matching rule applies:

0. **`Never` is the bottom.** `LUB(Never, T) = T` and `LUB(T, Never) = T`. `Never` is the type of expressions that produce no value: `panic(...)`, `return`, `break`, `continue`, `todo()`, `unreachable()`, a `loop` with no reachable `break`, a call to a function declared `-> Never`. A diverging branch contributes nothing to the join.
1. **Identical types.** `LUB(T, T) = T`.
2. **Same generic type, some arguments unknown.** Unify the unknown arguments: `LUB(Result[i64, ?E], Result[?T, String]) = Result[i64, String]`, with `?E = String` and `?T = i64`. A conflict is a type mismatch at the branch.
3. **Same generic type, known arguments that differ.** No LUB: `LUB(Vec[i64], Vec[String])` is an error. Annotate the binding to switch to check mode.
4. **Anything else.** No LUB: `type mismatch in branches: expected <first-branch-type>, found <other-branch-type>`, with a note listing every branch type and a suggestion to annotate.

For `match`, the join runs left to right: the first arm sets the running type, and each later arm joins it. The first pair with no LUB is the error site.

### Bounds and generic bodies

**A generic body uses only what its bounds provide.** Inside `fn f[T: Ord + Display](...)`, a value of type `T` supports the methods and operators of `Ord` and `Display` and nothing else ([§9](#9-traits)). The body is type-checked once, against its bounds.

**So instantiation adds no type errors.** Every instance of a well-typed generic body is well-typed. What does run per instance:
- the view checks of [core-semantics.md §5.8](core-semantics.md#5-references-and-views-c5), because a type parameter may be instantiated with a view;
- effect computation: a call through a bound has the effects its trait method declares, or, when the trait method declares none, the effects of the instance's method ([§12](#trait-methods-and-generic-calls), [core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)).

**Bounds are checked at the call.** When a call solves `T` to a concrete type, each bound must have an applicable impl, chosen under the coherence rules of [§9](#9-traits). A missing impl is an error at the call site, naming the type and the unsatisfied bound and suggesting where an impl could go. This is what lets `Map[K, V]` rely on `K: Hash + Eq`.

`CrossTask`, the bound that gates channels and tasks, is computed by the compiler and cannot be implemented by hand ([§9](#9-traits)).

### `where` clauses

Bounds may move to a `where` clause after the return type and effects, when they are long or many:

```kara
// Inline bounds: fine for short constraints
fn sort[T: Ord](list: own Vec[T]) -> Vec[T] { ... }

// A where clause: clearer for long or numerous bounds
fn merge_sorted[T, U](
    left: Vec[T],
    right: Vec[T],
    transform: Fn(T) -> U,
) -> Vec[U]
where
    T: Ord + Clone + Display,
    U: From[T] + Into[String],
{ ... }
```

- **Inline and `where` bounds mix** in one declaration:

  ```kara
  fn process[T: Ord, U](items: own Vec[T]) -> U
  where U: From[T] + Display
  { ... }
  ```

- **On `impl` blocks**, with the same syntax:

  ```kara
  impl[T] Display for Pair[T]
  where T: Display
  {
      fn to_string(self) -> String { ... }
  }
  ```

- **On `struct` and `enum` definitions**, allowed but rarely needed:

  ```kara
  struct SortedPair[T]
  where T: Ord
  {
      first: T,
      second: T,
  }
  ```

- **Order in a function declaration:** signature, then effects, then `where`, then the body:

  ```kara
  fn binary_search[T](haystack: Vec[T], needle: T) -> Option[i64]
      with reads(SearchIndex)
  where T: Ord
  { ... }
  ```

- **`where` holds type bounds only.**

### Default generic arguments

A generic parameter of a type or a trait may have a default. An argument left out takes the default:

```kara
struct Map[K, V, H = SipHash13BuildHasher] { ... }   // Map[String, i64] uses the default hasher

trait Add[Rhs = Self] {
    type Output;
    fn add(own self, rhs: own Rhs) -> Self.Output;
}

impl Add for Money { ... }      // impl Add[Money] for Money
fn total[T: Add](a: own T, b: own T) -> T.Output { a + b }    // T: Add[T]
```

Parameters with defaults come after those without. The operator traits rely on this ([§9](#operator-traits)), and so does the hasher parameter of `Map` and `Set` ([library/collections.md](library/collections.md)).

A function's generic parameters take no defaults, because a function never takes type arguments at a call ([Generic parameters and arguments](#generic-parameters-and-arguments)).

### Const generic parameters

A generic list may hold const parameters beside type parameters. A const parameter is a value fixed per instance:

```kara
fn rotated[T, const N: i64](arr: own Array[T, N]) -> Array[T, N] { ... }

struct Matrix[T, const ROWS: i64, const COLS: i64] { ... }

trait FixedBuf[const CAP: i64] { ... }
```

The `const` prefix is what makes the parameter a value. Its name is Const class, and a single letter such as `N` is allowed ([CN-7](#rules)).

**Permitted types:** `i64` (and `i8`, `i16`, `i32`, `i128` where a bit width matters), `bool`, `char`, and enums whose variants all have no fields. Other types (`String`, floats, structs, enums with fields, distinct types) are rejected at the declaration: `const generic parameters must be of integer / bool / char / fieldless-enum type`. Floats are excluded because NaN payloads make equality of instances ambiguous; the others because a const argument must have a finite representation with stable equality, usable as an instance key. `usize` is not permitted: sizes, counts and capacities are `i64` in Kāra ([§5](#5-types)), in the type system too.

**Arguments** are integer literals, `true`/`false`, character literals and qualified fieldless variants:

```kara
let buf: Array[u8, 256] = [0; 256];      // N = 256
let mat: Matrix[f64, 3, 4] = ...;        // ROWS = 3, COLS = 4
let asc: Sorted[T, Order.Ascending] = ...;
```

A negative argument is accepted by the parameter (`const N: i64` takes `-1`); whether it makes sense is the type's concern. `Array[T, N]` requires `N >= 0` as a bound on its definition.

**Constant expressions as arguments.** An argument may be any constant expression of the parameter's type: `Array[T, N + 1]`, `Matrix[T, M, K * 2]`, `Buffer[T, BLOCK_SIZE * 2 - 1]`. These are evaluated during type checking, with the same expressions allowed as in a `const` initializer ([Constants](#constants)). Calls wait for `const fn` (comptime track). A run-time value in an argument position is `const generic argument must be a compile-time constant expression`. Evaluation uses checked arithmetic: overflow is a compile error.

**Constant expressions as bounds.** `where` clauses may constrain const parameters:

```kara
fn split_half[T, const N: i64](arr: own Array[T, N])
    -> (Array[T, N / 2], Array[T, N - N / 2])
    where N >= 0
{ ... }

impl[const N: i64] FixedBuf[N] for Buffer[N] where N > 0, N <= 4096 { ... }
```

The compiler evaluates each bound at each instance, directly: no SMT solver and no symbolic reasoning across calls. A failed bound is `const constraint violated: <expr> evaluated to false`, listing the call site and the argument values.

**Inference.** A const parameter that appears in a parameter type is solved from the argument, like a type variable: `first(my_array)` solves `N` from `my_array`'s type, given `fn first[T, const N: i64](arr: own Array[T, N]) -> T`. A const parameter that appears only in the return type or in `where` clauses is solved from the expected type, since there are no call-site type arguments:

```kara
fn zeros[T: Zero, const N: i64]() -> Array[T, N] { ... }

let a: Array[i32, 8] = zeros();  // T = i32, N = 8, from the annotation
let b = zeros();                 // error unless a later use fixes N: cannot infer const parameter N
```

The same `N` solved to two values is a const-argument mismatch at the conflicting position.

**Instances.** Each distinct tuple of type and const arguments is a separate instance ([Monomorphization](#monomorphization)). Const parameters are erased to their values: they are not run-time data and do not appear in dispatch tables. `karac query monomorphization` counts const arguments in the instance key `(T1..Tk, C1..Cm)`, so a function with const parameters called with many values shows its instance count.

Only `Array[T, N]` is implemented so far. Constant-expression arguments, const bounds in `where`, fieldless-enum parameters and solving several const parameters at once are not yet implemented.

### Subtyping and variance

Kāra has nominal types and little subtyping. The subtyping relation has exactly these sources:
- **`Never ≤ T`** for every type `T`.
- **Function types.** Contravariant in parameter types, covariant in the result, covariant in the effect set: a function with fewer effects may be used where one with more is expected ([§12](#12-effects)).
- **Function kinds.** An `Fn` may be used where a `MutFn` or `OnceFn` is expected, and a `MutFn` where an `OnceFn` is expected ([core-semantics.md §9.6](core-semantics.md#9-closures)).
- **Covariant built-in types**, below.

**Variance is never inferred from a type's structure.** It is fixed by position, and for built-in types by this specification. Inference can produce the wrong answer in the presence of interior mutability or generic instantiation, and explicit declaration is worth its cost.

**Variance by position:**

| Position | Variance |
|---|---|
| `mut ref T` target | invariant |
| `ref T` target | covariant |
| Function result | covariant |
| Effect set of a function type | covariant |
| Function parameter | contravariant |

**`mut ref T` is invariant.** `mut ref Sub` is not a subtype of `mut ref Super`, nor the reverse. A `mut ref Super` can write any `Super`, including one that is not a valid `Sub`; if `mut ref Sub` could become `mut ref Super`, a write through one alias could store a value that a read through the other cannot soundly type. A function taking `mut ref T` therefore receives exactly `T`.

**Built-in types:**

| Type | Variance |
|---|---|
| `Option[T]`, `Result[T, E]`, tuples | covariant in every parameter |
| `TaskHandle[T]` | covariant (it only yields a `T`) |
| `Vec`, `Slice`, `Array`, `Map`, `Set`, `VecDeque`, `SortedMap`, `SortedSet` | invariant |
| `Sender[T]`, `Receiver[T]` | invariant |
| `Atomic[T]`, `Mutex[T]` | invariant (interior mutability) |
| `MaybeUninit[T]`, `PhantomData[T]` | invariant |

**User-declared generic types are invariant in every parameter.** Variance markers (`+T`, `-T`) are deferred ([deferred.md](deferred.md#unscheduled-language-extensions)), as is higher-kinded polymorphism.

### Monomorphization

Kāra emits **one specialized implementation per tuple of type and const arguments** for every generic: user functions and types, and the standard collections (`Vec[T]`, `Map[K, V]`, `Set[T]`). There is no type erasure, no vtable dispatch for generics and no function-pointer indirection inside collections. This is a design property of code generation, not an implementation detail: it shapes the ABI, what the optimizer can see, and the runtime.

**What monomorphizes.**
- **Generic functions.** `fn identity[T](x: own T) -> T` called with `i64` and `String` produces two functions. Instances are deduplicated by their argument tuple, so `identity` at `i64` is emitted once however many calls use it.
- **Generic types.** `Pair[i64, String]` and `Pair[String, String]` are two compiled types with their own methods.
- **Standard collections.** Each is compiled per argument tuple, with `K`'s hash and equality inlined into `Map[K, V]`, so the optimizer sees the whole collection body.
- **Trait method calls in generic code.** In `fn process[T: Display](xs: Slice[T])`, each instance calls its `T`'s `to_string` directly. No vtable.

**What does not.**
- **`dyn Trait`**, which returns with the services track ([deferred.md](deferred.md#dyn)), dispatches through a vtable by design. It is a separate feature from generics.
- **Effects are not part of the instance key.** An instance is chosen by its type and const arguments alone. Its effects are computed on the instance, and the effects of a function passed to a non-escaping parameter are charged to each caller ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)).

**The standard library ships as source.** Collection code is compiled into each program at the instances it uses, like Rust's `std::collections`, not called through a prebuilt archive. The runtime library holds only non-generic primitives: panic support, the allocator interface, FFI bridges and OS bindings.

**Cost.** Binary size grows with the number of distinct instances: a program using `Map[i64, i64]`, `Map[String, i64]`, `Map[String, Vec[i64]]` and `Set[String]` carries four map and set implementations. Stripping, LTO and dead-code elimination remove unused methods per instance, but each instance has a floor. A typical small program is about Rust's size; one with many instances grows linearly. In return, collections run at close to Rust's speed, the optimizer reaches into collection bodies (constant folding through hashing, vectorized bulk operations, scalar replacement of keys), and no probe pays an indirect call. Kāra chooses run-time speed over archive size.

There is no dynamic linking ([deferred.md](deferred.md), permanent omissions); monomorphization is the alternative to plugin-style extension.

---

## 9. Traits

A trait names a set of methods and associated types that a type can implement. Traits are how code is generic over behaviour: Kāra has no classes, no inheritance and no subtyping between nominal types. The standard library is ordinary Kāra code over this trait system. The compiler knows a short, closed list of [lang-item traits](#lang-item-traits) and nothing more.

### Declarations and impls

A trait declares methods and associated types.
- A method ending in `;` is **required**: every impl must provide it.
- A method with a body is a **default**: an impl may override it or leave it out.

```kara
trait Shape {
    fn area(self) -> f64;                           // required
    fn describe(self) -> String {                   // default
        f"a shape with area {self.area()}"
    }
}

struct Circle { radius: f64 }

impl Shape for Circle {
    fn area(self) -> f64 { 3.14159 * self.radius * self.radius }
}
```

An `impl` block without `for` is an **inherent impl**. It adds methods to the type itself:

```kara
impl WordCount {
    fn total_ratio(self) -> f64 {
        self.unique as f64 / self.total as f64
    }
}
```

The rules:
- **Method calls and UFCS are the same call.** `user.validate()` and `User.validate(user)` call the same method. The second form, with a trait name (`Shape.area(c)`), names a trait method explicitly.
- **Receivers.** A method's first parameter may be a receiver: `self` (borrowed), `own self` (consuming) or `mut ref self` ([§7 Parameter modes](#parameter-modes)). A `shared` type's methods take `self` only.
- **A method without a receiver is an associated function**, called on the type: `T.default()`. See [Associated functions](#associated-functions).
- **Parameter names are required in trait methods too.** `fn visit(self, i32);` is error `E_TRAIT_METHOD_ANONYMOUS_PARAM`. Write `_: i32` for a parameter whose name means nothing. An impl may use different parameter names from the trait; only types and modes must match.
- **Conformance.** An impl method has exactly the trait method's parameter and return types. Its parameter modes, the receiver's included, may be the trait's or weaker, in the order `own`, `mut ref`, borrowed. A call on a known type uses the impl's modes; a call through a generic bound uses the trait's. See [core-semantics.md §4.7](core-semantics.md#4-parameters-calls-and-patterns).
- **Effects.** A trait method's `with` clause is a ceiling: each impl's method must stay within it, and a call through a bound has the clause's effects. A trait method with no clause has no ceiling, so a call through a bound has the effects of the instance's method, charged to whoever instantiates the generic function ([§12 Trait methods and generic calls](#trait-methods-and-generic-calls)).
- **An impl header takes no effect clause.** `impl From[A] for B with writes(Log) {` is error `E0005`, and `karac fix` deletes the clause. Effects belong on the impl's methods.
- **Generic impls** list their parameters after `impl`: `impl[T: Ord] Sorted[T] { ... }` (inherent) or `impl[T: Display] Display for Wrapper[T] { ... }` (trait). See [Conditional impls](#conditional-impls).
- **Associated types** are bound in the impl with `type Name = Type;`. See [Associated types](#associated-types).
- **Visibility.** `pub trait` exports a trait from its package ([§4 Visibility](#visibility)).

#### Associated functions

A trait method without a receiver is an associated function of the trait. It is dispatched on a type, not a value. Constructor traits use this form:

```kara
trait Default {
    fn default() -> Self;
}

trait FromStr {
    type Err;
    fn from_str(s: Str) -> Result[Self, Self.Err];
}
```

**It is called on a type**, written as the receiver: `T.default()`, `Config.default()`. The compiler looks `default` up on the type as it looks up methods (inherent first, then traits), with no receiver value. The ambiguity rules are the same. There is no bare `default()` form that takes its type from the expected type.

There are no call-site type arguments: the form is `T.default()`, never `default[T]()` ([§8 Generic parameters and arguments](#generic-parameters-and-arguments)).

An inherent associated function hides a trait one of the same name at `T.name()`, as inherent methods hide trait methods. `Trait.name(args)` reaches the trait's.

### Coherence and the orphan rule

For any trait and type, at most one impl applies. The compiler checks this where each impl is declared, so trait resolution never depends on which packages are in scope or on the order impls appear in.

- **Overlap is an error.** Two impls of one trait overlap when their headers unify, ignoring their bounds. `impl[T: Eq] Foo for Bar[T]` and `impl[T: Ord] Foo for Bar[T]` overlap, and so do `impl Foo for Bar[i64]` and `impl[T] Foo for Bar[T]`. The error is reported at the second impl. There is no specialization to choose between them ([No specialization](#no-specialization)).
- **A type implements a trait once.** A duplicate impl is the simplest overlap.
- **The orphan rule.** A package may write `impl Trait for Type` only if it defines `Trait` or `Type`, or both. The standard library counts as a package.
- **The final application package is exempt from the orphan rule** for trait impls. Nothing depends on an application, so its impls cannot conflict with another package's. An application may implement a foreign trait for a foreign type. The overlap rule still applies.
- **Blanket impls.** `impl[T: B1 + B2] Trait for T` is allowed only if the package defines `Trait` (or is the application). Owning one of the bounds is not enough: `T` ranges over every type, so no package owns it.
- **Inherent impls only on your own types.** A package may write `impl Type { ... }` only for a type it declares; the application exemption does not change this. Outside the standard library, `impl Vec[i64] { ... }` is an error. Write a trait, or wrap the type in a `distinct type`.
- **`Copy` and `Drop` are exclusive.** See [`Copy`, `Clone` and `Drop`](#copy-clone-and-drop).
- **Every impl form follows these rules**: unconditional, conditional, blanket, and impls with associated types.

```kara
// In a library package:

// The package defines the trait: OK
pub trait Printable { fn print(self); }
impl Printable for Vec[i64] { ... }      // own trait, foreign type

// The package defines the type: OK
struct MyPoint { x: f64, y: f64 }
impl Display for MyPoint { ... }         // foreign trait, own type

// Neither (Render comes from a dependency): an error in a library.
// In the application package it is allowed, unless it overlaps another impl.
impl Render for Vec[i64] { ... }
```

```kara
impl[T: SomeForeignTrait] MyTrait for T { ... }   // OK: the package defines MyTrait
impl[T, U: From[T]] Into[U] for T { ... }         // OK in the standard library, which defines Into
impl[T: MyConstraint] ForeignTrait for T { ... }  // ERROR in a library: ForeignTrait is not its own
```

**Why.** Without the orphan rule, two libraries could each provide `impl Display for Vec[i64]`, and a program depending on both would have no principled way to choose. With it, at most one library can provide the impl for any trait and type, so resolution is coherent whatever is in scope.

**The newtype escape hatch.** A library that needs a foreign trait on a foreign type wraps the type in a local `distinct type` ([§5 Distinct types](#distinct-types)):

```kara
distinct type DisplayVec = Vec[i64];

impl Display for DisplayVec {
    fn to_string(self) -> String { ... }
}
```

When the wrapper must also have its inner type's ABI, add `#[repr(transparent)]` ([§15](#15-unsafe-ffi-and-layout-control)).

**Compatibility.** Adding an impl that overlaps no existing impl is a minor-version-safe change. Removing an impl is a breaking change.

### Method resolution

For a call `e.m(args)` where `e` has type `T`:
1. **Inherent methods.** If `T`'s own impls declare `m` (including generic inherent impls whose bounds hold), that method is chosen. Trait methods named `m` are not considered.
2. **Trait methods.** Otherwise the candidates are the methods named `m` of every trait in scope that `T` implements, including conditional impls whose bounds hold for `T`. One candidate is chosen. Two or more are an ambiguity error.
3. **Receiver check.** The chosen method's receiver is then checked against `e`. `own self` moves `e` (or copies it, if `T` is `Copy`), `self` borrows it, and `mut ref self` borrows it mutably and needs a mutable place. A mismatch is an error at the call. The check never changes which method was chosen.

**Which methods a receiver finds** is a fixed rule of the language, not a user trait:
- A receiver of type `ref T` or `mut ref T` finds `T`'s methods. Calling an `own self` method through a reference to a non-`Copy` `T` moves out of a borrowed place, which is an error ([core-semantics.md §3.7](core-semantics.md#3-moves)).
- A `String` receiver also finds `Str`'s methods, after `String`'s own, and a `Vec[T]` or `Array[T, N]` receiver also finds `Slice[T]`'s, after its own. This is the coercion that passes a `String` as `Str` and a `Vec[T]` as `Slice[T]` ([§7 Parameter modes](#parameter-modes)), applied to the receiver.
- Inherent methods come first, then traits, as in steps 1 and 2.

There is no autoref ladder: the compiler does not try `T`, `ref T` and `mut ref T` as separate candidates, and the receiver mode never decides between two methods. An inherent `m(self)` always wins over a trait's `m(own self)`.

**Traits in scope** at a call site are:
- the traits the package declares;
- traits imported by name ([§4 Imports](#imports));
- the prelude's traits.

A trait's methods are candidates exactly when the trait's name resolves at the call site. There is no separate step that brings a trait's methods into scope.

**Ambiguity.** When two traits in scope both provide `m` for `T`, the call is an error. The diagnostic lists each candidate with its trait, signature and effects, and suggests the UFCS form.

**UFCS.** `Trait.method(receiver, args)` calls the named trait's impl directly and skips lookup. It is resolved at compile time: the trait is fixed at the call site, the impl is selected from the trait and the receiver type, and the call is checked against that impl's signature and effects. UFCS is the fix the ambiguity diagnostic points at, and the way to reach a trait method that an inherent method hides.

**No method found.** The error is "no method named `m` found on type `T`", with the most similar method names on `T` as suggestions.

**Distinct types do not reach their base type's methods.** `distinct type UserId = i64` does not make `i64`'s methods available on `UserId`. That is the point of a distinct type ([§5 Distinct types](#distinct-types)).

**No user `Deref`.** No trait can reroute method lookup through another type; the receiver rule above is the whole of it. A type that wraps a value exposes it as a field instead: a `MutexGuard[T]` has a public field `value: mut ref T`, so access is `g.value.count += 1` ([library/concurrency.md](library/concurrency.md)). A user `Deref` can be added later without breaking any program.

#### Effects of an ambiguous call

A type may implement two traits whose methods have the same name and different effects. Each impl is checked against its own trait only, so the two coexist:

```kara
trait Reader {
    fn access(self, key: Key) -> Value with reads(Db);
}

trait Writer {
    fn access(self, key: Key) -> Value with writes(Db);   // same name, other effect
}

struct Connection { ... }

impl Reader for Connection {
    fn access(self, key: Key) -> Value with reads(Db) { ... }
}

impl Writer for Connection {
    fn access(self, key: Key) -> Value with writes(Db) { ... }
}

fn use_conn(conn: Connection, key: Key) {
    let v = conn.access(key);              // ERROR: ambiguous, Reader.access or Writer.access?
    let r = Reader.access(conn, key);      // OK: this call has reads(Db)
    let w = Writer.access(conn, key);      // OK: this call has writes(Db)
}
```

**There is no union of effects across candidates.** Once UFCS picks a trait, that trait's impl is the only source of the call's effects. A union would either force every candidate to have compatible effects, which forbids the example above, or let a new trait added to a dependency silently widen a caller's inferred effects. With one source, the ambiguity diagnostic offers exactly two choices, each with its effect.

#### Generic receivers

In `fn f[T: Reader](x: T) { x.access(key); }`, lookup uses the bound, not any concrete type. The one bound gives one candidate, `Reader.access`, whatever other traits the type passed as `T` implements. With `T: Reader + Writer`, `x.access(key)` has two candidates and is the same ambiguity error; UFCS resolves it. A generic body may use only what its bounds provide, operators included ([§8 Bounds and generic bodies](#bounds-and-generic-bodies)).

Conditional impls take part only when their bounds hold for the receiver's type. An impl whose bounds do not hold is dropped from the candidates; that is how conditional impls narrow coverage, and it is not an error. If nothing remains, the error is "no method named `m`".

#### Raw pointers need a known pointee

A method call on a raw pointer (`*const T` or `*mut T`) needs `T` to be known at the call. If `T` is still an unresolved inference variable, the call is rejected:

```kara
let p = ptr.null();           // *const ?T: the pointee is unknown
unsafe { let v = p.read(); }  // ERROR: pointee type unresolved
```

```
error[E_RAW_POINTER_UNRESOLVED_POINTEE]: method 'read' on raw pointer requires a known pointee type
  --> src/decode.kara:14:21
   |
14 |     unsafe { let v = p.read(); }
   |                      ^ pointee type 'T' is unresolved at this call site
   |
help: annotate the pointer type explicitly:
        let p: *const u8 = ptr.null();
```

- **Why.** Pointer methods (`read`, `write`, `offset`, `read_unaligned`, `write_volatile` and so on) work on a memory window whose size and alignment come from `T`. With `T` unknown the call cannot be lowered. Rejecting it at the call names the place to annotate, instead of failing later at an unrelated site.
- **It fires** when the pointee has no solution by the time the call is resolved. Common causes:
  - the pointer came from `ptr.null()`, `ptr.null_mut()`, `ptr.dangling()` or `ptr.dangling_mut()`, and no later use fixes `T`;
  - it came from a generic helper whose type parameter nothing constrains;
  - it went through a generic container whose element type is unsolved.
- **It does not fire** when `T` is a generic parameter in scope (`fn f[T](p: *const T)`; it is known per instance), or when the binding is annotated (`let p: *const u32 = ...`).
- **The fix-it** annotates the binding's type. The diagnostic underlines the receiver, not the method name.
- **Order.** The check runs before the `unsafe` requirement ([§15](#15-unsafe-ffi-and-layout-control)), so it is reported inside an `unsafe` block too.
- **No lookup through the pointer.** Raw pointers are not dereferenced by method lookup. A method of `T` needs `(*p).method()`. The pointer's own methods are inherent methods of the pointer type.

#### No specialization

Kāra rejects overlapping impls where they are declared, so lookup never has to pick the "most specific" impl. There is no `default fn`, no priority between impls and no "most specific impl wins" rule. This is a permanent decision, not a deferral. Three reasons:
1. **Soundness.** Specialization in other languages has stayed incomplete for years, with open soundness holes. Adopting it would bring that uncertainty into Kāra's coherence rules.
2. **Stable resolution.** Without specialization, what `T: Trait` resolves to is fixed by the visible impls and the orphan rule. With it, adding a new, coherent impl in another package could change which impl an existing call uses, so adding a non-overlapping impl would stop being a safe minor-version change.
3. **Workarounds exist.** What specialization is used for elsewhere (a faster `clone` for `Copy` types, a faster `from_iter` for `Vec`) can be reached with a separate impl under a `Copy` bound, with default method bodies over existing bounds, with conditional impls, or with `#[derive]`.

If a real workload cannot be served by these and justifies overriding the reasons above, the question reopens as a language change at an edition boundary.

### Supertraits

A trait may require its implementors to implement other traits:

```kara
trait Ord: PartialOrd + Eq {
    fn cmp(self, other: Self) -> Ordering;
}
```

Every type that implements `Ord` must also implement `PartialOrd` and `Eq`. The compiler checks this at each `impl Ord for T` and rejects the impl if either is missing.

**A constraint, not inheritance.** `trait Ord: PartialOrd + Eq` means exactly "`Self: PartialOrd + Eq` holds inside this trait":
- no field inheritance (traits have no fields);
- no reuse of method bodies: each impl block implements its own trait's methods;
- no subtyping: `Ord` is not a subtype of `PartialOrd`, and the impls are independent;
- no dispatch up a chain: a call resolves through the one trait that declares the method ([Method resolution](#method-resolution)). If `cmp` is declared only on `Ord`, `x.cmp(y)` resolves through `Ord`, never through `Eq`.

**What supertraits are for:**
1. **Default bodies that use the other traits.** A default method on `Ord` may call `PartialOrd`'s and `Eq`'s methods, because they exist on any `Self`:
   ```kara
   trait Ord: PartialOrd + Eq {
       fn cmp(self, other: Self) -> Ordering;
       fn max(own self, other: own Self) -> Self {
           if self.cmp(other) == Ordering.Less { other } else { self }
       }
   }
   ```
2. **Shorter bounds.** `fn sort[T: Ord]` instead of `fn sort[T: Ord + PartialOrd + Eq + PartialEq]`.
3. **Extension traits.** `trait ExactSizeIterator: Iterator`: every exact-size iterator is an iterator, stated once at the trait.

A trait may require several traits (`trait Hashable: Eq + Copy`); each must be implemented separately. Deep chains (`A: B: C: D`) used only to organise a library's vocabulary are discouraged: if a link is not used by a default body or a bound, prefer flat traits with several bounds at the use site.

**Diagnostics say "requires".** A failing impl reports "`impl Ord for T` requires `impl PartialOrd for T`", never "extends" or "inherits".

**Effects are not inherited.** Each trait's methods declare their own effects, and each impl is checked against its own trait. `trait Writer: Reader` does not give `Writer`'s methods `Reader`'s effects ([§12 Trait methods and generic calls](#trait-methods-and-generic-calls)).

### Marker traits

A marker trait has no methods. It is a tag on types. The `marker trait` form makes that visible where the trait is declared:

```kara
marker trait Pod;                       // plain-old-data tag
marker trait Sealed[T];                 // generic marker
marker trait Storeable: Clone;          // marker with a supertrait
```

- **Body.** `marker trait Foo;` and `marker trait Foo { }` are the same. The `;` form is the usual one.
- **No methods.** A method in a marker trait body is `E_MARKER_TRAIT_HAS_METHOD`. The fix-its are "remove `marker`" and "remove this method".
- **No associated types or constants.** `type Item;` or `const SIZE: i64;` in the body is `E_MARKER_TRAIT_HAS_ITEM`.
- **Impls are empty.** `impl Pod for u8 { }`. Methods or items in the impl body are `E_MARKER_IMPL_HAS_METHOD`. The form `impl Foo for T;` is not accepted; write `{ }`.
- **Supertraits** are checked at every impl, as for ordinary traits.
- **Generics and bounds** work as for ordinary traits, conditional impls included (`impl[T: Bound] Sealed[T] for Wrapper[T] { }`).
- **Never implemented implicitly.** Every `impl Foo for T { }` is written in source. See [`CrossTask`](#crosstask) for the one compiler-computed bound.
- **Coherence and the orphan rule** apply unchanged.
- **Use as a bound**: `fn f[T: Pod](x: T)`. A missing impl is the ordinary "type does not implement trait" error.
- **No effects.** A marker trait has no methods, so it has no effects and plays no part in effect inference.
- **Compatibility.** Adding a marker trait, or an impl of one in a downstream package, is non-breaking. Removing or renaming one, or removing an impl, is breaking.

**Why a keyword.** `trait Eq { }` with no methods reads as "this trait has no methods yet". `marker trait Eq;` reads as "this trait is a tag; a method would be a category error". A marker trait cannot grow a method without becoming a different kind of trait, which is a real compatibility constraint, so it is worth stating where readers look first.

The standard library's markers are `Eq` (equality is reflexive), `Pod` (plain old data, the bound of `mem.zeroed`; [§15](#15-unsafe-ffi-and-layout-control)) and `Sealed` (closed-trait patterns inside the standard library).

### Associated types

A trait may declare **associated types**: named types fixed by the implementing type, not chosen by the caller.

```kara
trait Iterator {
    type Item;
    fn next(mut ref self) -> Option[Self.Item];
}
```

The impl binds the associated type once:

```kara
struct CountUp { current: i64, limit: i64 }

impl Iterator for CountUp {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.current >= self.limit { return None; }
        self.current += 1;
        Some(self.current - 1)
    }
}
```

**Projection.** In type position, `T.Name` is the associated type `Name` of `T`. Inside the trait it is `Self.Name`.

```kara
fn sum[I: Iterator](iter: own I) -> I.Item
where I.Item: Add[Output = I.Item]
{ ... }
```

The caller does not name `Item` at the call; it comes from `I`.

**Equality constraints.** A `where` clause may fix an associated type to a type:

```kara
fn sum_ints[I: Iterator](iter: own I)
where I.Item = i64
{ ... }
```

An equality constraint (`=`) is distinct from a bound (`:`). A `where` clause may hold one equality constraint per associated type. A bound may also name an associated type directly: `I: Iterator[Item = i64]`.

**One impl per trait.** A type implements a trait once ([Coherence and the orphan rule](#coherence-and-the-orphan-rule)), so it has one binding of each associated type:

```kara
// ERROR: CountUp already implements Iterator, with Item = i64
impl Iterator for CountUp {
    type Item = String;
    ...
}
```

This is the difference from a generic trait parameter. `trait Iterator[T]` would allow both `impl Iterator[i64] for CountUp` and `impl Iterator[String] for CountUp`. An associated type is a one-to-one relation.

**Bounds on associated types.** The trait may bound an associated type:

```kara
trait Parseable {
    type Output: Display;
    fn parse(input: String) -> Result[Self.Output, String];
}
```

The bound is checked at every impl, so any `T: Parseable` has `T.Output: Display` without repeating it.

**Standard traits with associated types:**

| Trait | Associated type | Meaning |
|---|---|---|
| `Iterator` | `Item` | The element `next` produces |
| `Iterable` | `Item` | The element `iter()` yields |
| `IntoIterator` | `Item` | The element of the iterator `into_iter()` returns |
| `Index[Idx]`, `IndexMut[Idx]` | `Output` | The element `collection[idx]` refers to |
| The [operator traits](#operator-traits) | `Output` | The result type of the operator |
| `TryFrom[T]` | `Error` | The error of a failed conversion |

`Index` and `IndexMut` keep their `Idx` parameter, because the caller chooses the index type, and fix `Output`, because the collection fixes the element type.

Generic associated types (`type Mapped[U]`) are in [deferred.md](deferred.md#unscheduled-language-extensions).

### `impl Trait`

`impl Trait` hides a concrete type from the caller while keeping monomorphized static dispatch. The bare `Trait` form is a type error in every position; the `impl` keyword is required, so each polymorphism choice shows at the type.

`impl Trait` is legal in three positions:

| Position | Form | Meaning |
|---|---|---|
| **Argument** | `fn f(x: impl Trait)` | Sugar for a generic parameter `[T: Trait]`. No existential: the caller's argument fixes the type at each call. |
| **Return** | `fn f() -> impl Trait` | An existential return type. The function returns some concrete type that implements `Trait`; the caller cannot name it. |
| **Return in a trait method** | `trait C { fn iter(self) -> impl Iterator[Item = i64]; }` | Each impl of the method returns its own hidden type, which callers cannot name. `Iterable.iter` and `IntoIterator.into_iter` use this form ([`Iterator` and `Iterable`](#iterator-and-iterable)). |

Type-alias `impl Trait` (`type X = impl Trait`) is deferred ([deferred.md](deferred.md#unscheduled-language-extensions)).

**Argument position is generic-parameter sugar.** `fn f(x: impl Trait)` is identical to `fn f[T: Trait](x: T)`: same monomorphization, same dispatch, same effect inference. Each `impl Trait` is an independent generic parameter, so `fn pair(x: impl Trait, y: impl Trait)` does not force `x` and `y` to share a type; when they must, write `[T: Trait]`. Argument-position `impl Trait` is not allowed in trait methods; use the explicit generic form there.

#### Return-position `impl Trait`

The function commits to "some concrete type that implements `Trait`", and the caller commits to using the value only through `Trait`'s methods. The concrete type is fixed at the function definition, one per monomorphization; the caller cannot match on it, downcast it or name it.

```kara
fn make_counter(start: i64) -> impl Iterator[Item = i64] {
    (start..).into_iter()       // concrete: RangeFromIter[i64]
}

let it = make_counter(0);                            // it: impl Iterator[Item = i64]
let first_three: Vec[i64] = it.take(3).collect();   // OK: Iterator methods are visible
// let r: RangeFromIter[i64] = it;                  // error: the concrete type cannot be named
```

**Inline associated-type bindings are part of the promise.** `[Item = i64]` is a two-sided commitment:
- **To the caller**, it makes the projection visible. `Iterator.next` returns `Self.Item`; on an existential there is no concrete type to project through, so without the binding `it.next()` has no type the caller can use. `Item = i64` says the hidden type's `Item` is `i64`.
- **To the definition**, it is an obligation. The concrete type's own `Item` must equal the bound type; otherwise the return site is rejected (`E_IMPL_TRAIT_ASSOC_MISMATCH`).

The binding is legal wherever a trait is named: a bound (`[I: Iterator[Item = T]]`), argument position and return position. In a bound and in argument position it is the where-clause `where I.Item = T`. In return position there is no name to constrain, so the binding rides on the existential itself. An inline binding names a type, not a type constructor. Naming an associated type the trait does not declare is an error where it is written (`E_UNKNOWN_ASSOC_TYPE_BINDING`).

**The existential is erased after type checking.** Opacity is a caller-side rule, enforced by the type checker: the concrete name cannot be written, its fields cannot be read, and a witness that does not implement the trait is refused. Because there is one concrete return per monomorphization, the compiler then substitutes the witness, and the backend sees an ordinary concrete return type. There is no runtime existential, no vtable and no dispatch machinery. An existential with two distinct witnesses is an error (`E_IMPL_TRAIT_MULTIPLE_WITNESSES`).

**Effects.** Calling the function has the function's own effects. A method call on the returned value has the effects of the hidden type's method, computed on the monomorphized instance and charged to the caller, as for a call through a trait method with no clause ([Declarations and impls](#declarations-and-impls)). So a caller that drops the value without calling a method pays nothing for its methods:

```kara
effect resource Audit;

fn audited(xs: own Vec[i64]) -> impl Iterator[Item = i64] {
    xs.into_iter().inspect(|x| record(x))   // `record` has writes(Audit)
}

fn total(xs: own Vec[i64]) -> i64 {
    let it = audited(xs);                   // no writes(Audit): nothing has run yet
    it.sum()                                // writes(Audit), through the hidden type's `next`
}
```

The return type may instead declare a ceiling with `with`. A trailing `with` after the return type is the function's own clause ([Function types and kinds](#function-types-and-kinds)), so the ceiling goes inside parentheses:

```kara
fn lines(path: Str) -> (impl Iterator[Item = String] with reads(FileSystem)) with allocates(Heap) { ... }
```

The hidden type's methods must then stay within the ceiling, and a method call on the value has the ceiling's effects, as a call through a trait method with a clause has ([§12 Trait methods and generic calls](#trait-methods-and-generic-calls)). Calling `lines` itself has `allocates(Heap)`.

**What the result borrows.** The caller cannot see into the hidden type, so it counts as a type that may hold a reference ([core-semantics.md §5.3](core-semantics.md#5-references-and-views-c5)). The signature rule decides what the result borrows, as for any view ([Returns, `ref` returns and views](#returns-ref-returns-and-views)): `self`, for a method with a `self` or `mut ref self` receiver, and otherwise every bare parameter of a non-`Copy` type, every `mut ref` parameter and every view parameter. A function with no such parameter, such as `make_counter` above, returns a value that borrows nothing.

```kara
fn count_logged[T](xs: own Vec[T], log: Logger) -> impl Iterator[Item = T] {
    log.write("starting count");
    xs.into_iter()               // the result still borrows `log`: the signature says so
}
```

A function whose result must borrow none of its parameters returns a named type with no reference in it.

### `CrossTask`

`CrossTask` is a bound that the compiler computes. It is the only such bound in the core language; `Numeric` and `GpuSafe` are deferred with the data and GPU tracks ([deferred.md](deferred.md#m4c-data), [deferred.md](deferred.md#gpu)).

- `T: CrossTask` holds unless `T` contains, at any depth, a `shared` handle or another task-local type. [§13 Concurrency](#13-concurrency) lists the task-local types.
- It cannot be implemented, derived or excluded by user code. An `impl CrossTask for T` is an error.
- It can be used as a bound like any trait: `fn forward[T: CrossTask](tx: Sender[T], v: own T)`.
- `ref T` is `CrossTask` when `T` is. Whether a view may enter a task is decided by the view rules, not by this bound: views enter `par` branches and `TaskGroup` tasks, never a channel ([§13 What may cross a task boundary](#what-may-cross-a-task-boundary)).
- The standard library's task and channel APIs state it on their generic parameters (`TaskGroup.spawn`, `channel[T]`, `Sender.send`), and `par {}` and `par for` check what their branches capture. Because the bound appears in signatures, a generic function that sends or spawns a `T` must itself require `T: CrossTask`. The error is reported at that generic signature, not after monomorphization.

**Users cannot create auto-traits.** No trait other than `CrossTask` is implemented for a type because of what the type contains. Everything else is stated at the type's definition, where a reader looks first:
- **a keyword on the type**: `shared struct` (counted, one task) and `sync struct` (shared across tasks) choose the sharing tier, and the compiler checks the keyword's rules ([§11](#11-ownership-and-sharing));
- **an explicit `marker trait` impl**: `impl Pod for T { }`;
- **an explicit derive**: `#[derive(Copy, Clone, Eq, Hash)]`, generated at the type ([Derive](#derive)).

**Why.** A property that propagates through every field and generic parameter must be answered for every type the compiler sees, and a user discovers the answer only when code fails to compile somewhere else, often with no pointer to the cause. Keeping such properties to one closed, compiler-computed bound, and making every other one explicit at the type, trades a little declaration syntax for much less mystery.

### Lang-item traits

The compiler knows a closed list of traits, called lang items. For each it knows where it is defined and which syntax uses it, and nothing more:

| Lang item | Used by |
|---|---|
| `Copy` | implicit copies ([`Copy`, `Clone` and `Drop`](#copy-clone-and-drop)) |
| `Drop` | destruction ([core-semantics.md §7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)) |
| the operator traits | `+`, `-`, `==`, `<`, `&` and the other operators ([Operator traits](#operator-traits)) |
| `Index`, `IndexMut` | `c[i]` ([`Index` and `IndexMut`](#index-and-indexmut)) |
| `Iterator`, `Iterable` | `for` loops ([`Iterator` and `Iterable`](#iterator-and-iterable)) |
| the `?` protocol | `?` ([§10 Results and propagation](#results-and-propagation)) |
| `Display` | `{}` in f-strings ([`Display` and `Debug`](#display-and-debug)) |

Everything else is ordinary Kāra code in the standard library, written over the same trait system user code uses: `Clone`, `Default`, `From` and `Into` (with a blanket impl), `IntoIterator`, `Eq`, `Ord`, `Hash`, `Debug`, `Serialize`, iterator adaptors (provided methods that return adapter structs) and every collection method. A derive produces an ordinary impl.

### Conditional impls

An impl may take generic parameters with bounds. It then applies only where the bounds hold:

```kara
impl[T: PartialEq] PartialEq for Vec[T] {
    fn eq(self, other: Vec[T]) -> bool {
        self.len() == other.len() and self.iter().zip(other.iter()).all(|(a, b)| *a == *b)
    }
}

impl[T: Eq] Eq for Vec[T] { }
impl[T: Hash] Hash for Vec[T] { ... }
impl[T: Clone] Clone for Vec[T] { ... }
impl[T: Ord] Ord for Vec[T] { ... }
```

The `[T: Bound]` syntax is the one functions use. Several bounds use `+`: `impl[T: Eq + Hash]`. Long bounds may go in a `where` clause ([§8 `where` clauses](#where-clauses)).

Inherent impls use the same brackets; only the missing `for` tells them apart:

```kara
impl[S: Scheduler] Elevator[S] {
    pub fn new(floor: i64, scheduler: own S) -> Elevator[S] {
        Elevator { floor, scheduler, stops: [] }
    }
    pub fn step(mut ref self) { ... }
}
```

**Derive adds the bounds.** `#[derive(PartialEq, Eq, Hash)]` on `Pair[T]` generates impls bounded by what the field types need. No annotation is needed:

```kara
#[derive(PartialEq, Eq, Hash)]
struct Pair[T] { first: T, second: T }
// The compiler generates:
//   impl[T: PartialEq] PartialEq for Pair[T] { ... }
//   impl[T: Eq] Eq for Pair[T] { }
//   impl[T: Hash] Hash for Pair[T] { ... }
```

Overlapping conditional impls are an error, and the orphan rule applies to them unchanged ([Coherence and the orphan rule](#coherence-and-the-orphan-rule)).

### Operator traits

Every operator is a call to a trait method. There is no separate operator table for built-in types: after type checking, `a + b` becomes `Add.add(a, b)`, for `i32`, `String` or a user type alike.

**The operator traits:**

| Category | Traits | Operator and desugaring |
|---|---|---|
| Arithmetic | `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg` | `a + b` → `Add.add(a, b)`; `-a` → `Neg.neg(a)` |
| Equality | `PartialEq` (and the marker `Eq`) | `a == b` → `PartialEq.eq(a, b)`; `a != b` → `not PartialEq.eq(a, b)` |
| Ordering | `PartialOrd` (and `Ord`) | `a < b` → `PartialOrd.partial_cmp(a, b).is_lt()`, and likewise for `<=`, `>`, `>=` |
| Bitwise | `BitAnd`, `BitOr`, `BitXor`, `Shl`, `Shr`, `Not` | `a & b` → `BitAnd.bitand(a, b)`; `~a` → `Not.not(a)` |
| Subscript | `Index`, `IndexMut` | see [`Index` and `IndexMut`](#index-and-indexmut) |

**Definitions:**

```kara
// Arithmetic (binary): owned operands, a new value as the result.
trait Add[Rhs = Self] { type Output; fn add(own self, rhs: own Rhs) -> Self.Output; }
trait Sub[Rhs = Self] { type Output; fn sub(own self, rhs: own Rhs) -> Self.Output; }
trait Mul[Rhs = Self] { type Output; fn mul(own self, rhs: own Rhs) -> Self.Output; }
trait Div[Rhs = Self] { type Output; fn div(own self, rhs: own Rhs) -> Self.Output; }
trait Rem[Rhs = Self] { type Output; fn rem(own self, rhs: own Rhs) -> Self.Output; }

// Arithmetic (unary)
trait Neg { type Output; fn neg(own self) -> Self.Output; }

// Equality: borrows both sides.
// PartialEq covers types whose equality may not be reflexive (floats: NaN != NaN).
// Eq is a marker: "my PartialEq is reflexive, symmetric and transitive on every value".
trait PartialEq[Rhs = Self] { fn eq(self, other: Rhs) -> bool; }
marker trait Eq: PartialEq;

// Ordering: borrows both sides.
// PartialOrd covers types where some pairs are incomparable (floats: NaN against anything).
// `None` from partial_cmp means "these two values are not comparable".
// Ord says the order is total; it requires Eq as well as PartialOrd.
trait PartialOrd[Rhs = Self]: PartialEq[Rhs] {
    fn partial_cmp(self, other: Rhs) -> Option[Ordering];
}
trait Ord: PartialOrd + Eq {
    fn cmp(self, other: Self) -> Ordering;
}

// Bitwise (binary): owned operands, like arithmetic.
trait BitAnd[Rhs = Self] { type Output; fn bitand(own self, rhs: own Rhs) -> Self.Output; }
trait BitOr[Rhs = Self]  { type Output; fn bitor(own self, rhs: own Rhs) -> Self.Output; }
trait BitXor[Rhs = Self] { type Output; fn bitxor(own self, rhs: own Rhs) -> Self.Output; }
trait Shl[Rhs = Self]    { type Output; fn shl(own self, rhs: own Rhs) -> Self.Output; }
trait Shr[Rhs = Self]    { type Output; fn shr(own self, rhs: own Rhs) -> Self.Output; }

// Bitwise (unary)
trait Not { type Output; fn not(own self) -> Self.Output; }
```

- **`Rhs` defaults to `Self`.** `T: Add` means `T: Add[T]`. See [§8 Default generic arguments](#default-generic-arguments).
- **`PartialEq` and `PartialOrd` take `Rhs` too**, so a comparison between two types, such as `String == Str`, can be an impl (`impl PartialEq[Str] for String`). `Eq` and `Ord` stay homogeneous: they relate a type to itself.
- **`Output` is the result type.** `a + b` has type `A.Output` for the impl `Add[B] for A`. A generic body that adds two `T`s and wants a `T` back states it: `fn total[T: Add[Output = T] + Copy](xs: Slice[T], zero: T) -> T`.
- **Arithmetic and bitwise traits take `own self` and `own Rhs`** because they produce a new value. **Comparison traits borrow both operands** (`self` and a bare `other`) because they only inspect: comparing two values never consumes them.
- **An impl may borrow where the trait takes ownership** ([Declarations and impls](#declarations-and-impls), [core-semantics.md §4.7](core-semantics.md#4-parameters-calls-and-patterns)). `String`'s impl is `fn add(self, other: String) -> String`, so `a + b` on two `String` locals moves neither ([core-semantics.md §3.1](core-semantics.md#3-moves)).

**The `Ordering` type** returned by `Ord.cmp`, and its helpers:

```kara
enum Ordering {
    Less,
    Equal,
    Greater,
}

impl Ordering {
    fn is_lt(self) -> bool { match self { Ordering.Less => true, _ => false } }
    fn is_le(self) -> bool { match self { Ordering.Greater => false, _ => true } }
    fn is_gt(self) -> bool { match self { Ordering.Greater => true, _ => false } }
    fn is_ge(self) -> bool { match self { Ordering.Less => false, _ => true } }
    fn is_eq(self) -> bool { match self { Ordering.Equal => true, _ => false } }
}

impl Option[Ordering] {
    // The same helpers for partial comparisons. `None` ("incomparable") makes every
    // predicate false. This matches IEEE 754: `NaN < 5.0`, `5.0 > NaN` and
    // `NaN == NaN` are all false.
    fn is_lt(self) -> bool { match self { Some(Ordering.Less) => true, _ => false } }
    fn is_le(self) -> bool { match self { Some(Ordering.Less) | Some(Ordering.Equal) => true, _ => false } }
    fn is_gt(self) -> bool { match self { Some(Ordering.Greater) => true, _ => false } }
    fn is_ge(self) -> bool { match self { Some(Ordering.Greater) | Some(Ordering.Equal) => true, _ => false } }
    fn is_eq(self) -> bool { match self { Some(Ordering.Equal) => true, _ => false } }
}
```

**Comparisons go through `PartialOrd`.** `a < b` is `PartialOrd.partial_cmp(a, b).is_lt()`, with both operands borrowed; `<=`, `>` and `>=` use `is_le`, `is_gt` and `is_ge`. No operator calls `Ord` directly. Generic code that needs a total order adds the `Ord` bound, but the desugaring stays the same. `==` and `!=` call `PartialEq.eq`. The desugaring never names `Eq`; it appears only as a bound in code that relies on reflexive equality (hash tables, set membership, deduplication).

**Why four traits, not two.** Floating-point values break two laws: `NaN != NaN` breaks reflexive equality, and `NaN` is incomparable with every number, which breaks total order. With only `Eq` and `Ord`, either floats cannot use `==` and `<`, or `Eq` and `Ord` lie about their contracts and generic algorithms that rely on them break. With the split, floats use `==` and `<` through `PartialEq` and `PartialOrd`, and generic code that needs totality states `Ord` and excludes floats for a reason the compiler can explain.

**`Eq` adds no method; `Ord` adds one.** `impl Eq for MyType { }` is the whole impl: it promises that the type's `PartialEq.eq` is reflexive. `Ord` adds `cmp`, which returns `Ordering` without the `Option`, because algorithms such as `sort` need the total form. Writing `impl Ord` is also a claim that `partial_cmp` never returns `None`.

**Compound assignment.** `a += b` means `a = a + b`, with the place `a` evaluated once ([core-semantics.md §8.2](core-semantics.md#8-evaluation-order)). There is no separate `AddAssign` trait, so the impl's `Output` must be the type of `a`: `total_i64 += n_i32` is legal, and `n_i32 += total_i64` is an error ([§5 Mixed-width operands](#mixed-width-operands)). A dedicated `AddAssign` may come later, if in-place updates of large owned values need to avoid the intermediate value. Through a `mut ref` binding, `x += 1` writes through the reference ([§7](#-and-ref-in-expressions)).

**Mixed numeric types.** The integer primitives implement `Shl[R]` and `Shr[R]` for every integer type `R`, with the left operand's type as `Output`; every other operator trait on a numeric primitive has only a `Self`-typed impl. The widening rule of [§5 Mixed-width operands](#mixed-width-operands) is a coercion applied before operator dispatch, on primitive operands only: when one operand converts losslessly to the other's type, it widens, and that type's `Self`-typed impl is called. It widens within a kind only, so `a_i32 + b_f64` needs `as`. Shifts need no widening, because their impls take any integer type on the right ([§5 Division, remainder and shifts](#division-remainder-and-shifts)). Generic code never widens: an operator on a `T: Add` value calls the impl its bound names, with no coercion.

**Standard impls:**
- The arithmetic traits on every numeric primitive, with `Rhs = Self`.
- `Add` on `String`: `fn add(self, other: String) -> String`, with `allocates(Heap)`. Both operands are borrowed, and the result is a new `String`.
- `PartialEq` on every primitive (integers, `bool`, `char`, floats), on `String` and `Str`, and through `Vec`, `Option`, `Result` and tuples when their element types are `PartialEq`.
- `Eq` on every primitive except `f32` and `f64`, on both string types, and through `Vec`, `Option`, `Result` and tuples when their element types are `Eq`. `f32` and `f64` are `PartialEq` without `Eq`, because `NaN != NaN`.
- `PartialOrd` on every primitive (floats included), both string types, and through `Vec`, `Option`, `Result` and tuples.
- `Ord` on the same set minus `f32` and `f64`. The total-order types `F32` and `F64` ([§5 Floats](#floats)) implement `Eq`, `Ord` and `Hash`.
- `BitAnd`, `BitOr`, `BitXor`, `Shl` and `Shr` on the integer primitives only. `bool` has no `&`, `|` or `^`: `a & b` on two `bool`s is "bitwise operator requires integer type, found 'bool'". Use `and` and `or`, which are separate forms and not trait calls.
- `Not` on the integer primitives and `bool`, reached through two operators. `~a` is the bitwise complement and takes integers only. `not a` is logical negation and takes `bool` only. `not (a: i64)` and `~(b: bool)` are both type errors.

**There is no `impl Add for Vec[T]`.** `vec1 + vec2` is an error whose diagnostic names the method to use: "type 'Vec[i64]' does not implement trait Add; there is deliberately no `impl Add for Vec[T]`; use `a.extend(b)` to append b's elements to a". Concatenation and elementwise addition are both plausible meanings, which is why the language asks for a method name. `extend` works in place and needs a `mut` receiver, so code that wants a new `Vec` declares a `mut` binding and extends it. `Vec.concat()` is a different operation: it joins a `Vec[String]` into one `String`.

**User impls are allowed**, for every operator trait, `Index` and `IndexMut` included:

```kara
impl Mul[i64] for Duration {
    type Output = Duration;
    fn mul(self, rhs: i64) -> Duration { ... }
}

impl Sub for Instant {
    type Output = Duration;
    fn sub(self, rhs: Instant) -> Duration { ... }
}
```

**Distinct types** can derive the comparison traits. `#[derive(PartialEq, Eq, PartialOrd, Ord)]` on `distinct type UserId = u64` compares the underlying values, so `user1 < user2` works. Arithmetic on a distinct type is opt-in with `#[derive(Arithmetic)]`. It provides `+`, `-`, `*`, `/`, `%` and unary `-` between two values of the **same** distinct type (`FloorNum + FloorNum -> FloorNum`). Arithmetic with another type (`FloorNum + UserId`, `FloorNum + i64`) stays an error, which is the point of a distinct type. `Arithmetic` suits numeric domain types such as floor numbers, pixel coordinates, durations and money, where two quantities of one unit can be combined:

```kara
#[derive(PartialEq, Eq, PartialOrd, Ord, Arithmetic)]
distinct type FloorNum = i64;

let a = FloorNum(3);
let b = FloorNum(1);
let c = a + b;          // OK: FloorNum + FloorNum -> FloorNum
let d = a + 1;          // ERROR: FloorNum + i64 is not defined
```

Without the derive, unwrap explicitly: `FloorNum(a.raw() + 1)`. That is the right default for IDs, keys, tokens and anything where adding two values means nothing.

**Desugaring happens after type checking.** The parser produces a binary-operator node. Once the operand types are known, the node becomes a call to the trait method, with the span kept. So a missing impl is reported in trait terms ("type Vec[T] does not implement trait Add"), not operator terms. `karac explain --concept=operators` shows the whole desugaring table.

### `Index` and `IndexMut`

`[]` is defined by two traits:

```kara
trait Index[Idx] {
    type Output;
    fn index(self, idx: Idx) -> ref Self.Output;
}

trait IndexMut[Idx] {
    type Output;
    fn index_mut(mut ref self, idx: Idx) -> mut ref Self.Output;
}
```

| Syntax | Meaning |
|---|---|
| `c[key]` | the place `*Index.index(c, key)`, with `c` borrowed |
| `c[i, j, k]` | `c[(i, j, k)]`: tuple-index sugar |
| `c[key] = value` | `*IndexMut.index_mut(c, key) = value`, with `c` borrowed mutably |
| `c[i, j, k] = value` | `*IndexMut.index_mut(c, (i, j, k)) = value` |

User types may implement both, so a user container supports `[]`.

**An index expression is a place, not a value.** `c[i]` refers to an element that `c` still owns.
- **Reading it as a value copies it**, so `let t = v[i];` needs the element type to be `Copy`. For a non-`Copy` element it would move the element out of the container, which is error `E_INDEX_MOVE_NON_COPY` ([core-semantics.md §3.7](core-semantics.md#3-moves)). A container has no representation for a hole, so an element cannot be moved out of a live one.
- **The same holds for `v[i] = v[j]`** on a non-`Copy` element: the right-hand side would move out of the container.
- **Operators that borrow their operands**, such as `==` and `<`, borrow `v[i]` and need no copy. Methods with `self` or `mut ref self` receivers work on the element in place.
- **`v[i] = value` with a new value** (a literal, a call result) replaces the element: the old element is dropped in place, then the new one is stored ([core-semantics.md §7.5](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).

The diagnostic offers three fixes:

| Intent | Spelling | Cost |
|---|---|---|
| Read it without owning it | `let t = ref v[i];` | none: a borrow ([§7](#-and-ref-in-expressions)) |
| Take an independent copy | `let t = v[i].clone();` | needs `T: Clone`; the allocation is visible at the call |
| Exchange two elements | `v.swap(i, j)` | none: no value dies, so no `Drop` body runs |

```
error[E_INDEX_MOVE_NON_COPY]: cannot move out of an index expression
   --> heap.kara:128:17
    |
128 |         let t = self.xs[i];
    |                 ^^^^^^^^^^ `Item` does not implement `Copy`
    |
    = note: `v[i]` refers to an element of `self.xs`; binding it by value would move the
            element out, leaving a hole the container cannot represent
help: borrow the element instead
    |         let t = ref self.xs[i];
help: or take an independent copy
    |         let t = self.xs[i].clone();
help: to exchange two elements, use `swap`
    |         self.xs.swap(i, j);
```

`mem.take`, `mem.replace` and `mem.swap` also work on an index place ([core-semantics.md §3.7](core-semantics.md#3-moves)).

**Out of bounds panics.** `index` and `index_mut` return a reference, not a `Result`. An invalid index panics, which adds `panics` to the caller's effects. Where an index may be invalid, use `.get(idx)`, which returns `Option[ref Output]`:

```kara
let n = counts[i];             // counts: Vec[i64]; panics if i is out of bounds
let item = items.get(i)?;      // Option[ref Item]; None if i is out of bounds
```

**Range indexing.** `c[a..b]` is a separate impl, `impl Index[Range[i64]]` with `type Output = Slice[T]`, provided by `Vec[T]`, `Array[T, N]` and `Slice[T]`. Mutable range indexing (`impl IndexMut[Range[i64]]`) also has `type Output = Slice[T]`. `c[a..b] = other` copies the elements of `other` into the range. It requires `T: Copy`, and a length mismatch panics. For other element types, use `clone_from_slice`. For strings, `s[a..b]` yields a `Str` ([library/strings.md](library/strings.md)).

### Conversion traits

`From`, `Into`, `TryFrom` and `TryInto` are the standard conversion traits.

**Infallible conversions:**

```kara
trait From[T] {
    fn from(value: own T) -> Self;
}

trait Into[T] {
    fn into(own self) -> T;
}
```

`Into` is never implemented directly. A blanket impl in the standard library derives it from every `From` impl:

```kara
impl[T, U: From[T]] Into[U] for T {
    fn into(own self) -> U { U.from(self) }
}
```

So: **implement `From`, and `Into` comes for free.** A direct `impl Into` would overlap the blanket impl, which is an error ([Coherence and the orphan rule](#coherence-and-the-orphan-rule)).

**Choosing among several impls.** A type may implement both `From[A]` and `From[B]`. `T.from(x)` picks the impl by the type of `x`; `x.into()` picks it by the expected type. When neither decides, the call is an error that asks for an annotation.

**Effects pass through the blanket impl.** Each use of `into` is a generic call, and its effects are those of the instance ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)). So `x.into()` has exactly the effects of the `impl From[T] for U` that inference selects at that call, and no others:

```kara
effect resource Log;

impl From[ParseError] for AppError {
    fn from(e: own ParseError) -> AppError with writes(Log) {
        log_error(e);           // `log_error` borrows `e`
        AppError.Parse(e)
    }
}

fn handler(e: own ParseError) -> AppError with writes(Log) {
    e.into()    // writes(Log), through the blanket Into impl
}
```

**Fallible conversions:**

```kara
trait TryFrom[T] {
    type Error;
    fn try_from(value: own T) -> Result[Self, Self.Error];
}

trait TryInto[T] {
    type Error;
    fn try_into(own self) -> Result[T, Self.Error];
}
```

The same blanket rule applies: implement `TryFrom`, and `TryInto` comes for free. Effects pass through it in the same way.

**`?` converts errors with `From`.** When a function returning `Result[T, MyError]` applies `?` to a `Result[U, OtherError]`, the error is converted with `MyError.from(e)`, which needs `impl From[OtherError] for MyError`. See [§10 Results and propagation](#results-and-propagation). Every `Error` type converts into `AnyError` this way; `AnyError` itself does not implement `Error` ([§10 `Error` and `AnyError`](#error-and-anyerror)).

**Common standard impls:**

| `impl` | Meaning |
|---|---|
| `impl From[i32] for i64` | Widening numeric conversion; infallible |
| `impl TryFrom[i64] for i32` | Narrowing numeric conversion; fails when out of range |
| `impl From[Str] for String` | A `String` from a string slice or literal |
| `impl From[T] for Option[T]` | Wrap a value in `Some` |
| `impl From[T] for Result[T, E]` | Wrap a value in `Ok` |

Parsing (`T.parse(s)`, which returns a `Result`) is in [library/strings.md](library/strings.md).

### `Copy`, `Clone` and `Drop`

Which values are copied, moved and dropped, and when, is decided in [core-semantics.md §1](core-semantics.md#1-values), [§3](core-semantics.md#3-moves) and [§7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0). This section covers the three traits as they appear in source.

#### `Copy`

A `Copy` value is duplicated by a bitwise copy: it owns no heap memory and has no destructor. Where a move-only value would move, a `Copy` value is copied, and the source stays valid. No annotation is needed, and there is no `copy(x)` syntax.

```kara
fn double(x: i64) -> i64 { x * 2 }

let n = 42;
let result = double(n);   // n is copied: i64 is Copy
println(n);               // OK: n is still valid
```

The `Copy` types are listed in [core-semantics.md §1.1](core-semantics.md#1-values): the primitives; `Array[T, N]`, tuples and `Option[T]` when their parts are `Copy`; `ref T` but not `mut ref T`; an `Fn` closure whose captures are all `Copy` ([§7 Function types and kinds](#function-types-and-kinds)); and user types that derive `Copy`. `String`, `Vec[T]` and every type that owns heap memory are not `Copy`.

- **`#[derive(Copy)]` is the only way to make a user type `Copy`.** A manual `impl Copy` is not allowed, so the field check always runs. The derive is rejected if any field is not `Copy`:
  ```kara
  #[derive(Copy, Clone)]
  struct Point { x: f64, y: f64 }          // OK: both fields are Copy

  #[derive(Copy, Clone)]                   // ERROR: name is a String, which is not Copy
  struct User { id: u64, name: String }
  ```
- **A fieldless enum is not `Copy` unless it derives it.** Otherwise adding a payload variant later would silently break the enum's users.
- **A `distinct type` does not inherit `Copy`.** `distinct type UserId = u64` is not `Copy` although `u64` is. A distinct type exists to stop accidental use as its base type, and inheriting `Copy` silently would undermine that. Opt in explicitly:
  ```kara
  distinct type SessionToken = u64;        // not Copy: duplicating it is a visible .clone()

  #[derive(Copy, Clone)]
  distinct type Offset = i64;              // Copy, by explicit choice
  ```
- **`Copy` implies `Clone`.** `Clone` is a supertrait of `Copy` (`trait Copy: Clone`), so `T: Copy` satisfies a `T: Clone` bound. `#[derive(Copy)]` without `Clone` is an error whose fix adds `Clone` ([Derive](#derive)). Derive only `Clone` for types that can be cloned but not copied bit for bit.
- **`Copy` and `Drop` are exclusive** (below).

#### `Clone`

`Clone` is how a move-only value is duplicated. The compiler never inserts a clone; the programmer writes `.clone()` ([core-semantics.md §1.3](core-semantics.md#1-values)). Generic code that must keep one copy while handing another on writes `T: Clone`.

```kara
trait Clone {
    fn clone(self) -> Self;
}
```

**The signature is fixed.** The receiver is borrowed because cloning must never consume the original. The result is owned because the caller wants a new, independent value. There is no variant for expensive, fallible or parameterised clones. A type that needs one provides a separate method (`try_clone`, `clone_with_capacity`) outside `Clone`.

**`Clone` cannot fail.** `clone` returns `Self`, not a `Result`. A fallible core trait would put an error type into every generic signature that clones, and `#[derive(Clone)]` would have to name one. Allocation failure panics, as it does for `allocates(Heap)` everywhere ([core-semantics.md §10.1](core-semantics.md#10-panics-and-errors-c8)). A type whose duplication is really fallible, such as a handle that needs a system call to duplicate, provides `fn try_clone(self) -> Result[Self, IoError]` and does not implement `Clone`.

**Each type decides how deep a clone goes:**
- **Owned value types** (`String`, `Vec[T]`, structs with heap-owned fields): `.clone()` is deep. It makes a new allocation and clones the children. Changing the clone does not change the original.
- **`shared` handles** are duplicated by counting, not by `.clone()`: using a handle as a value increments its count, and both handles reach the same object ([core-semantics.md §6.1](core-semantics.md#6-sharing-c7)). Cloning a value that contains a handle copies the handle.
- **References.** `ref T` is `Copy`, so it satisfies a `Clone` bound, and cloning it through that bound copies the reference. Method-call syntax on a `ref T` value finds `T`'s methods ([Method resolution](#method-resolution)), so `r.clone()` clones the referent and returns a `T`.
- **Primitives** clone by the bitwise copy `Copy` already provides.

A derived `Clone` calls `.clone()` on each field and trusts each field's type. A struct holding a `Vec[String]` and a `shared` `Config` handle clones into a new `Vec` with each `String` cloned, and a second handle to the same `Config`. Deep or shallow is a property of the leaf types, not of the composition.

**Derive.** `#[derive(Clone)]` clones each field in declaration order and builds a new value. For an enum, every variant's fields must be `Clone`. The bound propagates: deriving `Clone` on `Pair[T]` generates `impl[T: Clone] Clone for Pair[T]`. Manual impls are allowed where field-by-field cloning is not what the type wants (a type that rebuilds an internal index, say), but should be rare.

#### `Drop`

A type that owns a resource that must be released deterministically (a file descriptor, a connection, a transaction, a buffer owned through FFI) implements `Drop`. The compiler calls `drop` when the value is destroyed, at the points [core-semantics.md §7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0) fixes.

```kara
struct File { fd: i32 }

impl Drop for File {
    fn drop(mut ref self) {
        host.close(self.fd);
    }
}

fn read_config() -> Result[Config, AnyError] {
    let f = File.open("config.toml")?;   // f owns the fd
    Ok(parse(f.bytes())?)
    // f's scope ends here: drop runs, and host.close closes the fd
}
```

**The signature is fixed: `fn drop(mut ref self)`.** `drop` may change `self` (close a handle, flush a buffer) but does not consume it: the compiler calls it on storage the scope already owns. `fn drop(own self)` would hand ownership to the body, and there is nobody to hand it back to. A type that wants a consuming cleanup that can report errors provides `fn close(own self) -> Result[(), IoError]` for callers to call explicitly. `drop` is the fallback that runs when `close` was not called.

**`Drop` and `Copy` are exclusive.** A type with a `Drop` impl cannot be `Copy`, and a `Copy` type cannot implement `Drop`. A bitwise copy whose destructor runs on every copy either releases the resource twice or is not really a copy. The compiler rejects the combination at the impl or derive, naming both traits. This holds however `Copy` would arise.

**A `Drop` body may panic.** The panic ends the process like any other, and `panics` is default-permitted ([core-semantics.md §7.11](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0), [§12 Default-permitted effects](#default-permitted-effects)). `drop` returns nothing, so cleanup that can fail handles the error in place, for example by matching on the result of `host.close` and logging an `Err`; it cannot propagate it.

**Effects.** A drop's effects are those of the type's drop glue, charged to the scope where the drop happens ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)).

**`Drop` is not derivable.** "Do nothing" is what having no impl means, and "drop each field" is what the compiler does after any `drop` body. A derived body would be empty or would repeat built-in behaviour.

**Order.** The type's own `drop` body runs first, with every field still alive. Then the fields drop, in reverse declaration order. For an enum, the active variant's payload drops after the body. See [core-semantics.md §7.8](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0).

```kara
struct Transaction {
    conn: Connection,       // dropped second (closes the connection)
    log: TransactionLog,    // dropped first (flushes pending writes)
}

impl Drop for Transaction {
    fn drop(mut ref self) {
        self.log.flush();               // the body runs first, with both fields alive
        self.conn.rollback_if_open();
    }
    // after the body returns:
    //   1. self.log drops
    //   2. self.conn drops
}
```

**`Drop` and `defer` compose.** `Drop` is attached to a type: every value of the type runs it, and the user writes no cleanup statement. `defer` and `errdefer` are attached to a scope: they run whatever the types in the scope, for cleanup that does not deserve a wrapper type (a log line, a flag reset). Both are popped from the same last-in, first-out order at scope end ([core-semantics.md §7.3](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0), [§6 `defer` and `errdefer`](#defer-and-errdefer)). Rule of thumb: if the cleanup belongs to the type, write a `Drop` impl; if it belongs to this piece of code, write `defer`.

**No part of a `Drop` value can be moved out.** The `drop` body assumes every field is present, so moving a field or payload out of a value whose type has a `Drop` body is an error, partial or whole ([core-semantics.md §3.6 and §3.7](core-semantics.md#3-moves)). The fixes are `.clone()`, `mem.take`, `mem.replace` and `mem.swap`. So:
- a full destructure, `let H { r, n } = h;`, is rejected too;
- reading a `Copy` field (`let q = h.n;`) copies it and is allowed;
- a pattern over a borrowed `Drop` value binds `ref`s and moves nothing, so it is allowed ([core-semantics.md §4.6](core-semantics.md#4-parameters-calls-and-patterns)).

A type without a `Drop` body may have fields moved out of an owned value. The fields not moved drop at the end of the value's scope ([core-semantics.md §3.6](core-semantics.md#3-moves)).

#### Destructors are not soundness mechanisms

**A `Drop` impl that does not run must never cause memory unsafety, undefined behaviour or a broken type-system invariant.** Drop is cleanup: if it runs, it releases the resource; if it does not, the resource leaks and the program stays sound. No standard library type, language feature or generic API may rely on a destructor running. The one exception is a `TaskGroup` that borrows, whose drop safe code cannot skip (below).

The lesson comes from other languages, where APIs that relied on destructors for soundness had to be redesigned once it was clear that destructors can be skipped. Kāra adopts it up front: every safety invariant is enforced statically, by the type system and the borrow rules, not by the promise that a destructor will run.

| Invariant | Naive (broken) approach | What actually enforces it |
|---|---|---|
| A reference cannot outlive what it borrows | "the source is released later, by `Drop`" | References are decided by signatures, and views cannot escape, checked statically ([core-semantics.md §5](core-semantics.md#5-references-and-views-c5)). |
| A task-local value never crosses a task boundary | "`Drop` cleans up after it crosses" | The [`CrossTask`](#crosstask) bound, checked during type checking. |

A drop may also release a resource in each case, but the invariant holds without it. If a drop is skipped, because a panic ends the process without running drops ([core-semantics.md §10.2](core-semantics.md#10-panics-and-errors-c8)), because a cycle of `shared` handles is never freed ([core-semantics.md §6.5](core-semantics.md#6-sharing-c7)), or because the process exits, the resource leaks and nothing becomes unsound.

**Consequences:**
- **Skipping a destructor needs `unsafe`.** Kāra has no safe operation that skips one; in safe code a value leaks only when a cycle of `shared` handles is never freed. A `TaskGroup` that borrows relies on its drop to join its tasks before the places they borrow die ([core-semantics.md §9.5](core-semantics.md#9-closures)). It is a view, so it cannot be stored where such a cycle could leak it ([core-semantics.md §5.7](core-semantics.md#5-references-and-views-c5)).
- **Standard library design.** Every other standard library type with a `Drop` impl releases resources that are desirable, not required for soundness, to release. The review question for a new type is "if `drop` is removed, what becomes unsafe?", and the answer must be "nothing; only resources leak".
- **`defer` and `errdefer`** follow the same rule. A `defer { close(fd) }` that does not run leaks the descriptor and breaks nothing. Like drops, they do not run on a panic.

**Soundness lives in the type system; cleanup lives in the runtime.** The type system guarantees that cleanup runs on every normal path, and the runtime runs it, but the type system's soundness never depends on it running.

### `Eq`, `Ord` and `Hash`

`PartialEq`, `Eq`, `PartialOrd` and `Ord` are defined with the [operator traits](#operator-traits). This section covers hashing.

`Hash` is the bound every hash-keyed structure names: `Map[K: Hash + Eq, V]`, `Set[T: Hash + Eq]`, memoization caches, content-addressed stores. Hashing is split into two traits. `Hash` says how to feed a value to a hasher; `Hasher` says how to absorb bytes and produce a digest. With the split, a struct's hash is composed from its fields' without each field choosing a width and a mixing function.

```kara
trait Hasher {
    fn write(mut ref self, bytes: Slice[u8]);
    fn finish(self) -> u64;

    // Convenience methods with default bodies that feed bytes through `write`:
    fn write_u8(mut ref self, n: u8)    { self.write([n]); }
    fn write_u16(mut ref self, n: u16)  { self.write(n.to_ne_bytes()); }
    fn write_u32(mut ref self, n: u32)  { self.write(n.to_ne_bytes()); }
    fn write_u64(mut ref self, n: u64)  { self.write(n.to_ne_bytes()); }
    // ...and the signed variants
}

trait Hash {
    fn hash[H: Hasher](self, hasher: mut ref H);
}
```

**Why a hasher parameter instead of `fn hash(self) -> u64`.** A `-> u64` method fixes the digest width, makes every composite type invent its own mixing (`h1 ^ h2.rotate_left(13)` does not compose), and takes the choice of algorithm away from the hash table, where it belongs. With the parameter, `Map[K, V, FxBuildHasher]` and `Map[K, V, SipHash13BuildHasher]` share the same key impls; the key does not know which hasher is in use.

**Hash values are not stable.** The default hasher of `Map` and `Set` is resistant to hash-flooding and seeded per process, so a value's hash differs between runs, Kāra versions and targets. Code that needs stable digests (content addressing, on-disk indexes, sharding) uses the `StableHash` functions instead of `Hash`. Hashers, their selection and `StableHash` are in [library/collections.md](library/collections.md).

**The consistency contract.** Every `Hash` impl must satisfy: if `a == b`, then hashing `a` and hashing `b` feed identical bytes to any hasher. The contract is documented, not checked, since checking it would mean reasoning about arbitrary code. Breaking it is a logic bug (lookups miss keys that are present), not a safety bug.

**Derive.** `#[derive(Hash)]` on a struct feeds each field to the hasher in declaration order. On an enum it feeds the discriminant, then the active variant's fields in declaration order. Every field type must be `Hash`. The derive generates ordinary code, `self.a.hash(mut hasher); self.b.hash(mut hasher);`, so a derived impl and a hand-written one behave alike.
- **`derive(Hash)` does not require or add `PartialEq` or `Eq`.** A type may be hashable without being comparable.
- **A `Map` key or `Set` element needs `Hash` and `Eq` together.** For a key type that lacks one, the diagnostic names the missing trait.
- A manual `impl Hash` is allowed but should be rare. It must keep the consistency contract with the type's `PartialEq`.

### `Display` and `Debug`

Two traits turn values into text. `Display` is the user-facing form; `Debug` is the programmer-facing form.

```kara
trait Display {
    fn to_string(self) -> String;
}

trait Debug {
    fn fmt_debug(self) -> String;
}
```

**`Display`** is a lang item. `{expr}` in an f-string calls `expr.to_string()`, and `println(x)` renders `x` through `Display` ([§3 Interpolated strings](#interpolated-strings)). The receiver is borrowed, so one value may appear in several `{}` slots. The primitives, both string types, the collections, `Option`, `Result` and tuples implement `Display` when their elements do.

**`Debug`** is an ordinary trait. It is used where prose is the wrong form: `dbg(x)`, `assert_eq` failure messages, and diagnostic dumps. It formats a value as a programmer reading a debugger expects:
- strings in quotes (`"hello"`, not `hello`);
- `None` as `None`, not as an empty string;
- struct and enum fields with their names (`Point { x: 1.0, y: 2.0 }`);
- collections with their brackets.

**Two traits, no fallback.** `Debug` is a separate trait, not a mode of `Display`. Where `Debug` is needed and missing (`dbg`, `assert_eq`), the error names the missing trait and suggests `#[derive(Debug)]`. The compiler never falls back to `Display`: that would leak user-facing prose into debug output, or the reverse, which is the bug the split exists to prevent. Format specifiers, including the reserved `Debug` slot `{x:?}`, are in [§3 Interpolated strings](#interpolated-strings).

**A value renders the same at every depth.** A hand-written `impl Display` is what the value renders as wherever it appears: on its own (`f"{e}"`), as a collection element (`f"{[e]}"` gives `[aye 7]`), as the payload of `Some` or `Ok` (`f"{Some(e)}"` gives `Some(aye 7)`), as a tuple component, and as a field of a containing struct. A container contributes only its punctuation (`[`, `, `, `]`, `{k: v}`); each element renders through its own `Display`. Since `println(e)` uses `Display`, wrapping `e` must not change how it renders. Otherwise a `Display` written to hide a type's internals would leak them from inside any `Vec`. This asks nothing new of the element: a container is `Display` exactly when its elements are, and `f"{v}"` on a `Vec[P]` with no `impl Display for P` is already rejected. `Debug` keeps the field-name form at every depth: `dbg(v)` on the same vector shows `[A { n: 7 }, B]`.

**Derived `Display`:**
- **A struct** renders as `TypeName { field: value, … }`, with all its fields in declaration order, each through its own `Display`.
- **An enum's unit variant** renders as its name. A variant with data renders as the name followed by its fields in parentheses, each through its own `Display`:
  ```kara
  #[derive(Display)]
  enum Direction { Up, Down, Idle }

  #[derive(Display)]
  enum Shape {
      Circle(f64),
      Rect(f64, f64),
      Point,
  }

  println(f"{Direction.Up}");     // Up
  // Shape.Circle(2.5)    renders as "Circle(2.5)"
  // Shape.Rect(3.0, 4.0) renders as "Rect(3.0, 4.0)"
  // Shape.Point          renders as "Point"
  ```
- **Casing.** `#[derive(Display(snake_case))]` writes variant names in `snake_case` (`Direction.Up` gives `"up"`), for enums that follow CLI or JSON conventions.
- **Custom formatting.** Implement `Display` by hand to replace the derived form entirely.

**Derived `Debug`** formats a struct as `TypeName { field1: ..., field2: ... }` and an enum variant as `Variant(...)` or `Variant { ... }`, each field through its own `Debug`. The bound propagates (`impl[T: Debug] Debug for Pair[T]`). `Debug` is never derived implicitly, since deriving it has visible consequences (bounds on generics, code size, API compatibility). The compiler suggests `#[derive(Debug)]` in the diagnostic when a type without it reaches `dbg` or `assert_eq`.

### `Iterator` and `Iterable`

Two lang-item traits drive `for`: `Iterator` and `Iterable`. A third, `IntoIterator`, is an ordinary library trait for consuming a collection.

```kara
trait Iterable {
    type Item;
    fn iter(self) -> impl Iterator[Item = Self.Item];
}

trait Iterator {
    type Item;
    fn next(mut ref self) -> Option[Self.Item];
}

trait IntoIterator {
    type Item;
    fn into_iter(own self) -> impl Iterator[Item = Self.Item];
}
```

- **`Iterator`** produces items one at a time with `next`. Its adaptors (`map`, `filter`, `zip` and so on) are provided methods that return adapter structs. They are listed in [library/iterators.md](library/iterators.md).
- **`Iterable`** is a collection that can be iterated by borrowing it: `iter` takes a borrowed `self`.
- **`IntoIterator`** is for consuming iteration: `into_iter` takes `own self`. `for` never calls it.

**What `for` does** depends on the type of the expression after `in` ([core-semantics.md §4.6](core-semantics.md#4-parameters-calls-and-patterns)):
- **An `Iterator` is consumed.** `for x in it` moves `it` and calls `next` until it returns `None`. `for x in c.into_iter()` and `for x in v.iter().map(f)` work this way.
- **Anything else is borrowed through `Iterable`.** `for x in c` needs `c: Iterable` and means:

```kara
let mut it = c.iter();          // borrows c
while let Some(x) = it.next() {
    ...
}
```

A collection is borrowed; `for` never consumes it. The item is a `ref` into the collection for the standard collections. It cannot be moved out or written through ([core-semantics.md §3.7](core-semantics.md#3-moves), [§5.9](core-semantics.md#5-references-and-views-c5)). To keep an owned copy of an item, write `.clone()`. To take the items, consume the collection with `.into_iter()`.

**An `Iterator` is not `Iterable`.** There is no blanket `impl[I: Iterator] Iterable for I`. To loop over an iterator without consuming it, write `for x in it.by_ref()`; `it` is still usable after the loop. There is no separate `Stream` trait for sources that can be read only once.

**Consuming iteration is explicit.** `for x in c.into_iter()` moves `c` and each item ([core-semantics.md §3.1](core-semantics.md#3-moves)). A source that can be read only once, such as a network stream or a channel receiver, implements `IntoIterator` but not `Iterable`, since it has nothing to lend. It is iterated with an explicit call:

```kara
for event in stream.into_iter() { process(event); }   // consuming
for item in my_vec { observe(item); }                 // borrowing: Vec is Iterable
```

A `for` over a type that is neither an `Iterator` nor `Iterable` is an error naming the missing impl. When the type implements `IntoIterator`, the diagnostic suggests `.into_iter()`.

**Destructuring in `for`.** `for (key, value) in map` uses ordinary tuple destructuring. Because `for` borrows a collection, `key` and `value` are `ref K` and `ref V`. To consume the map and get owned pairs, use `.into_iter()`:

```kara
// Borrowing: key is ref String, value is ref Value
for (key, value) in map {
    println(f"{key}: {value}");
}

// Consuming: key is String, value is Value, and map is moved
for (key, value) in map.into_iter() {
    owned_keys.push(key);       // no clone needed
    owned_values.push(value);
}
```

The same holds for `Vec`, `Set` and every other collection: bare `for` borrows, `.into_iter()` consumes. `Map` and `Set` iterate in an order that is unspecified and differs between runs ([library/collections.md](library/collections.md)).

**Loop effects are static.** A loop body adds its effects to the enclosing function once. Effects are a property of the code, not counted per iteration: a `while let` over a network stream has the same effects as one read.

### Derive

`#[derive(...)]` generates trait impls from a type's fields. It is built into the compiler, and each derive produces an ordinary impl, as if written by hand. Deriving is always explicit: not every struct should be comparable or hashable, and the programmer states which are.

```kara
#[derive(PartialEq, Eq, Hash, Display)]
struct Point { x: i64, y: i64 }
// Generates: PartialEq (field-by-field ==), Eq (marker), Hash (field-by-field hashing)
// and Display (a formatted string)
```

**The derivable traits:**

| Trait | Generated impl | Requires |
|---|---|---|
| `PartialEq` | `==` field by field | every field `PartialEq` |
| `Eq` | empty marker impl | `PartialEq` |
| `PartialOrd` | lexicographic comparison in field order | `PartialEq` |
| `Ord` | lexicographic comparison in field order | `PartialOrd` and `Eq` |
| `Hash` | each field fed to the hasher in order ([`Eq`, `Ord` and `Hash`](#eq-ord-and-hash)) | every field `Hash` |
| `Clone` | `.clone()` of each field ([`Clone`](#clone)) | every field `Clone` |
| `Copy` | marker impl ([`Copy`](#copy)) | `Clone`, and every field `Copy` |
| `Default` | see below | every field `Default` (structs) |
| `Display`, `Debug` | see [`Display` and `Debug`](#display-and-debug) | every field `Display` / `Debug` |
| `Arithmetic` | `+`, `-`, `*`, `/`, `%` and unary `-` on one distinct type ([Operator traits](#operator-traits)) | distinct types only |
| `Serialize`, `Deserialize` | field traversal (below) | every field `Serialize` / `Deserialize` |

**A derive never adds another.** A derived impl needs its supertraits implemented, like any impl ([Supertraits](#supertraits)). A missing one is an error with a machine-applicable fix that adds the derive: `#[derive(Copy)]` without `Clone`, or `#[derive(Ord)]` without `PartialOrd`, `Eq` or `PartialEq`. The order of the list does not matter. The chains are:
- `Eq` needs `PartialEq`;
- `PartialOrd` needs `PartialEq`;
- `Ord` needs `PartialOrd` and `Eq`;
- `Copy` needs `Clone`, and every field `Copy`;
- `Hash` and `Default` need no other derive.

**Bounds propagate.** On a generic type, each derived impl is bounded by what its fields need ([Conditional impls](#conditional-impls)).

**A derive and a manual impl of the same trait** on one type is the ordinary duplicate-impl error. Remove one; the compiler does not prefer either.

#### `Default` and `#[default]`

`Default` provides `fn default() -> Self`, the starting value of a type. Code calls it as `T.default()` ([Associated functions](#associated-functions)), or through features that rely on it, such as `#[serde(default)]`.

**On a struct**, `#[derive(Default)]` builds the struct from each field type's `default()`:

```kara
#[derive(Default)]
struct Point { x: i64, y: i64 }
// Generates: fn default() -> Self { Point { x: i64.default(), y: i64.default() } }
//            which is Point { x: 0, y: 0 }
```

Every field type must implement `Default`. Otherwise the derive is rejected at the type with `E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT` ("cannot #[derive(Default)] for 'Point': field 'name' of type 'T' does not implement 'Default'"), and the field is labelled. On `Pair[T]` the derive generates `impl[T: Default] Default for Pair[T]`.

**On an enum**, exactly one variant must be marked `#[default]`, and it must be a unit variant:

```kara
#[derive(Default)]
enum Status {
    #[default]
    Pending,
    Active,
    Closed,
}
// Generates: fn default() -> Self { Status.Pending }
```

**Why unit variants only.** A default that walks into a payload variant and builds each field (`Status.Pending { since: Time.default(), reason: String.default() }`) hides a non-trivial starting state: a reviewer must run every field's `default()` in their head, and the chain grows as field types derive `Default` too. A plain sentinel such as `Status.Pending` is what defaults are for. A payload default is two lines by hand: `impl Default for Status { fn default() -> Self { Status.Pending { since: Time.epoch(), reason: String.new() } } }`. Lifting the restriction later would not break any program.

**Diagnostics for the enum derive**, all reported at the type:
- **No variant marked**: `E_DEFAULT_NO_VARIANT_MARKED`, suggesting the mark on the variant that represents the starting state. There is no "first variant wins" fallback: the default is visible at the type without reading the variant order, and reordering variants cannot change it.
- **Several variants marked**: `E_DEFAULT_MULTIPLE_VARIANTS`, labelling every `#[default]`.
- **The marked variant has fields**: `E_DEFAULT_VARIANT_HAS_PAYLOAD`. The help line shows a manual `impl Default` skeleton.

**Where `#[default]` may appear.** `#[default]` is a derive helper in the core attribute set ([§17 Attributes](#attributes)). It is allowed only on a unit variant of an enum that derives `Default`:
- anywhere else (a struct, field, function, alias, trait, impl, constant or extern item) it is `E_DEFAULT_ATTRIBUTE_INVALID_POSITION`;
- it takes no arguments: `#[default(...)]` is malformed;
- on a variant of an enum without `#[derive(Default)]` it is `E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE`, so unused marks do not accumulate. The fix-it adds the derive.

`#[default]` is independent of `#[non_exhaustive]` ([§11](#11-ownership-and-sharing)) and of explicit discriminants. `#[non_exhaustive] #[derive(Default)] enum Mode { #[default] Idle, ... }` is the usual shape of an evolving state enum, and `#[repr(u8)] #[derive(Default)] enum Op { #[default] Reset = 0x01, ... }` defaults to `Op.Reset`.

#### `Serialize` and `Deserialize`

`Serialize` and `Deserialize` generate format-independent traversal code at compile time. There are no macros and no runtime reflection: the derive walks the fields in declaration order, as the other derives do. Not yet implemented.

```kara
#[derive(Serialize, Deserialize)]
struct User { name: String, age: i64, active: bool }

let json: String = Json.serialize(user);
let back: User = Json.deserialize(json)?;        // the annotation selects User
```

The derived `Serialize` visits each field in order; the derived `Deserialize` rebuilds the value field by field. A format (`Json`, `Toml`, `MessagePack` and so on) implements `Serializer` and `Deserializer`. Adding a format needs only a new implementation; derived code does not change.

```kara
trait Serializer {
    fn serialize_bool(mut ref self, v: bool);
    fn serialize_i64(mut ref self, v: i64);
    fn serialize_string(mut ref self, v: Str);
    fn serialize_seq_begin(mut ref self, len: i64);
    fn serialize_seq_end(mut ref self);
    fn serialize_struct_begin(mut ref self, name: Str, len: i64);
    fn serialize_field(mut ref self, name: Str);
    fn serialize_struct_end(mut ref self);
    // ... enum, map and option visitors
}

trait Serialize {
    fn serialize[S: Serializer](self, s: mut ref S);
}

trait Deserializer {
    fn deserialize_bool(mut ref self) -> Result[bool, DeserializeError];
    fn deserialize_i64(mut ref self) -> Result[i64, DeserializeError];
    fn deserialize_string(mut ref self) -> Result[String, DeserializeError];
    fn deserialize_seq_begin(mut ref self) -> Result[i64, DeserializeError];
    fn deserialize_seq_end(mut ref self) -> Result[(), DeserializeError];
    fn deserialize_struct_begin(mut ref self, name: Str) -> Result[(), DeserializeError];
    fn deserialize_field(mut ref self) -> Result[String, DeserializeError];
    fn deserialize_struct_end(mut ref self) -> Result[(), DeserializeError];
    // ... enum, map and option visitors
}

trait Deserialize {
    fn deserialize[D: Deserializer](d: mut ref D) -> Result[Self, DeserializeError];
}
```

**Generic, not dynamic.** `serialize` and `deserialize` are generic over the format, so each format gets its own monomorphized code and no trait objects are involved.

**Effects come from the format.** A call to a generic function has the effects of its instance ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)). Serializing with a JSON serializer that writes to an open file has that file's write effects ([§12 Value-rooted resources](#value-rooted-resources)); serializing into a `String` buffer has only `allocates(Heap)`. The traits and the derived code name no effects of their own.

**Field attributes:**

```kara
#[derive(Serialize, Deserialize)]
struct Config {
    #[serde(rename: "server_name")]
    name: String,
    #[serde(skip)]
    internal_cache: Vec[i64],
    #[serde(default: 8080)]
    port: i64,
}
```

- The attributes are `rename`, `skip`, `skip_serializing`, `skip_deserializing` and `default` (deserializing only).
- Enums are externally tagged by default. `#[serde(tag: "type")]` makes them internally tagged and `#[serde(untagged)]` untagged.
- **`#[serde(default: expr)]`** takes a constant expression, the same class a `const` initializer accepts ([§4 Constants](#constants)): literals, named constants (`i64.MAX`), constant arithmetic (`MAX_PORT - 1`), and struct, array and tuple literals built from constants. A run-time expression (`Vec.new()`, `load_config()`) is a "not a constant expression" error.

---

## 10. Errors and panics

Errors are values. A recoverable failure is a `Result`, and a bug or an unrecoverable condition is a panic. There are no exceptions and no `try`/`catch`.

### Results and propagation

`Result[T, E]` and `Option[T]` are defined in [§5](#5-types). The `?` operator propagates their failure case.

- **`?` on `Result` and `Option`.** In a function returning `Result[T, E]`, `expr?` returns early with the `Err`. In a function returning `Option[T]`, `expr?` returns early with `None`. Otherwise it yields the success value.
- **Converting errors.** If `expr: Result[U, E1]` in a function returning `Result[T, E2]`, `expr?` converts the error with `From`:

  ```kara
  match expr {
      Ok(v) => v,
      Err(e) => return Err(E2.from(e)),
  }
  ```

  It needs `impl From[E1] for E2` ([§9](#9-traits)); without one, the `?` is a type error. When `E1` is `E2`, no conversion is inserted.
- **The conversion is a real call for effects.** The effects declared on the `From` impl are added to the enclosing function's effects, exactly as if the call were written out. A `From` impl that declares `writes(Log)` makes every `?` that uses it contribute `writes(Log)`. Without this rule a function could look effect-free while carrying the effects of conversions the compiler inserted.
- **`?` is postfix**, so `f()?.x` means `(f()?).x` and `-x?` means `-(x?)` ([Operators and precedence](#operators-and-precedence)). There is no optional-chaining operator.
- **Leaving early is a normal exit.** A `?` that returns runs the drops, `defer`s and `errdefer`s of every scope it leaves ([core-semantics.md §7.6](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).
- **`??` supplies a default** instead of propagating ([§6 `?` and `??`](#-and-)).
- **`unwrap()`** extracts the value and panics on `None` or `Err`. Its `panics` effect is inferred and queryable; a public function need not declare it ([§12](#12-effects)).
- **Panics are for unrecoverable conditions** ([Panics](#panics)): assertion failures, an index out of bounds, `unwrap()` on `None` or `Err`. The alternatives are `?`, `??`, `unwrap_or(default)` and `match`.
- **`try { ... }` blocks** are deferred ([deferred.md](deferred.md#unscheduled-language-extensions)). `try` is reserved.

**Panics report the caller's location: `#[track_caller]`.** Every standard-library function that panics on invalid input carries `#[track_caller]`: `Option.unwrap`, `Option.expect`, `Result.unwrap`, `Result.expect`, `Result.unwrap_err`, the panicking indexing helpers and their `.first()`/`.last()` relatives, the trapping arithmetic methods (`i64.unchecked_div`, `i64.unchecked_rem`), `assert`, `assert_eq`, `assert_ne`, `todo()`, `unreachable()` and `panic()`.
- The attribute makes the panic location, on stderr and in the runner's panic record ([§17 Panic record](#panic-record)), the caller's location, not a line inside the library. `users.get(99).unwrap()` reports the user's `unwrap()`.
- It adds a hidden caller-location parameter that the panic path reads. It has no other effect on type checking, effects, the ABI, monomorphization or code generation.
- It is transitive: a `#[track_caller]` function calling another reports its own caller.
- Built-in operator panics (overflow in `a + b`, division by zero in `a / b`, an out-of-bounds `a[i]`) already report the operator's location.
- Every new panicking standard-library function must carry it; the library lint rejects one that does not. User code may use it on its own panicking wrappers.

**The error-return trace.** In debug builds the compiler records the path an error travels through `?` sites: file, line and expression. The trace, with any `.context()` messages ([below](#error-and-anyerror)), appears in the JSON output:

```json
{
  "error_return_trace": [
    { "file": "src/db.kara", "line": 84, "expr": "db.find(id)?",
      "context": "while loading user 42" },
    { "file": "src/api.kara", "line": 12, "expr": "process_user(id)?",
      "context": null }
  ]
}
```

A site with `.context()` fills `"context"`; a bare `?` leaves it `null`. Release builds omit `error_return_trace`, because recording it costs time at every `?`. A context message survives release builds, because it is part of the error value.

### `Error` and `AnyError`

**The `Error` trait** marks a type as an error:

```kara
pub trait Error: Display { }
```

The standard library's error types implement it.

**`AnyError`** is an erased error type that needs no `dyn`. It keeps the original error's message, the name of its type, and a list of context messages:

```kara
pub struct AnyError { msg: String, kind: String, context: Vec[String] }

impl[E: Error] From[E] for AnyError { ... }
```

- Because every `Error` converts into it, `?` propagates any error into a function returning `Result[T, AnyError]`. An application can use it without writing an error enum and a `From` impl per library.
- Its `Display` prints the chain outermost first, joined by `": "`: the last context message added, then the earlier ones, then the original message (`while loading user 42: connection refused`).
- It keeps the message, not the original value, so it cannot be matched by the original error type. A function whose callers handle specific errors returns its own error type.
- It does not implement `Error`. If it did, `From[E]` for every `E: Error` would overlap the identity conversion, and the two `.context()` impls below would overlap, as with Rust's `anyhow`. So an `AnyError` never wraps another.
- `Error` and `AnyError` are in the prelude.

**`.context(msg)`** adds why an error happened, which the compiler cannot infer; the trace already says where:

```kara
impl[T, E: Error] Result[T, E] {
    fn context(own self, msg: own String) -> Result[T, AnyError] { ... }
}

impl[T] Result[T, AnyError] {
    fn context(own self, msg: own String) -> Result[T, AnyError] { ... }
}
```

On `Ok` the value passes through. On `Err` the error becomes an `AnyError` (if it is not one already) and the message is added to its context list. Chaining `.context()` keeps the type `Result[T, AnyError]`, so context can be added at every level without nesting new types.

```kara
fn process_user(db: Db, id: u64) -> Result[Report, AnyError] {
    let user = db.find(id).context(f"while loading user {id}")?;
    let orders = db.orders_for(user.id).context(f"while fetching orders for user {id}")?;
    Ok(build_report(user, orders))
}
```

### Cleanup with `defer` and `errdefer`

Two statements give deterministic cleanup without wrapper types. Their place in the drop order is [core-semantics.md §7.3 and §7.7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0): bindings, `defer`s and `errdefer`s share one LIFO order per scope, and neither statement runs on a panic.

- **`defer expr`** runs `expr` when the enclosing scope ends: at its end, or through `return`, `break`, `continue` or `?`.
- **`errdefer expr`** runs `expr` at its position in that order only if the function is returning the failure variant of its `Result` or `Option` return type, however that value is produced: a `?` that propagates, `return Err(...)`, `return None`, or a tail `Err(...)`. Otherwise it is skipped.

```kara
fn open_connection(addr: String) -> Result[Connection, AnyError] {
    let conn = Connection.open(addr)?;
    errdefer conn.close();        // runs only if we return Err below
    register_metrics(conn)?;      // register_metrics(conn: Connection) borrows conn; on error, conn is closed
    Ok(conn)                      // success: conn is returned and the errdefer does not run
}
```

`errdefer(e)` binds the error being returned, by `ref`, for use in the cleanup block:

```kara
fn open_connection(addr: String) -> Result[Connection, AnyError] {
    let conn = Connection.open(addr)?;
    errdefer(e) {
        log_error(e);             // log_error(e: AnyError) borrows e
        conn.close();
    }
    register_metrics(conn)?;
    Ok(conn)
}
```

`e` is a `ref` to the function's error type, because the error is still being returned; it is in scope only in the block. `errdefer(e)` requires a function returning `Result[T, E]`, because `None` carries no value: in a function returning `Option[T]` it is `errdefer with a binding requires Result return type; use errdefer { ... } for cleanup on None-propagation`.

**Rules.**
- **No `?` inside a `defer` or `errdefer` block.** Handle errors in the block: ignore them, log them, or panic.
- **No capture by move.** The bindings a cleanup block uses must stay valid until the scope ends; the ownership rules enforce this.
- **Effects.** A cleanup block's effects are part of the enclosing function's effects, like any other expression's.
- **Strict order.** Cleanup blocks run one at a time in LIFO order and are never reordered or run in parallel, even when their effects are disjoint, so resources are released in order (a child handle before its parent).

### Panics

A panic is for a bug or an unrecoverable condition. [core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8) lists what panics and what a panic does: it writes the [panic record](#panic-record) to stderr, runs the program's [custom panic handler](#panic-handler) if it has one, and ends the process with exit code **101**. No `Drop` body, `defer` or `errdefer` runs.

- **There is no way to catch a panic.** Kāra has no unwinding, no `catch_panic` and no `extern "C-unwind"`. A service runs under a supervisor that restarts it; failure isolation per task comes with the services track.
- **The stream and the exit code are the contract.** On every backend, a panic report goes to stderr, never stdout, and the exit code is 101, never 1, so a script can tell a bug from an error exit (code 1, [below](#the-main-function-and-exit-codes)). The report's fields are in [§17 Panic record](#panic-record); its text is not specified.
- **Tests** run each in its own process, so a panicking test fails alone ([§14](#14-testing)).

**A custom panic handler**, one function marked `#[panic_handler]`, is the only user code that runs on a panic. It is specified in [§16 Panic handler](#panic-handler).

### The `main` function and exit codes

`main` is the program's entry point. It is never `pub`, takes no parameters, and its effects are inferred like any private function's. Command-line arguments come from `env.args()`, whose `reads(Env)` effect flows into `main`'s effects.

**Return types.** `main` returns exactly one of:

| Return type | Exit code on success | Exit code on failure | Notes |
|---|---|---|---|
| `()` | `0` | none | No fallible operations at the top. |
| `Result[(), E]` with `E: Display` | `0` on `Ok(())` | `1` on `Err(e)` | The runtime prints `Error: {e}` to stderr. |
| `ExitCode` | the returned code | the returned code | For arbitrary exit codes. |

Any other return type is a compile error that lists these three.

**The `E: Display` bound** is checked at `main`'s signature. If `E` lacks `Display`, the error points at the signature with a fix that adds `impl Display for E`. There is no `Termination` trait: a direct bound is simpler for one use site.

**An error exit** is ordinary: when `main` returns `Err(e)`, `main`'s drops, `defer`s and `errdefer`s run, then the runtime writes exactly `Error: {e}` and a newline to stderr and exits with `1` ([core-semantics.md §10.3](core-semantics.md#10-panics-and-errors-c8)). The code is the literal `1` on every platform, not `EXIT_FAILURE`. `KARA_BACKTRACE=1` in the environment adds a backtrace.

**`ExitCode`** wraps an OS exit code: `ExitCode.SUCCESS`, `ExitCode.FAILURE`, `ExitCode.from(code: i32) -> ExitCode`. Returning it is a normal return, so `main`'s drops and `defer`s run. A program that wants both `?` and a custom code says so:

```kara
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode.SUCCESS,
        Err(e) => {
            eprintln(f"Error: {e}");
            ExitCode.from(e.exit_code())
        }
    }
}
```

**`process.exit`** ends the process with a chosen code, for when threading an `ExitCode` up through many frames is impractical:

```kara
fn exit(code: i32) -> Never    // in std.process
```

- It flushes standard output and standard error, then ends the process with `code`.
- No `Drop` body, `defer` or `errdefer` runs, in any frame.
- It declares no effects, because ending the process is not a resource access, and it exists on every target.
- It may be called anywhere, a `Drop` body and the panic handler included.

**Effects.** Because `main` is not `pub`, its effects are inferred from its body: `writes(Stderr)` from `eprintln`, `reads(Env)` from `env.args()`, and so on. This set is the `program_effects` summary reported after a successful build.

#### Script mode

A file with top-level statements, not just items, gets an implicit `fn main() -> Result[(), AnyError]`. One-off scripts need no `fn main() { ... }`:

```kara
// file: hello.kara (no fn main)
let name = "world";
print(f"hello, {name}");

// compiled as:
// fn main() -> Result[(), AnyError] {
//     let name = "world";
//     print(f"hello, {name}");
//     Ok(())
// }
```

`?` works, because the implicit `main` returns a `Result`:

```kara
// file: analyze.kara
let data = File.read("input.csv")?;
let rows = parse_csv(data);
print(f"{rows.len()} rows");
```

- **Items and statements mix.** Items (`fn`, `struct`, `enum`, `trait`, `impl`, `import`) are hoisted as in any module; statements form `main`'s body in file order.
- **Both top-level statements and an explicit `fn main` is an error:**

  ```
  error: file contains both top-level statements and an explicit `fn main()`
    hello.kara:4:1
        print("hello");
        ^^^^^
    note: fn main() is defined at hello.kara:8:1
    help: move the statements into `main`, or remove the explicit `main` to use script mode
  ```

- The implicit `main`'s effects are inferred from the statements, as for an explicit `main`, and it ends with `Ok(())`.

---

## 11. Ownership and sharing

[`core-semantics.md`](core-semantics.md) is normative for moves, borrows, views, sharing and destruction. This section covers the surface a programmer writes: the sharing types `shared`, `sync` and `frozen`, `weak` fields, typestate, and the attributes `#[must_use]`, `#[non_exhaustive]` and `#[deprecated]`. Where a rule belongs to core-semantics, this section states it in a sentence and links it. Parameter modes ([§7 Parameter modes](#parameter-modes)), call-site `mut` markers and `ref` returns are in [§7](#7-functions-and-closures).

### The core rules in brief

- **Values are owned.** Using a moved place is error E0500. Duplication is always written `.clone()`; nothing is copied, counted or shared implicitly ([core-semantics.md §1, §3](core-semantics.md#3-moves)).
- **Modes are declared.** A bare parameter `x: T` borrows (a `Copy` type is copied); `own T` is owned and belongs to the callee; `frozen T` is a read-only shared handle. The forms are in [§7 Parameter modes](#parameter-modes) and [core-semantics.md §4](core-semantics.md#4-parameters-calls-and-patterns).
- **Nothing moves out of a borrowed or shared place.** That covers borrowed parameters and `ref` places, `shared`, `sync` and `frozen` values, index projections and fields of a type with a `Drop` body. The fixes are `.clone()`, `mem.take`, `mem.replace`, `mem.swap` and `Option.take()` ([core-semantics.md §3.7](core-semantics.md#3-moves)).
- **References are decided by signatures, and views cannot escape** their origins ([core-semantics.md §5](core-semantics.md#5-references-and-views-c5)).
- **Sharing exists only through `shared`, `sync` and `frozen`.** The compiler never turns an owned value into a shared or atomically counted one ([core-semantics.md §6](core-semantics.md#6-sharing-c7)).
- **Values are destroyed at the end of their scope**, in reverse declaration order; temporaries at the end of their statement ([core-semantics.md §7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)).
- **A panic ends the process** without running drops ([core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8)).

### Escalation path

owned → `ref` (or a view) → `shared`, `sync` or `frozen`. Each step is a choice written in the source, never a compiler decision.

| Need | Use |
|---|---|
| One owner | An owned value (the default) |
| Temporary access | A borrow: a bare parameter, `mut ref T`, or a view such as `Slice[T]` |
| Several holders in one task: trees, graphs, lists | [`shared struct` / `shared enum`](#shared-types) |
| Mutation shared across tasks | [`sync struct` / `sync enum`](#sync-types) |
| A structure built once and then read by many tasks | [`frozen T`](#frozen-handles) |
| Many values with independent lifetimes and stale-handle detection | `Pool[T]` ([library/collections.md](library/collections.md)) |

### Shared types

**Philosophy.** Shared mutable state is permitted but never implicit. It is opted into at the type definition: `shared struct` and `shared enum` give reference semantics within one task, `sync struct` and `sync enum` give reference semantics across tasks with `Atomic[T]` and `Mutex[T]` fields, and `frozen` gives read-only sharing across tasks. There is no ambient global mutable state.

For data structures that are inherently shared, such as trees, graphs and linked lists, several references to one node are the norm. `shared struct` opts a type into reference semantics with a reference count:

```kara
shared struct TreeNode {
    val: i64,                      // immutable: set at construction
    mut left: Option[TreeNode],    // mutable: the links change
    mut right: Option[TreeNode],
}

let root = TreeNode { val: 1, left: None, right: None };
let child = TreeNode { val: 2, left: None, right: None };
root.left = Some(child);   // OK: `left` is mut; `child` stays usable

let alias = root;          // `alias` and `root` name the same node
```

| Behavior | Regular `struct` | `shared struct` |
|---|---|---|
| Using a value (assign, store, pass to an `own` parameter) | Moves it | Increments the count; the source stays usable |
| Mutation | Through an owned binding or `mut ref` | `mut` fields, through any handle |
| Destruction | At the owner's scope end | When the last handle is released |
| Cycles | Cannot form | Leak unless a back-edge is `weak` ([Cycles and `weak`](#cycles-and-weak)) |

**Handles** ([core-semantics.md §6.1](core-semantics.md#6-sharing-c7)):
- Using a handle as a value increments the count, and each binding, field or temporary holding a handle releases it when it is dropped. The object's `Drop` body and fields run when the count reaches zero.
- An `Option` or tuple made only of handles, `Copy` values and further such `Option`s and tuples (`Option[Node]`, `(Node, i64)`) is a *handle aggregate* and counts the same way. So `cur = node.next` walks a list without `.clone()`.
- Any other type containing a handle, such as a `Vec[Node]` or a user struct, is an ordinary move-only value. Moving it moves the handle; `.clone()` increments.

**Fields are immutable by default.** A `shared` type's fields are fixed at construction unless declared `mut`, so the definition is the record of what can change. Assigning a non-`mut` field is an error. A `mut` field can be written through any handle, however the handle was reached, including through a `ref` or a borrowed parameter ([core-semantics.md §5.9](core-semantics.md#5-references-and-views-c5)).

**`mut` fields and borrows** ([core-semantics.md §6.2](core-semantics.md#6-sharing-c7)):
- **A `Copy` or handle-aggregate field is read as a value.** Reading it, including as a `match`, `if let` or `while let` scrutinee, copies it or counts its handles, and holds no borrow. So the natural peek-then-mutate shape works:

  ```kara
  shared struct ListNode {
      val: i64,
      mut next: Option[ListNode],
  }

  fn remove_next(cur: ListNode) {
      match cur.next {
          Some(nxt) => { cur.next = nxt.next; }   // the arm works on a counted copy
          None => {}
      }
  }
  ```

- **Any other `mut` field is borrowed.** A `ref` into it holds a read borrow while the reference is live; a `mut ref`, including a `mut ref self` method call on the field, holds a write borrow; a `match` or `for` over it holds a read borrow for the whole construct.
- **A conflict through the same handle is a compile error.** The common case:

  ```kara
  shared struct Inbox {
      mut items: Vec[String],
  }

  fn requeue(inbox: Inbox) {
      for x in inbox.items {
          inbox.items.push(x.clone());   // error: `inbox.items` is borrowed by the loop
      }
  }
  ```

  The fix ends the borrow first:

  ```kara
  fn requeue(inbox: Inbox) {
      let again: Vec[String] = inbox.items.clone();
      for x in again.into_iter() {
          inbox.items.push(x);           // OK: no borrow of `inbox.items` is live
      }
  }
  ```

- **A conflict through another handle is a runtime check.** Two handles may name the same object, and the compiler cannot always tell. So each `mut` field whose type is neither `Copy` nor a handle aggregate carries a borrow flag, and a conflicting access through another handle panics:

  ```kara
  fn copy_items(from: Inbox, to: Inbox) {
      for x in from.items {
          to.items.push(x.clone());      // panics if `from` and `to` are the same object
      }
  }
  ```

  The panic message names the field and the source spans of both accesses. A flag costs one byte per field per instance before padding; `Copy` and handle-aggregate fields and immutable fields have none. `karac explain` reports the total for each type.

**Methods take a borrowed `self` only** ([core-semantics.md §4.3](core-semantics.md#4-parameters-calls-and-patterns)). `mut ref self` is an error, because other handles prevent exclusive access. A consuming `own self` is an error, because other handles may still hold the object. A method changes the object through its `mut` fields.

**Patterns.** A `shared` scrutinee counts as a `ref` scrutinee: binding a part of it borrows that part, and moving the binding is an error whose fix is `.clone()`. A binding that covers the whole handle copies the handle ([core-semantics.md §4.6](core-semantics.md#4-parameters-calls-and-patterns)).

**Equality.** A `shared` type has no default `==`. `#[derive(PartialEq, Eq)]` gives structural equality. `a.ref_eq(b)` compares identity: it is a method the compiler provides on every `shared`, `sync` and `frozen` handle type, with no bound.

**One task only.** A `shared` handle never crosses a task boundary (`E_NOT_CROSS_TASK`, [core-semantics.md §6.3](core-semantics.md#6-sharing-c7)). Its count is not atomic and its borrow flags are not synchronized. For mutation across tasks use a [`sync` type](#sync-types); for reads across tasks, [freeze](#frozen-handles) the value.

**`shared enum`** has the same semantics. It suits recursive data such as JSON values and ASTs.

**Field visibility** follows the rules for ordinary structs ([§5](#5-types)).

**Writing a `mut` field is not an effect.** It needs no annotation. A shared handle never crosses a task, so there is no concurrent access for an effect to order.

**Why a keyword.** Without `shared struct`, trees and graphs need handles into a `Pool[T]` or an arena: correct, but it makes basic data structures feel second-class. Swift makes the same split (`class` versus `struct`); Kāra writes `shared struct` instead of adding `class`, to avoid object-oriented connotations.

**Self-referential data that is deduplicated, not lifetime-managed, uses `Pool[T]`.** Type tables, schemas and descriptors name themselves in their own fields. Reaching for `shared` with `weak` on the recursive field is wrong there: such values are held strongly for the whole computation, and `weak` would thread `Option[T]` through every lookup. Use `Pool[Type]` with `Handle[Type]` in the recursive position: the table holds the pool, equality on types becomes equality on handles, and the cycle is broken structurally. `weak` is for lifetime cycles; `Pool[T]` and `Handle[T]` are for deduplication. See [library/collections.md](library/collections.md) for `Pool[T]` and `Arena[T]`.

### Cycles and `weak`

**A cycle of strong handles is never freed** ([core-semantics.md §6.5](core-semantics.md#6-sharing-c7)). There is no cycle collector, and the compiler does not detect cycles. A back-edge must be declared `weak`; reading a `weak` field yields `Option[T]`.

```kara
shared struct Parent {
    mut children: Vec[Child],     // strong: a parent keeps its children alive
}

shared struct Child {
    mut parent: weak Parent,      // weak: a child does not keep its parent alive
}
```

A graph whose edges must not keep nodes alive makes every edge weak and keeps the strong handles in one place, such as the graph's node list:

```kara
shared struct GraphNode {
    id: u64,
    mut neighbors: Vec[weak GraphNode],
}

fn connect(a: own GraphNode, b: own GraphNode) {
    a.neighbors.push(b);   // strong to weak conversion
    b.neighbors.push(a);   // both directions, no cycle of strong handles
}
```

**A type that no value can inhabit is rejected.** A self-reference with no way to end, directly (`mut next: Node` in `shared struct Node`) or through a type with no empty case (`mut kid: (Node, i64)`), is an error. A self-reference through a type that has an empty case (`Option[Node]`, `Vec[Node]`, `Map[K, Node]`, `Set[Node]`) is accepted: these are the ordinary list, tree and AST shapes, and built without a cycle they free normally. Whether a running program closes a cycle is not visible in the type, so placing `weak` on each back-edge is the programmer's obligation. The compiler cannot infer it: `Vec[weak Node]` used as a *children* list would drop each child as soon as the builder's handle is gone.

**`weak` rules:**
- **Strong to weak is implicit.** Assigning or passing a strong handle where a `weak` type is expected stores a weak reference.
- **Reading a `weak` field yields `Option[T]`.** The read checks whether the object is still alive and returns `Some(handle)` (counted) or `None`. A program never sees a raw weak reference; `weak` exists only in storage.
- **`.upgrade()`** does the same as reading the field, for code that wants to name the `Option` before matching.
- **`weak` combines with `pub` and `mut`** under the ordinary field rules. Assignment always accepts a strong handle; a read always yields `Option[T]`.
- **A `weak` reference to a `sync` type is atomic**: the liveness check and the increment are one atomic step.

**Ergonomics.** `weak` fields show their uncertainty as `Option[T]` on purpose, and there is no shorter spelling.
- **`Option.expect(msg)` is the tool for an invariant that guarantees the object is alive**, for example `node.parent.expect("a constructed tree has a parent")`. It panics like `.unwrap()`, but the message documents the invariant at the use site and appears in the panic report.
- **Upgrade once per scope.** `while let Some(p) = current.parent { current = p; }` upgrades once per iteration. Chained `.parent.expect(...).parent.expect(...)` reads count once per step.
- **If validity is guaranteed by the structure, the reference should be strong**, or the data should move to a central store (`Map[NodeId, NodeData]`, or a `Pool`) with ids.
- **Diagnostic.** `.unwrap()` on any `Option` gets a suggestion to use `.expect("…")` with a machine-applicable fix; this covers `weak` reads without a special rule.

**There is no unsafe escape hatch** such as `assume_valid`. `weak` is memory-safe even when the programmer's invariant is wrong, and an operation with undefined behavior on a dead reference would remove that property. The cost is one `.expect` per upgrade, at a site that says why the reference is live.

### `sync` types

`sync struct` and `sync enum` are the cross-task form of `shared`. Their count is atomic, and no field is written `mut` ([core-semantics.md §6.3](core-semantics.md#6-sharing-c7)).

```kara
// shared struct: one task, plain count, mut fields with borrow flags
shared struct TreeNode {
    val: i64,
    mut left: Option[TreeNode],
    mut right: Option[TreeNode],
}

// sync struct: any number of tasks, atomic count
sync struct Counter {
    name: String,                  // fixed: any task may read it
    count: Atomic[u64],            // lock-free updates
    state: Mutex[CounterState],    // compound updates under a lock
}
```

**Field constraints**, checked where the type is defined, not where it is used:
- **No field is written `mut`.** A `sync` value is never reassigned field by field through a handle, so `mut val: i64` and `mut count: Atomic[u64]` are both compile errors. State that changes lives in `Atomic[T]` and `Mutex[T]` fields, whose methods take a borrowed `self`.
- **Plain fields** are fixed at construction and may be read from any task without synchronization.
- **`Atomic[T]` fields** change through atomic methods such as `load`, `store` and `fetch_add` ([library/concurrency.md](library/concurrency.md#atomics)).
- **`Mutex[T]` fields** change through a lock guard ([Locks and atomics](#locks-and-atomics)), which suits compound updates.
- **Every field's type must be `CrossTask`** ([What may cross a task boundary](#what-may-cross-a-task-boundary)), so a `sync` type cannot hold a `shared` handle.

**`sync enum`** follows the same rules in every variant:

```kara
sync enum WorkItem {
    Task { id: u64, payload: Mutex[Vec[u8]] },
    Control { cmd: Atomic[u32] },
    Poison,
}
```

**Handles** count like `shared` handles ([core-semantics.md §6.1](core-semantics.md#6-sharing-c7)), with an atomic count. Passing one to an `own` parameter increments it. Patterns, equality (`#[derive(PartialEq, Eq)]`, `.ref_eq`) and `weak` fields work as for `shared` types.

**Methods take a borrowed `self` only**, for the same reason as on `shared` types:

```kara
impl Counter {
    fn increment(self) { self.count.fetch_add(1); }   // OK
    fn clear(mut ref self) { ... }    // error: methods of a sync type take a borrowed `self`
}
```

**Crossing tasks.** `sync` handles enter `par` branches, `par for` iterations and `TaskGroup` tasks freely ([core-semantics.md §11.3](core-semantics.md#11-concurrency-c9)). That is the reason the type exists: safety is enforced where the type is defined, so nothing more is checked where it crosses.

**Effects.** A `sync` type adds no effects. Atomic operations have none. `Mutex.lock` has `blocks`, because it may wait. Effects of code that runs while a guard is held belong to the enclosing function as usual.

**From `shared` to `sync`.** A type that started in one task and must now be mutated across tasks is converted by renaming the keyword and replacing each `mut` field with an `Atomic[T]` or `Mutex[T]` field, without `mut`:

```kara
// Before: one task
shared struct Session { id: u64, mut request_count: u64, mut last_path: String }

// After: many tasks
sync struct Session { id: u64, request_count: Atomic[u64], last_path: Mutex[String] }
```

The compiler suggests this conversion when a `shared` handle reaches a task:

```
error[E_NOT_CROSS_TASK]: `state` (type `Elevator`) cannot cross a task boundary
  --> src/main.kara:42:9
   |
42 |         run_controller(state),
   |                        ^^^^^ captured by a `par` branch here
   |
note: `Elevator` is a shared type
   = help: declare `Elevator` as a `sync struct` and replace its mut fields with `Mutex[T]` or `Atomic[T]` fields
   = fix: karac fix --apply=concurrent-promote-shared   (applies the diff below)

   suggested diff:
   - shared struct Elevator { stops: Vec[i64], mut direction: Direction, mut floor: i64 }
   + sync struct Elevator { stops: Vec[i64], direction: Mutex[Direction], floor: Atomic[i64] }
```

The diff puts each `mut` field in an `Atomic[T]` or a `Mutex[T]`. The programmer reviews that choice and applies the diff: a concurrent program carries correctness requirements that depend on context, and no machine-applied fix in this area is safe without review. If the value is only read across tasks, [freezing](#frozen-handles) it is the better fix and the type stays `shared`.

**Performance note: types that are mostly single-task.** A `sync` type pays an atomic count on every handle operation, in single-task code too. For a type that is mostly used in one task, keep the mutable state in a plain struct behind one `Mutex`: code holding the guard works on the plain struct with no atomic traffic.

```kara
struct SessionState { request_count: i64, last_active: Instant }   // plain

sync struct Session {
    id: u64,
    state: Mutex[SessionState],   // the atomic cost is on the handle only
}
```

**Not decided for v1:** whether `sync` should be the default and `shared` the narrow form. Either is defensible; the question is whether `sync` costs measurably more in single-task code. v1 ships `shared` for one task and `sync` for many.

### Frozen handles

**The gap `frozen` fills.** A `shared` type is single-task: its count is a plain load, add and store, so two tasks merely *reading* one handle would race on the count, and the compiler rejects the capture. A `sync` type shares, but it pays an atomic operation on every handle use of the type, including the sequential ones. Neither fits a structure that is built once and then only read: a graph to traverse, a parsed AST, an interned table, a loaded configuration.

`frozen` removes the raced count instead of making it atomic:

```kara
shared struct Node { val: i64, mut neighbors: Vec[Node] }

fn sum(n: frozen Node, depth: i64) -> i64 {
    if depth <= 0 { return n.val; }
    let mut t: i64 = n.val;
    for k in n.neighbors.iter() { t += sum(k, depth - 1); }
    t
}

fn main() {
    let root: Node = build();
    let g = freeze root;                        // the freeze site
    let (a, b) = par { sum(g, 3), sum(g, 4) };  // both branches read the same graph
    println(f"{a + b}");
}
```

**Four properties, each necessary.** A `frozen T` is a handle to a `shared` value that is:
1. **non-counting**: it never changes the count, which is what makes concurrent reads safe without atomics;
2. **deeply immutable**: nothing reachable through it may be written, which is what makes skipping the count safe;
3. **non-escaping**: it may not outlive its owner, because a non-counting handle that outlives its source would dangle;
4. **usable across signatures**: it is a parameter mode, so a traversal in a callee can take one.

Dropping any one of them brings back the hazard the other three remove.

**Two spellings, and nothing is inferred.** `frozen T` is a parameter mode beside bare `T`, `own T` and `mut ref T` (`fn sum(n: frozen Node)`, and `frozen self` for a receiver). `freeze` is an expression that produces a frozen binding (`let g = freeze root;`). A value never becomes frozen by inference: the author writes one of the two, which keeps the cost model readable and keeps the compiler from changing a type's representation program-wide.

**What a frozen place permits.** Frozen-ness belongs to a *place*: the parameter, and any chain of `.field`, `[i]` or `.0` rooted at it.

| Form | Permitted | Why |
|---|---|---|
| `n.inner.deep.d`: a scalar read at any depth | yes | a projection does not create a handle |
| `n.kids[i].val`: a read through an index | yes | same |
| `g(n.kids[i])`: a whole handle into another `frozen` slot | yes | frozen-ness is kept across the call |
| `n.kids.len()`, `n.kids.is_empty()` | yes | read-only queries |
| `for k in n.kids` and `for k in n.kids.iter()` | yes | both spellings lower the same way |
| `let k = n.kids[i];` | yes | the binding is itself a frozen place |
| `n.count = 5` and every other write | **no** | property 2 |
| capturing `n` in a closure | yes; the closure is a view and may not escape | property 3; [Handles in closures](#handles-in-closures) |
| `return n;`, storing the **handle** in a non-frozen slot | **no** | property 3 |

A *scalar* read out of a frozen place is an ordinary value and may go anywhere; only the handle may not escape.

A `mut` field is **readable** through a frozen place and never **writable**. That is the point of freezing one value rather than a whole type: a graph algorithm that reads `mut neighbors` on a frozen shared root while writing `mut neighbors` on its own fresh copies does nothing wrong, and a rule on the type could not tell the two apart.

**Local worklists.** A `Vec` local whose elements are frozen handles is recognized without new syntax: declare `let mut q: Vec[Node] = Vec.new();`, and `q.push(<frozen place>)`, `q.len()`, `q.is_empty()` and `q[i]` are permitted, with `q[i]` itself a frozen place. This lets an iterative traversal keep a worklist. It is `Vec`-only: `pop_front` on a `VecDeque` returns a value rather than a place, so it cannot keep frozen-ness. An index cursor over a `Vec` is the supported shape.

**The freeze site.** `freeze` refuses three things, and refuses when a type cannot be resolved:
- a **non-`shared`** type: a plain value has no count for `frozen` to skip;
- a **`sync`** type: it is already shareable, so `frozen` would add nothing;
- a **non-unique source** for a type with a `mut` field anywhere in it: `let root = nodes[0]; let g = freeze root;` is refused while `nodes` is live, because `nodes` holds another handle to the same value and the immutability claim could not be checked against it. A type with no `mut` field anywhere is immutable by structure, so aliasing it is harmless and no uniqueness is needed.

**With `par`.** A `par` branch or `par for` iteration may use a frozen handle from the enclosing scope ([core-semantics.md §11.3](core-semantics.md#11-concurrency-c9)). The same program without `freeze` is refused, because the branches would capture a `shared` handle.

**Cost.** A frozen parameter changes no count, which is also true of a borrowed parameter. What `frozen` adds is the freeze-site guarantee that permits concurrent reads, which a borrow alone does not give. The alternative, a `sync` type, makes counting atomic on every use of the type, roughly an order of magnitude slower on count-heavy sequential code.

**Not decided for v1:**
- whether a `freeze` may be scoped to a region shorter than the owner's lifetime, rather than to the binding;
- whether frozen-ness should have a type-position spelling (`Vec[frozen Node]` as a declared local type), rather than being recognized for worklists as above.

### Handles in closures

Any closure may capture a `shared`, `sync` or `frozen` handle ([core-semantics.md §9.1](core-semantics.md#9-closures)).
- **`shared` and `sync` handles are counted** when captured, so the closure holds handles of its own. Whether it may then enter a task follows from what it captures ([What may cross a task boundary](#what-may-cross-a-task-boundary)): a closure that captures a `shared` handle is task-local, like any other value that contains one.
- **A `frozen` handle is never counted**, so a closure that captures one is a view ([core-semantics.md §5.7](core-semantics.md#5-references-and-views-c5)). It may be called, passed down, and enter `par` branches and `TaskGroup` tasks, but it may not escape ([core-semantics.md §9.3](core-semantics.md#9-closures)).

### Typestate

Ownership gives compile-time protocol state machines with no extra language feature. A generic type whose parameter names a state carries the state in the type at no runtime cost, and the compiler rejects an operation on a value in the wrong state:

```kara
// State markers: empty types
struct Disconnected {}
struct Connected {}

// `State` appears in no field: it exists only in the type
struct Connection[State] {
    socket: Socket,
}

fn connect(c: own Connection[Disconnected]) -> Result[Connection[Connected], NetError] { ... }
fn send(c: Connection[Connected], data: Slice[u8]) -> Result[(), NetError] { ... }
fn disconnect(c: own Connection[Connected]) { ... }

let conn: Connection[Disconnected] = Connection { socket: Socket.new() };
send(conn, data);   // error: expected `Connection[Connected]`, found `Connection[Disconnected]`
```

A state cannot be forked: once `connect` consumes the `Connection[Disconnected]`, using the old value is E0500. This covers protocol state machines, builders and resource lifecycles (open, closed, disposed).

### `#[must_use]`

`#[must_use]` on a type makes silently dropping a value of that type a warning. It completes typestate by making sure a value reaches a final state instead of being forgotten:

```kara
#[must_use("connections must be explicitly disconnected")]
struct Connection[State] {
    socket: Socket,
}

fn open_only(conn: own Connection[Disconnected]) -> Result[(), NetError] {
    connect(conn)?;            // warning: unused `Connection[Connected]`
    Ok(())
}
```

Writing `let _ = connect(conn)?;` discards the value on purpose and suppresses the warning.

**Rules:**
- On a type, it applies wherever a value of that type would be dropped without being bound or used.
- The warning text is the string argument: `#[must_use("reason")]`. Without one, a default message is used.
- On a function, it warns when the return value is discarded. That is separate from the type form.
- There are no linear types. `#[must_use]` asks for at least one use, as a warning. `let _ = expr;` is the standard way to discard on purpose; it drops the value at the `;` ([core-semantics.md §4.6](core-semantics.md#4-parameters-calls-and-patterns)).

**Standard library mandate.** Every standard library type or function whose result is the point of the operation carries `#[must_use]`. Five categories:
1. **`Result[T, E]` and `Option[T]`** are must-use by a compiler rule, in library and user code alike. Discarding an error is the bug class the attribute exists for, and an `Option` from a lookup or parse must be handled.
2. **Every iterator adapter** (`map`, `filter`, `take`, `skip`, `chain`, `zip`, `enumerate`, `peekable`, `rev`, `flatten`, `flat_map`, and the rest). A discarded adapter does nothing, so the discard is a missing final call such as `.collect()`, `.for_each(...)` or `.sum()`. The attribute is on the adapter's return type, so it fires however the chain is written.
3. **Every guard type**, such as `MutexGuard[T]`. Discarding a guard drops it at once, releasing the lock at the wrong point. The message names the resource: `"the mutex is released when this guard drops; bind it to a name to keep the lock"`.
4. **Every builder step before the final `.build()` or `.finish()`.** Discarding a half-built builder loses every option set on it. The final step returns a `Result` (or the value, for an infallible builder), which category 1 covers.
5. **Pure transformations whose result is the only effect**: `String.to_lowercase()`, `String.trim()`, `String.replace(...)`, `Vec.iter()`, and every method that returns a new value derived from `self` without changing it. The attribute is on the function, so `String` itself is not must-use.

**Displaced-value exception to category 1.** A mutating container method whose `Option` result reports the element it displaced or removed is exempt: `Map.insert` and `SortedMap.insert` (the previous value), `Map.remove` and `SortedMap.remove`, `Vec.pop`, `Vec.remove_first`, and `VecDeque.pop_front` and `pop_back`. The mutation is the point, and `map.insert(k, v);` as a statement is how nearly everyone writes it. Warning there would train programmers to scatter `let _ =` and weaken the warning where it matters. Rust makes the same choice for `HashMap::insert`. The exemption is scoped to these standard containers: a user-defined `insert` returning `Option` still warns.

**The exemption follows the fallible twin.** `Map.try_insert(k, v)` returns `Result[Option[V], AllocError]`, so `m.try_insert(k, v)?;` discards exactly the displaced value that `m.insert(k, v);` discards, and is exempt for the same reason. The exemption never covers the allocation outcome: `m.try_insert(k, v);` without `?` discards the `Result` and still warns.

The mandate is enforced by a standard library lint, not by a compiler rule on user code. User code may use `#[must_use]` wherever discarding is almost always a bug. `let _ = expr;` is the universal escape.

### `#[non_exhaustive]`

`#[non_exhaustive]` on a `pub enum` or `pub struct` declares that the type will gain variants or fields in later minor versions. It limits what other packages may do with the type; code in the defining package is unaffected.

- **On an enum**, a `match` outside the defining package needs a wildcard arm, even when it names every current variant. Adding a variant is then a minor change. Inside the defining package, matches are checked for exhaustiveness as usual, which is where a forgotten new variant is caught.
- **On a struct**, a struct literal outside the defining package is an error whose diagnostic names the package's constructor function, and a pattern there must end with `..` (`let Options { port, .. } = opts;`). Adding a field is then a minor change.
- **Fix.** A `match` without the wildcard arm gets a machine-applicable fix that adds `_ => todo()`.

```kara
// In another package
match err {
    FetchError.Timeout => retry(),
    FetchError.NotFound => skip(),
    _ => fail(err),   // required: `FetchError` is #[non_exhaustive]
}
```

The standard library marks every public error enum and option struct that is expected to grow. A closed set (`enum Direction { N, S, E, W }`) or a struct whose fields are its identity (`struct Point { x: f64, y: f64 }`) does not carry it: changing one of those is a breaking change on purpose.

### `#[deprecated]`

`#[deprecated]` on an item (a function, struct, enum, enum variant, trait, method, associated function, type alias or constant) marks it for removal. Every use of the item, whether a call, a use of the type name, a variant constructor or a pattern, emits the `deprecated` lint at the use site. The lint warns by default. Code keeps compiling; the warning is the migration signal.

```kara
#[deprecated]
pub fn old_api() { ... }

#[deprecated(note: "use `fs.read_to_string` instead")]
pub fn read_file_contents(path: Str) -> String { ... }

#[deprecated(since: "1.2.0", note: "use `parse_config` instead")]
pub fn load_config(path: Str) -> Config { ... }
```

The bare form gives a default warning. Otherwise it takes two labeled arguments, either of which may be left out: `since: "version"`, which is advisory and not checked against `kara.toml`, and `note: "text"`, the migration message.

```
warning[deprecated]: use of deprecated function `read_file_contents`
  --> src/main.kara:42:13
   |
42 |     let s = read_file_contents(p);
   |             ^^^^^^^^^^^^^^^^^^
   = note: use `fs.read_to_string` instead
   = help: suppress with `#[allow(deprecated)]`
```

With `since`, the diagnostic adds `= deprecated since: "1.2.0"`. For a `pub` item it also names the defining package, so the migration target is clear across re-exports.

- **Suppression** follows the [lint levels](#lint-levels). `#[allow(deprecated)]` works at a use site, or on a function or module, for example for the defining package's own callers during the deprecation window. `#[expect(deprecated)]` suits a planned migration: once the call is rewritten, the expectation reports itself as unfulfilled.
- **What it does not do.** It does not remove the item, change its visibility or behavior, or change its semver category: adding `#[deprecated]` is a minor change, and removing the item later is a major one. It is not transitive: deprecating a function does not mark its callers, and deprecating a struct does not mark its methods.
- **Where it is rejected.** On an `impl` block (`error[E_DEPRECATED_ON_IMPL]`; deprecate the methods instead) and on a struct field.

---

## 12. Effects

Every function has an **effect set**: what it does to the world besides computing its result. Effects have three jobs in v1:
1. **A capability contract.** A `pub fn` declares what it may do, and by omission what it may not. `#[no_effect(...)]` forbids effects below a boundary.
2. **Safe concurrency.** Two `par` branches that conflict on a resource are a compile error ([§13](#13-concurrency)).
3. **Explanation.** `karac explain` and `karac query effects` show every function's effect set and why it has each effect.

Private functions infer their effects; public functions declare them, and the compiler checks the declaration against the body. The defaults that keep the system sound (unknown callees, `extern` functions, drops, recursion and value-keyed resources) are normative in [core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10).

```kara
effect resource UserDb;
effect resource OrderDb;

fn save_user(user: User) with writes(UserDb) { ... }
fn load_orders(id: u64) -> Vec[Order] with reads(OrderDb) { ... }
fn calculate(x: f64, y: f64) -> f64 { ... }    // no effects
```

**Effects are static capabilities.** A function that does not have `reads(FileSystem)` cannot read files: the compiler rejects the body. Public declarations pin authority at module edges, and `#[no_effect]` forbids effects below a boundary. Authority is encoded in signatures and checked at compile time. Kāra does not use runtime capability tokens or object-capability enforcement, which would need a trust model the language does not have.

### Effect verbs

| Verb | Takes a resource | Meaning | Conflicts with |
|---|---|---|---|
| `reads(R)` | yes | observes `R` | `writes(R)` |
| `writes(R)` | yes | changes `R` | `reads(R)`, `writes(R)` |
| `sends(R)` | yes | sends on `R` | `sends(R)` |
| `receives(R)` | yes | receives from `R` | `receives(R)` |
| `allocates(R)` | yes, usually `Heap` | allocates from `R` | nothing |
| `panics` | no | may end the process ([§10](#10-errors-and-panics)) | nothing |
| `blocks` | no | may park the OS thread | nothing |

`reads`, `writes`, `sends` and `receives` are the conflict verbs. `allocates` and `panics` are capabilities that never conflict, and both are [default-permitted](#default-permitted-effects). `blocks` is an [execution effect](#blocks): it says where a function may wait, not what it touches.

A second execution effect, `suspends` (the task may yield cooperatively), returns with coroutines in the [services track](deferred.md#m4a-services).

**Syntax.** An effect clause follows the return type: `fn f(x: T) -> U with reads(A) writes(B)`. Effects are written side by side, and a verb may list several resources: `reads(UserDb, OrderDb)`. Every effect word, `with` excepted, is a contextual keyword: `reads`, `writes`, `sends`, `receives`, `allocates`, `panics` and `blocks` are ordinary identifiers outside an effect clause ([§3](#3-lexical-structure)).

### Conflict rules

| Combination | Same resource | Different resources |
|---|---|---|
| `reads` + `reads` | Safe | Safe |
| `reads` + `writes` | **Conflict** | Safe |
| `writes` + `writes` | **Conflict** | Safe |
| `sends` + `sends` | **Conflict** | Safe |
| `receives` + `receives` | **Conflict** | Safe |
| `sends` + `receives` | Safe | Safe |
| `reads`/`writes` + `sends`/`receives` | Safe | Safe |
| `allocates` + anything | Safe | Safe |
| `panics`, `blocks` | Never conflict | Never conflict |

- **Sends keep their order, and so do receives.** Messages on one channel or connection have an order, so two sends to the same resource conflict, and two receives do too.
- **A send and a receive on one resource are independent**, as in full-duplex I/O.
- **State and communication are independent.** `reads` and `writes` track state; `sends` and `receives` track messages. A resource is usually used one way or the other.
- **Capabilities never conflict.** `FileSystem` and `Network` are capabilities, not conflict keys, so effects on them never conflict ([Built-in resources](#built-in-resources)).
- **Where conflicts are checked:** between the branches of a `par` block and the iterations of a `par for` ([§13](#13-concurrency)). A conflict is an error that names both accesses ([core-semantics.md §11.2](core-semantics.md#11-concurrency-c9)). `TaskGroup` tasks are not checked for effect conflicts ([`TaskGroup`](#taskgroup)).

### Resources

#### Built-in resources

| Verb | Resource | Covers |
|---|---|---|
| `reads`/`writes` | `FileSystem` | the capability to use files by path |
| `reads` | `Stdin` | standard input |
| `writes` | `Stdout` | standard output (`print`, `println`) |
| `writes` | `Stderr` | standard error (`eprint`, `eprintln`) |
| `reads` | `Clock` | wall-clock and monotonic time |
| `reads`/`writes` | `Env` | environment variables, arguments, working directory |
| `sends`/`receives` | `Network` | the capability to use the network |
| `reads` | `RandomSource` | system entropy |
| `allocates` | `Heap` | the heap (default-permitted) |

Because `println` has `writes(Stdout)`, two `par` branches that both print conflict ([core-semantics.md §11.2](core-semantics.md#11-concurrency-c9)).

**`FileSystem` and `Network` are capabilities, not conflict keys** ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10), item 5). `writes(FileSystem)` means "may write files by path", and `sends(Network)` means "may send on the network". Effects on them never conflict, so two branches that call path-taking functions such as `fs.write(path, data)` do not conflict. Conflicts on files and connections come from the values operations are rooted at: an open `File` or a connection is keyed by its value ([Value-rooted resources](#value-rooted-resources)), so two branches that write one `File`, or send on one connection, conflict, and two that use separate ones do not.

**Two branches that write the same path by name are not detected.** The compiler does not compare paths, so such a program compiles, and the file's contents depend on which branch writes last.

The names `CompileTimeEnv` and `CompileTimeHeap` are reserved for the comptime track; declaring a resource with either name is an error.

#### Value-rooted resources

A resource may be rooted at a value: `sends(tx)` on a channel sender, `receives(rx)` on a receiver, `writes(self.cache)`, `reads(f)` on an open file. Such a resource is **keyed by the value** ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10), item 5):
- two channels, two connections or two files are two resources;
- when the compiler cannot tell whether two such values are the same, they conflict;
- a resource rooted at a parameter (`writes(f)`, `sends(tx)`) is substituted by the argument at each call, and keyed by that argument;
- effects on a function's own locals are not effects of the function: they never appear in its signature. Inside its body they still decide which `par` branches conflict, as `one_channel` below shows.

```kara
fn produce(tx: Sender[i64], from: i64, to: i64) -> Result[(), SendError[i64]] {
    for i in from..to { tx.send(i)?; }       // sends(tx)
    Ok(())
}

fn two_channels() -> Result[(), SendError[i64]] {
    let (tx1, rx1): (Sender[i64], Receiver[i64]) = channel(8);
    let (tx2, rx2): (Sender[i64], Receiver[i64]) = channel(8);
    let (r1, r2) = par { produce(tx1, 0, 4), produce(tx2, 4, 8) };   // OK: two resources
    r1?;
    r2?;
    Ok(())
}

fn one_channel() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(8);
    let (r1, r2) = par { produce(tx, 0, 4), produce(tx, 4, 8) };
    // error: both branches have `sends(tx)`; sends on one channel keep their order
}
```

This is also what lets automatic parallelism, when it arrives, leave `conn.write(header); conn.write(body);` in order while running writes on different connections in parallel.

#### Nondeterminism resources

**Every source of nondeterminism enters through a named resource.** A function that reads the clock, draws a random number or reads the process environment is observably nondeterministic, and its effect set says so. There is no ambient clock, no global random generator and no environment access that bypasses the effect system. This is a language commitment, because reproducibility and deterministic replay are properties of the language, not of one standard library.

- **`Clock`**: wall-clock time, monotonic time and any value that depends on when the program runs. Kāra does not split it into wall and monotonic resources: the distinction matters to the API, not to conflict checking.
- **`RandomSource`**: entropy and any value drawn from a randomness primitive (random numbers, UUIDs, nonces). A pseudo-random generator seeded from a value the caller passes in is not `reads(RandomSource)`: determinism returns as soon as the source of randomness is an argument.
- **`Env`**: environment variables, command-line arguments, the working directory, and anything else the operating system hands the process. Reading is `reads(Env)`; setting a variable is `writes(Env)`.

Their APIs are in [library/time-random-env.md](library/time-random-env.md).

**Inference does the work.** If a private function transitively reads the clock, it infers `reads(Clock)` with no annotation. A public function declares it, which is where nondeterminism belongs: callers may rely on the signature to know whether a function is deterministic. `#[no_effect(reads(Clock, RandomSource, Env))]` forbids all three below a boundary ([`#[no_effect]`](#no_effect)).

**Observability is exempt.** A clock, random or environment read performed inside an [observability operation](#observability), at any depth, is transparent too: a log line may carry a timestamp without giving its caller `reads(Clock)`. The rule is transitive because a one-level rule would break as soon as a helper was extracted. It applies only inside observability operations; any other function that reads the clock has `reads(Clock)`, so the exemption cannot launder a clock read into a deterministic function.

**Not resources in v1:** locale, scheduling order, thread and process identifiers. Locale may join `Env` when the library chooses a shape. Scheduling order is covered by the concurrency rules ([Determinism contract](#determinism-contract)). Replacing these resources with test doubles (providers) returns with the [services track](deferred.md#m4a-services).

#### User-declared resources

`effect resource Name;` declares a resource at module level, with the ordinary visibility rules. A user resource has no built-in operations. It enters the effect graph where a function declares it, and inference carries it to callers from there:

```kara
pub effect resource AuditLog;

pub fn record(line: Str) with writes(AuditLog) writes(Stderr) {
    eprintln(line);
}

fn handle(req: Request) -> Response {   // inferred: writes(AuditLog), writes(Stderr), ...
    record(req.summary());
    ...
}
```

Two `par` branches that both call `record` conflict on `AuditLog`. Parameterized resources (`UserDb[id]`) are deferred to the [effects-expressiveness track](deferred.md#effects-expressiveness).

### Inference and declarations

- **Private functions** infer their effects from their bodies and their callees, transitively. No annotation is needed. A private function may still declare an effect set, and a declared set is checked like a public one.
- **`main`** is private, so its effects are inferred ([§10](#10-errors-and-panics)). After a successful build the compiler prints the program's effect summary, the inferred set of `main`, and includes it as `"program_effects"` in `--output=json`.
- **Public functions declare their effects** with a `with` clause. The compiler infers the body's effects and checks that the declaration covers them. A mismatch is an error with a machine-applicable fix. A declaration may name more than the body does, so a library can reserve an effect; the `unused_effect` lint, allowed by default, reports such effects.
- **Default-permitted effects** (`allocates(Heap)` and `panics`) are inferred and shown, but never required in a declaration ([Default-permitted effects](#default-permitted-effects)).
- **Effects that arrive through a non-escaping function parameter, or through a type parameter's trait method that has no `with` clause, are not declared.** They are computed on each call's instance and charged to the caller ([Trait methods and generic calls](#trait-methods-and-generic-calls), [core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)):

  ```kara
  pub fn each[T](xs: Slice[T], f: Fn(T)) {
      for x in xs { f(x); }
  }

  fn report(rows: Vec[Row]) {          // inferred: writes(Stdout)
      each(rows, |r| println(f"{r.name}"));
  }
  ```

- **Inference is transitive and stops at public functions.** A call to a public function contributes its declared set; the compiler never looks through it. That bounds inference to one module at a time. The compiler caches each function's effect summary by its body and its callees' signatures, and re-infers only functions whose key changed.
- **Declarations are verified, not trusted.** A stale or wrong declaration is an error. The exception is an `extern` function, whose body the compiler cannot see: it has the effects it declares, none if it is declared `pure`, and every effect if it declares nothing ([§15](#effects-of-extern-functions), [core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)).
- **Inference may change across compiler releases** only under the edition rules of [§2](#2-specification-layers): within an edition a private function's inferred set may only shrink; across editions it may grow, with each change itemized.

**`public_effects` is a project setting** in `kara.toml`:

```toml
[project]
public_effects = "declared"   # default: every pub fn declares its effects, and they are verified
# public_effects = "inferred" # prototyping: pub fn effects are inferred and displayed, not declared
```

Under `inferred`, public functions infer their effects like private ones. `karac build` prints each public function's set in terminal and JSON output, and `karac doc` shows it beside the signature. No declaration is required and mismatch errors do not fire. Libraries should stay on `declared`: under `inferred`, the public effect set is not part of the API contract. The setting is per project; there is no per-function override. `#[no_effect]` applies in both modes, because it checks the inferred set. Trait method clauses ([Trait methods and generic calls](#trait-methods-and-generic-calls)) are not affected by the setting.

**Soundness defaults** ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)):
1. A call whose callee cannot be resolved statically has every effect.
2. An `extern` function has the effects it declares (none if it is `pure`), or every effect.
3. A drop's effects are those of the dropped type's drop glue, charged to the scope where the drop runs.
4. Recursive functions get their effects from a fixed point over their strongly connected component, with callees identified by definition, never by name.
5. A value-rooted resource is keyed by its value.

#### Effect lattice

Effect inference computes a least fixed point over a finite lattice. This gives the effect checker and `karac explain` an exact meaning for convergence, and the reason iteration over a recursive group terminates.

**Atoms.** The effect atoms of a compilation unit are

```
Atoms = ResourceAtoms ∪ { panics } ∪ ExecutionAtoms
```

where `ResourceAtoms = { verb(R) | verb ∈ {reads, writes, sends, receives, allocates}, R ∈ ResourcesInScope }` and `ExecutionAtoms = { blocks }`. `ResourcesInScope` is the finite set of resources reachable from the unit: built-in, declared, and value-rooted ones named in its signatures and bodies. Observability operations contribute no atoms.

**The lattice** is the powerset of `Atoms` ordered by inclusion:

```
L     = ⟨ P(Atoms), ⊆, ∪, ∩, ∅, Atoms ⟩
⊥     = ∅          no effects
⊤     = Atoms      every atom in scope
a ⊔ b = a ∪ b      join
a ⊓ b = a ∩ b      meet
```

`L` is a complete, distributive, finite lattice. [Effect-set subtyping](#function-values-and-effects) is exactly its order. Resource, `panics` and execution atoms share one carrier: a recursive group mixing `writes(Db)` with `blocks` is handled by one join.

**Transfer function.** For a private function `f` with body `B_f`, given an assignment `φ : PrivateFns → P(Atoms)`:

```
T_f(φ) = direct(B_f)
       ∪ ⋃ { φ(g)        | g ∈ callees(B_f), g private }
       ∪ ⋃ { declared(g) | g ∈ callees(B_f), g public  }
```

`direct(B_f)` is the atoms produced by the body's own operations: field writes, explicit sends, `panics` atoms, `extern` calls and drops. A public callee contributes a constant; it is looked up, never iterated.

**`panics` atoms**, the operations that put `panics` in `direct(B_f)`:
- `panic(msg)`, `todo()`, `unreachable()` ([§10](#10-errors-and-panics));
- the `assert` family ([§14](#14-testing));
- `unwrap()` and `expect(msg)`;
- indexing and slicing with `[]`, which may be out of bounds;
- integer `/` and `%`, which may divide by zero;
- arithmetic that may overflow in checked mode, the default ([core-semantics.md §10.1](core-semantics.md#10-panics-and-errors-c8));
- an access to a `mut` field of a `shared` value that carries a borrow flag ([§11](#11-ownership-and-sharing)).

Allocation failure is not a `panics` atom ([Default-permitted effects](#default-permitted-effects)).

**Monotonicity.** `T_f` is monotone in `φ`: if `φ₁(g) ⊆ φ₂(g)` for every private `g`, then `T_f(φ₁) ⊆ T_f(φ₂)`, because the right-hand side is a union over growing inputs and the other terms are constant. Monotonicity and finiteness give a unique least fixed point `φ*`, reached by iteration from `φ₀ = λg. ∅`.

**Iteration bound.** For a recursive group of `n` private functions and `m` atoms, the chain has length at most `n · m`: each step adds at least one atom to at least one member, and no member exceeds `m`. In practice most functions touch a few atoms and most groups have one or two members. The checker stops with an internal compiler error if a group exceeds `n · m` iterations; that is a guard against a future monotonicity bug, not an expected path.

**`karac explain`.** Asked why `f` has the set `φ*(f)`, the compiler reports the chain `∅ = φ₀(f) ⊊ φ₁(f) ⊊ … ⊊ φ_k(f) = φ*(f)` and, for each step, the callee or operation that added atoms. The `k ≤ n · m` bound keeps the chain short enough for one diagnostic.

#### Mutual recursion

Private functions that call each other form a strongly connected component of the call graph. Inference:
1. builds the call graph of the module's private functions;
2. finds the components;
3. processes them leaves first, so callees are done before callers;
4. iterates within each component from empty sets until nothing changes, which terminates by the bound above.

A public function is a firewall: its declaration is ground truth, and inference never flows back through it into a cycle.

**Diagnostic.** For each recursive group, the compiler emits a note listing the functions and their effects, in terminal output and under `"mutual_recursion_groups"` in `--output=json`. `#[allow(mutual_recursion_note)]` suppresses it.

**Not every cycle is recursion.** The inference graph has two kinds of edge: a **call**, and a mention of a function **as a value** ([§7](#7-functions-and-closures)). Both are real effect dependencies, so a cycle through either needs the fixed point and is reported. Only a cycle of calls is mutual recursion, though: two functions that store each other in a struct field never call each other. Each entry therefore carries:
- `"kind"`: `"mutual_recursion"` (every member reaches every other by calls), `"value_reference_cycle"` (every cycle passes through a function used as a value) or `"mixed"`;
- `"call_recursive_members"`: the members that recurse through calls: all of `functions` for `"mutual_recursion"`, none for `"value_reference_cycle"`, and the recursing subset for `"mixed"`.

`functions` and `resolution_trace` keep their meaning. Membership is not narrowed: the group is still reported, because the fixed point was still needed.

#### Annotation workflow

```
1. Write or change a public function's body.
2. The compiler infers the body's effects.
3. No declaration: an error with the annotation to add.
   A declaration that does not match: an error with the exact diff.
4. Apply the fix (an AI agent does this mechanically).
5. The compiler verifies, and the build is clean.
```

An effect declaration is not a design decision; it reflects what the code does, and the compiler says exactly what to write. Its value is in review: when a function's effects change, the diff shows it, and the reviewer sees which functions now touch new resources.

### Default-permitted effects

`allocates(Heap)` and `panics` are **default-permitted**:
- they are inferred and propagated like any other effect, and `karac explain` shows them;
- a `pub fn` never has to declare them, although it may;
- only `#[no_effect(...)]` forbids them.

They are substrate effects: nearly every non-trivial function allocates, and nearly every one indexes, divides or does checked arithmetic. Requiring them in signatures would put them on almost every function and bury the signal ([Design principles](#design-principles)).

**`allocates` is a gating effect, not a concurrency effect.** It never conflicts, even with itself, so it does not affect parallelism. It does two things: it makes heap use visible to `karac explain`, and it lets a boundary forbid allocation.

**`allocates` as a real-time guarantee.** A function under `#[no_effect(allocates(Heap))]` is proven allocation-free for the whole call: the checker follows the full transitive call graph and rejects any callee that allocates. Under the default rules, leaving `allocates` out of a declaration constrains nothing, because the permit keeps everyday signatures quiet. Languages without a static effect system cannot make this guarantee without external tools. Examples:
- **Game loops.** A per-frame callback must not stall on the allocator; a `#[no_effect(allocates(Heap))]` boundary on the loop body turns any allocation below it into a compile error.
- **Real-time audio callbacks.** A render callback that touches the heap causes glitches; the same boundary enforces the rule where the library is called, not at run time.

The guarantee is compositional: a single allocation anywhere below the boundary is a compile error during development, not a latency spike in production.

**Allocation failure** panics ([core-semantics.md §10.1](core-semantics.md#10-panics-and-errors-c8)). It is covered by `allocates(Heap)`, not by a separate `panics` atom. Code that must handle it calls the `try_*` methods, which return the failure as a value ([library/collections.md](library/collections.md#fallible-allocation)).

**`panics` is a control-flow capability.** It names no resource: a panic ends the process ([core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8)). `#[no_effect(panics)]` proves that nothing below a boundary panics; with `#[no_effect(allocates(Heap))]` beside it, allocation failure is excluded too.

### `#[no_effect]`

`#[no_effect(VERB, ...)]` on a function declares that the named effects are **absent** from its transitive effect set. Under the default rules, where `allocates(Heap)` and `panics` are permitted and so invisible in signatures, it is how one boundary opts back into the constraint. Project profiles, which forbid effects across a whole project, are deferred to the [systems track](deferred.md#systems).

```kara
// Heap use anywhere below this boundary is a compile error.
#[no_effect(allocates(Heap))]
pub fn render_frame(buf: mut Slice[f32]) {
    for i in 0..buf.len() {
        buf[i] *= 0.5;
    }
}

// Several effects in one attribute.
#[no_effect(allocates(Heap), panics)]
fn mix(a: f32, b: f32) -> f32 { a * 0.5 + b * 0.5 }
```

**The arguments use the grammar of a `with` clause**: `allocates(Heap)`, `panics`, `reads(Config)`. One grammar means the spelling that declares an effect and the spelling that forbids it cannot drift apart.

**Matching is by verb, then by resource if one is named.** A bare verb forbids every occurrence of it; a verb with resources forbids only those:

| Declaration | `allocates(Heap)` in the set | `allocates(Arena)` in the set |
|---|---|---|
| `#[no_effect(allocates)]` | rejected | rejected |
| `#[no_effect(allocates(Heap))]` | rejected | accepted |
| `#[no_effect(allocates(Arena))]` | accepted | rejected |

The bare form must be the broad one. `panics` and `blocks` take no resource, so reading a bare verb as "only resource-less occurrences" would make `#[no_effect(allocates)]` nearly a no-op while `#[no_effect(panics)]` worked: one spelling with two strengths.

**The check is transitive.** It uses the function's full effect set, so a function that calls an allocating helper has the helper's `allocates` and is rejected. It applies to a declared set (`pub fn f() with panics` together with `#[no_effect(panics)]` is a contradiction the checker reports) and to an inferred one. A violation is `error[E_NO_EFFECT_VIOLATED]`.

**Functions only.** The attribute constrains an effect set, and only a function has one; anywhere else it is `error[E_NO_EFFECT_INVALID_TARGET]`. An empty list (`#[no_effect]` or `#[no_effect()]`) is rejected, because an attribute that reads as a guarantee must never be a silent no-op.

### `blocks`

`blocks` means a call may park the OS thread in a kernel wait: while it waits, the thread does no other work. Examples: sleeping, synchronous file I/O, `Mutex.lock` under contention, channel `send` and `recv`, and a foreign function that waits on a condition variable. In v1 all I/O blocks the thread it runs on.

- **No resource.** A function either may block or it may not.
- **Never conflicts.** Two blocking branches may run in parallel.
- **Declared on public functions.** Unlike `allocates(Heap)` and `panics`, `blocks` is not default-permitted: whether a call may wait is a fact callers need. Private functions infer it.
- **Forbidden where waiting is unacceptable**, with `#[no_effect(blocks)]`.

**Where `blocks` comes from:**
1. **Standard library declarations.** Primitives such as `time.sleep`, synchronous file reads, `Mutex.lock` and the channel operations declare `blocks`.
2. **`extern` functions.** A foreign function has the effects it declares, or every effect, `blocks` included ([§15](#15-unsafe-ffi-and-layout-control)).
3. **Nothing else.** A loop that burns CPU is not blocking: it uses the thread rather than parking it.

**Why `blocks` is not a resource verb.** Occupying a thread is not reading or writing a named resource. Modeling it as `writes(Thread)` would push a scheduling fact into conflict checking and produce misleading diagnostics. Resource verbs answer "can these conflict?"; `blocks` answers "may this wait?".

**Diagnostics.** `karac explain f` traces `blocks` like any other effect, edge by edge: `f → g → time.sleep (declared blocks)`.

### Observability

**Logging, tracing, metrics and `dbg()` are transparent.** They never change a function's effect set and never conflict with anything, so adding a log line cannot change a signature or forbid parallelism. This is built into the language and the standard library; there are no user-defined effect verbs. `std.log`'s functions are declared with no effects. The standard library marks them with an internal attribute that user code cannot write, as it does with `#[compiler_builtin]` ([§15](#intrinsics)).

**No ordering between branches.** Because observability output does not conflict, output from concurrent branches may interleave. Within one branch it follows source order. Output that must be ordered, such as snapshot-tested text or logs consumed by other tools, goes through `println` or `eprintln`, which have `writes(Stdout)` and `writes(Stderr)` and therefore conflict across branches.

**`dbg()`** prints an expression's file, line, source text and value to stderr, then returns the value, so it can wrap any expression. It is removed from release builds (`karac build --release`). Use `print`/`println` (`writes(Stdout)`) or `eprintln` (`writes(Stderr)`) for intended output, and `dbg` for temporary instrumentation.

- **Terminal format** (the default), one line per call. Inside a `par` branch the line carries a task id, so interleaved output can be filtered:

  ```
  let result = dbg(compute(x));
  // stderr, sequential:     [src/main.kara:42] compute(x) = 17
  // stderr, in a branch:    [task:3 src/main.kara:42] compute(x) = 17
  ```

- **Structured format** (`--output=json` or `--output=jsonl`): one JSON object per call, per line. AI agents group and filter by `task_id` instead of relying on order. `task_id` is `null` outside concurrent code, and `kind` separates `dbg` events from compiler diagnostics in the same stream.

  ```json
  {"kind":"dbg","task_id":3,"file":"src/main.kara","line":42,"expr":"compute(x)","type":"i32","value":"17"}
  ```

- **How the format is chosen.** A compiled binary has no `--output` flag of its own, so it reads `KARAC_DBG_OUTPUT` (`json` or `jsonl` for structured, anything else for terminal); `karac run --output=json` sets it for the program it starts. The interpreter takes the mode from the flag directly.
- **Lines do not tear.** Each call writes its whole line in one `write(2)`, so lines up to `PIPE_BUF` bytes never mix, but lines from different branches may come in any order.
- **Snapshot-testing `dbg()` output is unsupported.** Use `eprintln` for stderr a test must capture.

**`log`** is the standard library's production logging: always present, with severity levels, structured fields and a configurable backend. Its API is in [library/log.md](library/log.md). `dbg` is for temporary instrumentation; `log` is for production. Both are transparent.

### Function values and effects

**A closure's effects are those of what it does when called**, not of what it captures. Capturing a `ref Db` gives the closure no `reads(Db)`; calling `db.query()` in its body does.

**A function type may carry an effect clause**: `Fn(T) -> U with reads(Log)`. The kind (`Fn`, `MutFn`, `OnceFn`), `escaping` and the clause are independent parts of the type ([§7](#7-functions-and-closures), [core-semantics.md §9.6](core-semantics.md#9-closures)).

**Effect-set subtyping.** Effect sets are ordered by inclusion. A function value with fewer effects may be used where one with more is expected: argument types are contravariant, return types covariant, and effect sets covariant.

```kara
fn run(f: Fn(i32) -> i32 with reads(Log) writes(Log)) -> i32 { f(0) }

fn reader(x: i32) -> i32 with reads(Log) { ... }
fn writer(x: i32) -> i32 with writes(Log) { ... }
fn pure_fn(x: i32) -> i32 { ... }

run(reader);    // OK: {reads(Log)} ⊆ {reads(Log), writes(Log)}
run(writer);    // OK
run(pure_fn);   // OK: the empty set is below every set
```

The reverse is an error: a `Fn(T) -> U with writes(Log)` cannot fill a slot declared `with reads(Log)`. Subtyping here is a relation on effect sets, not a coercion: nothing is converted at run time. `karac explain` reports the check as `effect-subset-ok: {reads(Log)} ⊆ {reads(Log), writes(Log)}`.

**How a call through a function value gets its effects** ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)):
- **A non-escaping parameter** (the default) has, at each call site, the effects of the argument passed there, charged to that caller. With no clause it accepts any function value. A clause, if written, is an upper bound the argument must fit.
- **An escaping value** (an `escaping` parameter, a struct field, a collection element) has the effects its type declares. Storing a value whose effects exceed the clause is an error. With no clause, a call through it has every effect.

```kara
struct EventBus {
    handlers: Vec[Fn(Event) with writes(OrderDb)],
}

impl EventBus {
    fn register(mut ref self, h: escaping Fn(Event) with writes(OrderDb)) {
        self.handlers.push(h);           // stored: checked against the clause, no effect here
    }

    fn emit(self, e: Event) {            // inferred: writes(OrderDb)
        for h in self.handlers { h(e); }
    }
}
```

**Default-permitted effects do not constrain a slot.** `allocates(Heap)` and `panics` are permitted in function types too, so a value that allocates or may panic fits a slot whose clause omits them: an `escaping Fn(Request) -> Response with reads(Db)` accepts a closure that allocates. A clause may still name them. A function type cannot require that its values do not allocate or panic.

**`blocks` is checked.** A value with `blocks` does not fit a slot whose clause omits `blocks`: "this callback must not wait" is worth expressing, and cheap to honor.

**Collections of function values.** When a collection literal holds function values with different effect sets, its element type takes their union; each element fits by subtyping:

```kara
// f1: reads(X); f2: reads(X) writes(Y)
let fs = [f1, f2];   // Vec[Fn() with reads(X) writes(Y)]
```

Effect variables for stored callbacks (`with E`) return with the [services track](deferred.md#m4a-services).

### Trait methods and generic calls

**A trait method may declare an effect clause.** The clause is a ceiling: each impl's method must stay within it, and a call through a type-parameter bound has the clause's effects, whatever the instance's impl does. Impls may narrow, never widen; this is effect-set subtyping at the method slot.

```kara
pub trait Storage {
    fn load(self, key: Key) -> Option[Value] with reads(Data);
    fn save(self, key: Key, value: Value) with writes(Data);
}

impl Storage for LocalFileCache {
    fn load(self, key: Key) -> Option[Value] with reads(Data) { ... }   // OK: matches
    fn save(self, key: Key, value: Value) with writes(Data) { ... }     // OK: matches
}

impl Storage for ReadOnlyView {
    fn load(self, key: Key) -> Option[Value] with reads(Data) { ... }
    fn save(self, key: Key, value: Value) with writes(Data) {           // OK: declares the
        panic("read-only view");                                         // ceiling, does less
    }
}

impl Storage for NetworkBackedStore {
    fn load(self, key: Key) -> Option[Value] with reads(Data) sends(Network) { ... }
    // error: `sends(Network)` is outside `Storage.load`'s `reads(Data)`
}
```

The diagnostic points at the impl method, names the extra effect and quotes the trait's clause. Exact matching would be too strict: `ReadOnlyView.save` never writes, yet it must still be a valid impl of a `writes(Data)` method.

**A trait method with no clause has no ceiling.** A call through a type-parameter bound has the effects of the instance's impl method, computed on the monomorphized instance and charged to whoever instantiates the generic function, exactly as calls through a non-escaping function parameter are ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)). This is how `Iterator.next`, the conversion traits and the operator traits work; the standard library writes them with no `with`.

**Default method bodies** are checked against their method's clause, by the same rules as impl bodies. **Associated functions** (trait items without `self`) follow the same rules as methods.

**Orphan impls stay within the ceiling.** A downstream package may implement a foreign trait for its own types, but it is bound by the trait's clauses. No downstream impl can widen a clause, so generic callers never see surprise effects through a new impl.

**Instances are keyed by types only.** An effect set never distinguishes two instances of a generic: effects change no code.

**`impl Trait`** in argument and return position is specified in [§9](#9-traits). Trait-level effect ceilings and the effects of `dyn Trait` calls arrive with the [`dyn` track](deferred.md#dyn).

### Design principles

**1. An effect is a capability the function exercises, not control flow imposed from outside.** Effects answer "what does this function reach for?". They do not encode outside interruptions, which use the return channel. That is why `panics` is an effect (the function, or a callee, may choose to stop the process) while cancellation, when it arrives with the services track, is not: a cancelled call sees `Err(Cancelled)` through its `Result`. Making cancellation an effect would widen every `Result`-returning function without telling the reader anything the `Result` does not.

**2. Substrate effects are permitted by default; scoped capabilities are declared.** A *substrate* effect is one that nearly every non-trivial function exercises; a *scoped capability* is a deliberate access to a named resource, such as `reads(UserDb)` or `sends(Network)`. Declaring a substrate effect everywhere would make the declaration noise, present on nearly every function and so informative about none. Declaring a scoped capability carries information because it is the exception. The substrate set in v1 is exactly `{allocates(Heap), panics}`. A program for which a substrate effect is scoped, such as a real-time or embedded one, forbids it with `#[no_effect]`. A proposal to add a substrate effect must show that the great majority of functions exercise it.

**3. An effect is transparent if and only if no other concurrent operation in the same program could depend on what it does.** This is the test for observability:
- logging and tracing append to a sink that nothing in the program reads back: transparent;
- metrics are commutative increments that nothing reads mid-flight: transparent;
- `reads(Db)` and `writes(Db)`: another operation may depend on the result: not transparent.

**Forward compatibility.** A future edition could invert the `allocates(Heap)` default and make allocation a scoped capability everywhere. That stays within principle 2, which allows the substrate set to be revisited. The migration would be a `karac migrate` rewrite that adds `allocates(Heap)` where a function transitively allocates, behind the edition mechanism.

### Deferred

These return with later tracks:
- effect groups, `stable` groups and effect semver rules; parameterized resources (`UserDb[id]`) with `alias` and `independent` ([effects expressiveness](deferred.md#effects-expressiveness));
- providers, `with_provider` and `providers {} in {}`; `suspends`; effect variables for stored callbacks (`with E`) ([M4a services](deferred.md#m4a-services));
- the web and host effect vocabulary ([web](deferred.md#web));
- `seq {}` ([M4b auto-par](deferred.md#m4b-auto-par));
- trait-level ceilings and `dyn Trait` effects ([`dyn`](deferred.md#dyn));
- project profiles that forbid effects across a project ([systems](deferred.md#systems)).

---

## 13. Concurrency

Concurrency in Kāra is explicit and verified. Three constructs start tasks, and nothing else does:
- `par { e1, e2 }` runs a fixed set of branches and gives a tuple of their values;
- `par for x in it { body }` runs one branch per element and gives a `Vec` of their values;
- `TaskGroup` runs a dynamic number of tasks, such as one per accepted connection, and joins them when it drops.

There is no free `spawn`, no `async fn` and no `.await`. A function that waits looks like one that computes; waiting is the [`blocks`](#blocks) effect.

The compiler verifies that the branches of a `par` block and the iterations of a `par for` do not conflict, using places and the effect system. A conflict is a compile error, not something the compiler quietly runs in order: running conflicting branches one after another without saying so would hide that the parallelism the programmer wrote did not happen. `TaskGroup` tasks are checked by the borrow rules and `CrossTask`, so they cannot race on memory, but they are not checked for effect conflicts and they run in no fixed order ([`TaskGroup`](#taskgroup)). The rules are [core-semantics.md §11](core-semantics.md#11-concurrency-c9); this section gives the surface and the examples.

v1 does not parallelize loops or statements on its own. Automatic parallelism arrives with the automatic-parallelism track (M4b), as an optimization that may change nothing observable ([core-semantics.md §11.5](core-semantics.md#11-concurrency-c9)).

### `par` blocks

`par { e1, e2, ... }` evaluates its comma-separated branch expressions concurrently and joins them at the closing brace. Its value is the tuple of the branch values, in source order. A branch may be a block.

```kara
fn load_dashboard(user_id: u64) -> Result[Dashboard, AppError] {
    let (profile, orders, notifs) = par {
        fetch_profile(user_id)?,
        fetch_orders(user_id)?,
        fetch_notifs(user_id)?
    };
    Ok(build_dashboard(profile, orders, notifs))
}
```

**Rules:**
- **Branches must not conflict.** Two branches conflict when one writes, moves, mutably borrows or drops a place or a resource that another reads or writes ([core-semantics.md §11.2](core-semantics.md#11-concurrency-c9)). Resources come from effects ([Conflict rules](#conflict-rules)), so two branches that both print conflict, and so do two that both send on one channel. The error, E0408, names both accesses.
- **Order and placement are free.** Once the branches are proven not to conflict, the runtime may run them in any order, in parallel or one after another ([§2](#2-specification-layers)). A target that cannot run them in parallel runs them in sequence with no source change ([§16](#concurrency-across-targets)).
- **What may enter a branch** is [core-semantics.md §11.3](core-semantics.md#11-concurrency-c9): owned values moved in, `sync` and `frozen` handles, and views. A `shared` handle may not ([What may cross a task boundary](#what-may-cross-a-task-boundary)).
- **Effects.** The enclosing function's effects include the effects of every branch.
- **Control flow stays in its branch.** `break`, `continue` and `return` may not cross out of a branch; that is a compile error. Loops inside a branch are fine. `?` is the way out (below).

**`?` in a branch pierces the block.** It ends that branch as an error exit, and once every branch is done the enclosing function returns the error ([Failure](#failure)). So:
- the block's type is the tuple of the branches' success values, never a `Result`;
- `?` is legal in a branch only where it is legal in the enclosing function, which must return `Result` or `Option` ([§10](#10-errors-and-panics));
- `?` at any depth inside a branch, in a nested `if` or loop, still returns from the enclosing function;
- applying `?` to the block itself is a type error, because the block is not a `Result`.

```kara
// ERROR: `transform` returns `Output`, so `?` has nowhere to return to
fn transform(data: Data) -> Output {
    let (a, b) = par { risky(data)?, other() };
    combine(a, b)
}

// ERROR: the block has type `(i64, i64)`, which `?` cannot apply to
fn process() -> Result[(i64, i64), StepError] {
    let pair = (par { step_a()?, step_b()? })?;
    Ok(pair)
}
```

**Borrows across branches** follow the ordinary rules. Several branches may read the same place; one branch may borrow it mutably if no other branch touches it; the borrow ends at the join.

```kara
// Two readers: both helpers borrow `data`.
fn process_in_parallel(data: Vec[i64]) -> (i64, i64) {
    par { sum_first_half(data), sum_second_half(data) }
}

// One branch borrows `log` mutably; the caller uses it again after the join.
fn append_concurrently(log: own Vec[String]) -> Vec[String] {
    let mut log = log;
    let (metric, _) = par { compute_metric(), append_log(mut log) };
    log.push(f"metric {metric}");   // OK: the borrow ended at the join
    log
}

// A mutable borrow and a read of the same place: rejected.
fn invalid_aliasing(log: own Vec[String]) {
    let mut log = log;
    par { append_log(mut log), read_log_count(log) }
    // error[E0408]: branch 1 mutably borrows `log`, and branch 2 reads it
}
```

**Nesting.** A branch may contain its own `par` block. The inner block joins before the branch that contains it ends.

```kara
fn nested() -> (i64, (i64, i64)) {
    par { step_a(), par { inner_x(), inner_y() } }
}
```

### `par for`

`par for x in it { body }` runs one branch per element of `it`. Its value is the `Vec` of the bodies' values, in iteration order, whatever order the branches finish in.

```kara
fn fetch_all(urls: Slice[String]) -> Vec[Result[Page, FetchError]] {
    par(limit: 50) for url in urls { fetch(url) }
}
```

- **Iterations are branches of one block**, so two iterations conflict by the rule for `par` branches. Iterations that write disjoint elements of one collection, as the compiler's disjointness analysis proves, do not conflict ([core-semantics.md §11.2](core-semantics.md#11-concurrency-c9)).
- **The loop variable** binds as in a `for` loop ([core-semantics.md §4.6](core-semantics.md#4-parameters-calls-and-patterns)): iterating a collection borrows it and binds `ref` elements, and `it.into_iter()` moves the elements in.
- **`par(limit: n) for`** runs at most `n` iterations at once. `n` is evaluated once, before the iterable expression, and `n <= 0` panics. Without a limit, the runtime chooses how many run at once.
- **`break`, `continue` and `return`** may not cross out of the body, as for a `par` branch.
- **`?` in the body** pierces the loop as it pierces a `par` block. The value is then the `Vec` of success values, and the enclosing function returns the error of the earliest failing iteration in iteration order.

```kara
fn load_all(paths: Slice[String]) -> Result[Vec[Config], ConfigError] {
    let configs = par for p in paths { parse_config(p)? };
    Ok(configs)
}
```

A structure that every iteration only reads is passed as a [frozen handle](#frozen-handles):

```kara
let g = freeze root;
let sums: Vec[i64] = par for depth in 1..5 { sum(g, depth) };
```

### `TaskGroup`

A `TaskGroup` runs a dynamic number of tasks. The API is in [library/concurrency.md](library/concurrency.md#taskgroup); the borrowing rules are [core-semantics.md §9.5](core-semantics.md#9-closures).

- **`group.spawn(|| ...)`** starts a task and returns a `TaskHandle[T]`, which owns the task's result. `handle.join()` waits for that task and returns its result.
- **Dropping the group joins every task it started.** So every task ends before the scope that owns the group ends.
- **A `TaskHandle` may outlive its group**, for example by being returned or stored. Joining it after the group has dropped returns the stored result at once. A result that is never joined drops with its handle, or with the group if the handle dropped first.
- **`TaskGroup.new(limit: n)`** bounds a group: `spawn` blocks while `n` of its tasks are running. Without `limit`, a group is unbounded.
- **`spawn` borrows the group** (its receiver is `self`), so a group can be passed borrowed to the functions and tasks that spawn into it, such as a server's request handlers.
- **The closure is not escaping.** It captures place by place, so a task may borrow from the enclosing scope. The group holds those borrows until it drops, every borrowed place must outlive the group, and a task spawned from inside another task captures that task's locals only by move ([core-semantics.md §9.5](core-semantics.md#9-closures)).
- **What a task captures and returns** must be able to cross a task boundary ([What may cross a task boundary](#what-may-cross-a-task-boundary)).
- **Tasks cannot race on memory, and are not checked for effect conflicts.** The borrow rules ([core-semantics.md §9.5](core-semantics.md#9-closures)) and `CrossTask` keep tasks from racing on a place. Tasks are not checked against each other for effect conflicts, and they run in no fixed order: a server's handler tasks may all print, or all write to one database. `par {}` and `par for` are the deterministic constructs, and they are the ones checked ([core-semantics.md §11.2](core-semantics.md#11-concurrency-c9)).
- **Effects.** A task's effects are part of the effects of the function that spawns it, as for any non-escaping function argument ([Function values and effects](#function-values-and-effects)).

A server that handles each connection in its own task, and passes a second group down so that handlers can start background work:

```kara
fn main() -> Result[(), ServerError] {
    let listener = TcpListener.bind("0.0.0.0:8080")?;
    let background = TaskGroup.new();
    serve(listener, background)
}

fn serve(listener: TcpListener, background: TaskGroup) -> Result[(), ServerError] {
    let connections = TaskGroup.new();
    loop {
        let conn = listener.accept()?;
        connections.spawn(|| handle_client(conn, background));
    }
}   // on an accept error, `connections` drops and joins every running handler

fn handle_client(conn: own TcpStream, background: TaskGroup) -> Result[(), ServerError] {
    let mut conn = conn;
    let req = read_request(mut conn)?;
    let receipt = req.receipt();
    background.spawn(|| send_receipt(receipt));   // `send_receipt` takes `own Receipt`: the local moves in
    write_response(mut conn, handle(req))?;
    Ok(())
}
```

Each handler moves its own `conn` in. `background` is declared in `main` and outlives both groups, so the handler tasks may borrow it.

**Collecting every result.** `par` blocks and `par for` loops without `?` already return every result, errors included, in source or iteration order ([Failure](#failure)). For tasks started one at a time, keep the handles and join them one by one.

### What may cross a task boundary

A value may move into a task, be returned from one, or be sent through a channel only if its type is `CrossTask`. The compiler computes this bound; user code cannot implement it ([§9](#crosstask)). It is checked at `par` branches, `par for` iterations, `TaskGroup.spawn` and channel sends.

**Task-local types** are not `CrossTask`:
1. **`shared` handles** (`shared struct` and `shared enum` values). Their count and borrow flags are not synchronized ([core-semantics.md §6.3](core-semantics.md#6-sharing-c7)). Use a [`sync` type](#sync-types) to mutate across tasks, or [freeze](#frozen-handles) the value to read it across tasks.
2. **Raw pointers** `*const T` and `*mut T` ([§15](#15-unsafe-ffi-and-layout-control)). The language makes no thread-safety claim about them. Transfer ownership of the data instead.
3. **Any type that contains one of these at any depth**: a field, an element, a tuple member, an enum payload or a closure capture. `Vec[Node]` is task-local when `Node` is a `shared` type.

**Every other type is `CrossTask`.** This covers:
- the primitive types, `String`, and the collections, tuples, `Option` and `Result` whose element types are `CrossTask`;
- owned user types whose fields are all `CrossTask`;
- `sync` and `frozen` handles;
- `Atomic[T]`, `Mutex[T]`, `Sender[T]` and `Receiver[T]`;
- `ref T` when `T` is `CrossTask`.

**Views** (`ref T`, `mut ref T`, `Slice[T]`, `Str`, and closures that capture by `ref` or capture a `frozen` handle) may enter `par` branches and `TaskGroup` tasks, where the borrow rules keep them valid. They may never go into a channel ([core-semantics.md §5.7](core-semantics.md#5-references-and-views-c5)). Whether a view may enter a task is decided by these view rules, separately from `CrossTask`.

A generic function that spawns or sends a `T` must require `T: CrossTask` in its signature, and the error is reported there ([§9](#crosstask)).

**Diagnostic shape.** One code, `E_NOT_CROSS_TASK`, covers every value that may not enter a task. For a `shared` handle captured directly, the fix diff is in [`sync` types](#sync-types). For a type that contains one, the diagnostic names the path to it:

```
error[E_NOT_CROSS_TASK]: `cache` (type `Server`) cannot cross a task boundary
  --> src/server.kara:42:9
   |
42 |         use_cache(cache),
   |                   ^^^^^ captured by a `par` branch here
   |
note: `Server` contains the shared type `Entries` at field path `inner.cache.entries`
help: declare `Entries` as a `sync struct`, or freeze the value if the branches only read it
```

**Comparison with Rust's `thread::scope`.** Both let a task borrow from the enclosing scope soundly, by different means:

| Aspect | Rust `thread::scope` | Kāra |
|---|---|---|
| Join guarantee | The scope joins its threads before it returns | A `par` block joins at its closing brace; dropping a `TaskGroup` joins its tasks |
| Borrow soundness | `'scope` and `'env` lifetimes bound the captures | The group borrows its tasks' captures until it drops, and they must outlive it ([core-semantics.md §9.5](core-semantics.md#9-closures)) |
| What may cross | `Send` and `Sync`, auto-traits that users may also implement | `CrossTask`, one bound computed by the compiler and never implemented by hand |
| Lifetime annotations | Required | None |

**Why this is sound.** A task's borrow is valid for the whole task: the task ends before its `par` block or its group does, and every borrowed place outlives that block or group. Only `CrossTask` values and views enter a task, and conflicting accesses to places are rejected (between `par` branches by the conflict rule, and between `TaskGroup` tasks by the borrow rules), so no two tasks can race on a place ([core-semantics.md §11.6](core-semantics.md#11-concurrency-c9)).

### Locks and atomics

`Mutex[T]` and `Atomic[T]` are the two ways to mutate state that several tasks share. Their API is in [library/concurrency.md](library/concurrency.md#mutext); a [`sync` type](#sync-types) holds its mutable state in them.

```kara
struct Totals { requests: i64, bytes: i64 }

fn add(totals: Mutex[Totals], n: i64) {
    let g = totals.lock();
    g.value.requests += 1;
    g.value.bytes += n;
}   // g drops here, releasing the lock
```

- **`m.lock()` returns a guard**, `MutexGuard[T]`, whose `value` field is a `mut ref T` to the protected value. The guard is a view of the mutex, and the lock is released when the guard drops at the end of its scope ([core-semantics.md §7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)). Discarding a guard is a [`#[must_use]`](#must_use) warning, because the lock would be released at once.
- **No poisoning.** A panic ends the process ([§10](#10-errors-and-panics)), so no task ever finds a lock left held by a failed one.
- **Effects.** `lock` has `blocks`, because it may wait. Code run while holding the guard has its ordinary effects. Atomic operations have no effects.

### Channels

A channel carries values from one task to another. Its API is in [library/concurrency.md](library/concurrency.md#channels): `channel(cap)` returns a `Sender[T]` and a `Receiver[T]`, the channel is bounded, and no channel operation panics.

The language-level rules:
- **A sent value moves** into the channel: `send` takes `value: own T`. Using it after the send is a use after move.
- **The element type must be `CrossTask`**, and no view may be sent ([What may cross a task boundary](#what-may-cross-a-task-boundary)).
- **Effects are keyed by the channel.** `tx.send(v)` has `sends(tx)` and `rx.recv()` has `receives(rx)` ([Value-rooted resources](#value-rooted-resources)). Clones of one `Sender` name the same channel. So two branches that send on one channel conflict, two that receive from one channel conflict, and a branch that sends and a branch that receives do not.
- **Fan-in uses `TaskGroup`.** Concurrent producers on one channel run as `TaskGroup` tasks, each with its own clone of the `Sender`. Tasks are not checked for effect conflicts, so their messages arrive in no fixed order; `par` stays deterministic.
- **`send` and `recv` have `blocks`**, because they may wait. On sequential `wasm_wasi`, `recv` yields instead, and panics with kind `deadlock` if no task can run ([§16](#concurrency-across-targets)).
- **The ends are ordinary owned values.** They can be moved into tasks, passed to functions and returned.

`select` across several channels, and send and receive timeouts, arrive with the services track (M4a).

### Failure

What happens when a branch fails is [core-semantics.md §11.7](core-semantics.md#11-concurrency-c9):
- **`?` in a branch** ends that branch as an error exit: its drops, `defer`s and `errdefer`s run ([§6](#6-expressions-and-statements)).
- **No branch is cancelled.** Every other branch runs to completion.
- **When every branch is done**, the enclosing function returns the source-earliest error (for `par for`, the earliest in iteration order), whichever failed first. The block's other values, errors included, drop in reverse source order.
- **A panic** in any branch or task ends the process with exit code 101. No drops, `defer`s or `errdefer`s run ([core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8)). A service that must survive a bad request runs under a supervisor that restarts it.

**Every result, not only the first error.** A block whose branches produce `Result` values without `?` returns all of them, in source order. This is the form for validating several fields and reporting every invalid one:

```kara
fn validate(form: Form) -> (Result[Name, FieldError], Result[Email, FieldError], Result[Age, FieldError]) {
    par { check_name(form), check_email(form), check_age(form) }
}
```

**`TaskGroup` tasks.** A task that returns `Err` does not affect its siblings. The error reaches whoever joins the task's handle.

**Planned for the services track (M4a):** cooperative cancellation, `TaskGroup.cancel()` and deadlines. Once a branch fails or a deadline passes, a sibling's next I/O or suspending call returns `Err(Cancelled)`, which converts into the branch's error type through `From` and propagates with `?`. Pure computation is never interrupted.

### Determinism contract

**The property.** Behavior that the effect system mediates is the same on every run: values returned, standard output and error, file contents, database state and panic messages. Snapshot tests are stable, and golden output compares byte for byte, without sequence numbers, locks or explicit ordering in the program. The property covers `par {}` and `par for`; `TaskGroup` tasks are outside it (below).

**The guarantees that give it:**
1. **Conflicts are errors.** No two branches touch one place or resource in a conflicting way, so every resource a branch writes is used by that branch alone, in that branch's program order. Two branches that both print are rejected rather than interleaved.
2. **Results are bound to positions.** A `par` block's tuple is in source order and a `par for` loop's `Vec` is in iteration order, whatever order the branches finish in.
3. **Errors are chosen by source order.** When several branches fail, the enclosing function returns the source-earliest error ([Failure](#failure)).
4. **Sequential code stays sequential.** v1 runs loops and statements in order. When automatic parallelism arrives (M4b), it may change nothing observable ([core-semantics.md §11.5](core-semantics.md#11-concurrency-c9)).

**What is not guaranteed:**
- the order in which branches, iterations and tasks run or finish;
- the order of effects from different `TaskGroup` tasks, which are not checked for conflicts ([`TaskGroup`](#taskgroup));
- the contents of a file that two branches write by path ([Built-in resources](#built-in-resources));
- the iteration order of `Map` and `Set`, which differs between runs of one program whether or not it is parallel ([library/collections.md](library/collections.md)); `SortedMap` and `SortedSet` are ordered;
- the state reached through `sync` handles, `Mutex` and `Atomic`: which task takes a lock first, and the order of atomic stores;
- the interleaving of [observability](#observability) output (`dbg`, logging) from branches that run at once, since it is not an effect;
- which panic is reported when two branches panic, since the first ends the process;
- values from [nondeterminism resources](#nondeterminism-resources), such as the clock and random numbers;
- effects that an `unsafe` block or an `extern` declaration hides from the compiler. That is a bug in the declaration, and declaring the effect restores the guarantee.

### Runtime

**The v1 model.** The language exposes tasks, not threads. On `native`, tasks run on a work-stealing pool of OS threads, and I/O blocks the thread it runs on. A blocked task holds its thread, which suits command-line tools, compilers, desktop applications and services with moderate concurrency. How each target runs tasks is [§16](#concurrency-across-targets). Non-blocking networking, with tasks that park instead of holding a thread, arrives with the services track (M4a); code written for v1 runs unchanged on it.

**The runtime is a library linked into every binary.** It is not a separate program or service. The compiler distribution contains `bin/karac` and the runtime archive, and every `karac build` links the runtime statically. A compiled binary depends only on the system C library and threads library: there is no runtime to install and no version to keep in step.
- **Why static.** A shared runtime would bring version skew between the runtime a binary was built against and the one installed where it runs, and a deployment dependency for every user. Static linking also allows link-time optimization across the runtime boundary. A shared-runtime option may come later; it is not specified here.
- **Why a library, not inline code.** The scheduler and synchronization grow over time. Generating them inline at each call site would risk diverging from the interpreter's semantics and would make every runtime change a codegen change. The compiler emits calls to runtime entry points, and the runtime owns the operating-system machinery behind them.

**Minimum scheduler invariants.** The language does not fix a scheduler, because targets need different ones. Every conforming scheduler satisfies these:
1. **No lost work.** A value a branch or task has computed is never discarded by the scheduler. It reaches the block's value, the function's returned error or the task's handle, or it drops as [Failure](#failure) says.
2. **Termination.** Every `par` block, `par for` loop and `TaskGroup` joins once its branches and tasks finish. The scheduler never parks one where it cannot make progress.
3. **Deadlock freedom.** The scheduler never adds a deadlock to a program that has none.

Behavior under overload (queueing or refusing excess work) and fairness between unrelated tasks are left to the runtime. Any answer that keeps the three invariants conforms.

### Deferred

- Automatic parallelism of loops and statements, its cost model and `seq {}`: [M4b auto-par](deferred.md#m4b-auto-par).
- Coroutines and `suspends`, the network event loop, `TaskGroup.cancel()` and cooperative cancellation, deadlines and `select`: [M4a services](deferred.md#m4a-services).

---

## 14. Testing

Tests live in `_test.kara` files and run with `karac test`. `karac build` ignores these files.

### Test blocks

A test case is a block at module scope: `test "case name" { body }`.

```kara
// math.kara
fn add(a: i64, b: i64) -> i64 { a + b }

// math_test.kara (same directory, so same module, with access to private items)
test "add positives" {
    assert(add(1, 2) == 3);
}

test "add across zero" {
    assert(add(-1, 1) == 0);
}

fn make_pair() -> (i64, i64) {      // a helper: not a test block, so not run as a test
    (1, 2)
}
```

**Discovery is structural, not by name.** `karac test` collects the `test "..." { }` blocks in `_test.kara` files. Free functions in those files are helpers, whatever they are named: `fn test_helper()` and `fn make_pair()` are both helpers. The block form is the only thing that makes a test a test.

Discovery by a name prefix (`test_`) fails silently in two ways: a test is skipped when the prefix is misspelled or forgotten, and an unrelated helper runs when it happens to carry the prefix. The block form fails loudly instead:
- a `test "..." { }` inside a function body is `E_TEST_BLOCK_NOT_TOP_LEVEL`;
- a misspelled keyword (`tset "..." { }`) is a parse error, not a missing test;
- `pub` or `private` on a test case is a parse error. A test case is not callable and has no identity outside its module.

**Identity and names.** A test case's source line identifies it within its module, so case names need not be unique. The case name is the string between `test` and `{`. It is what appears in every runner event and what filtering matches.

**A test file is part of its module.** `math_test.kara` beside `math.kara` is in the same module and can use its private items ([§4 Modules](#modules)).

### Assertions

`assert`, `assert_eq` and `assert_ne` are prelude functions. They need no import and may be used everywhere, production code included:

| Function | Signature | Failure message |
|---|---|---|
| `assert(cond)` | `fn assert(cond: bool) with panics` | `"assertion failed: <source text of cond>"`; the compiler captures the source expression |
| `assert_eq(a, b)` | `fn assert_eq[T: PartialEq + Debug](a: T, b: T) with panics` | `"assertion failed: a == b\n  left:  <a>\n  right: <b>"` |
| `assert_ne(a, b)` | `fn assert_ne[T: PartialEq + Debug](a: T, b: T) with panics` | `"assertion failed: a != b\n  value: <a>"` |

- `assert` takes a `bool` and has no bound. `assert_eq` and `assert_ne` need `PartialEq` to compare and `Debug` to print the operands ([§9 `Display` and `Debug`](#display-and-debug)). They borrow both operands, so neither moves: after `assert_eq(v, expected)`, `v` is still usable.
- A failed assertion panics ([§10 Panics](#panics)).

### Isolation

**Each test runs in its own process** ([core-semantics.md §10.4](core-semantics.md#10-panics-and-errors-c8)). A panic ends the process ([core-semantics.md §10.2](core-semantics.md#10-panics-and-errors-c8)), so a panicking test, including one whose assertion failed, fails alone, and the other tests still run. No state carries from one test to the next. This is not yet implemented: `karac test` currently runs all of a run's tests in one process.

### Runner output

`karac test` writes **JSONL to stdout**: one JSON object per line, one event per object. Each line has a `"type"` field naming the event, with the same envelope as the compiler's streaming events ([§17 Tooling contract](#17-tooling-contract)), so one client can read every JSONL stream `karac` writes. This is a stable public contract: CI dashboards, editor plugins, AI tooling and flaky-test trackers depend on it. A text mode may come later as `--output=text`; JSONL is the default.

| Event | Required fields | Notes |
|---|---|---|
| `run_start` | `total_tests: int` | Once, at the start of the run, after discovery. |
| `test_pass` | `test: string`, `duration_ms: int` | `test` is the case name, verbatim. |
| `test_fail` | `test: string`, `duration_ms: int`, `location: {file, line, col}`, `message: string` | Optional: `assertion: string` (the assertion's source text); `left: string` and `right: string` for `assert_eq` and `assert_ne`; `reason: string` for a failure that is not an assertion. |
| `test_skip` | `test: string`, `reason: string` | Reserved for skipped tests. Consumers must accept `reason` values they do not know. |
| `summary` | `total: int`, `passed: int`, `failed: int`, `skipped: int`, `duration_ms: int` | Once, at the end of the run. |

**Compatibility.** Later versions may add fields to an event, add event types, and add `test_skip` reasons. Consumers **must** ignore unknown fields and event types, and **must not** depend on the order of fields within a line. Field names and meanings do not change once shipped; a rename or a change of meaning is a breaking schema version.

**Exit code.** `0` if every test passed or was skipped. Non-zero if any `test_fail` was emitted.

**Filtering.** `karac test <substring>` runs only the cases whose name contains `<substring>`. Filtering happens after discovery. Filtered-out tests produce no events (not even `test_skip`), and `run_start`'s `total_tests` counts only the tests that remain.

Property tests, snapshot tests, fuzzing and benchmarks ([deferred.md](deferred.md#tooling)), and test attributes for services that need a live resource or a substitute implementation ([deferred.md](deferred.md#m4a-services)), are deferred.

---

## 15. Unsafe, FFI and layout control

This section covers the parts of Kāra that step outside what the compiler can check: `unsafe` code and raw pointers, compiler intrinsics, ABI layout with `#[repr]`, calls into C through `extern "C"`, and `host fn`.

**Who may use it.**
- The intrinsics (`std.intrinsics`) and `#[compiler_builtin]` are restricted to the standard library ([Intrinsics](#intrinsics)).
- `unsafe` blocks, `unsafe fn`, raw pointers, the `ptr` module and `MaybeUninit[T]` are available to every package, because a binding to C needs them. Their main user is the stdlib, which implements `Vec`, `String`, `Map` and the other collections in Kāra over them.
- A program that neither binds C nor implements a collection needs nothing in this section.

Layout blocks (data-oriented field grouping), `#[repr(packed)]`, `#[repr(align(N))]`, `offset_of`, FFI unions, exporting Kāra functions under the C ABI, volatile access, inline assembly, interrupt handlers, linker control attributes, calling conventions other than `"C"` and codegen hint attributes are deferred to the [layout](deferred.md#layout) and [systems](deferred.md#systems) tracks.

### `unsafe`

The `unsafe` keyword appears in three positions:

- **`unsafe { ... }` expressions**, for raw pointer arithmetic, unchecked casts and bypassing ownership.
- **`unsafe fn` declarations**, announcing that a function carries a precondition the type system cannot enforce (see [`unsafe_op_in_unsafe_fn` rule](#unsafe_op_in_unsafe_fn-rule)).
- **`unsafe extern "ABI" { ... }` blocks**, for foreign-import declarations (see [`unsafe extern` blocks](#unsafe-extern-blocks)). The block is the trust boundary at which the programmer asserts that the foreign signature, ABI and effect set describe the foreign symbol faithfully.

The rules below apply to the expression form. The other two positions have matching rules: an `unsafe fn` requires a `# Safety` doc-comment section, and an `unsafe extern { }` block requires the same section at the block level.

```kara
let p: *const T = ptr.const(value);     // construction is safe: no unsafe { } needed
// Safety: `value` outlives this block and is not mutated concurrently;
// offset 4 lies within the valid range of T's layout.
unsafe {
    let raw = p.offset(4);
    let val = *raw;
}
```

Pointer construction (`ptr.const(...)` / `ptr.mut(...)`) is safe. Dereference, arithmetic, unaligned access, pointee-changing casts and the cast that adds write capability (`*const T as *mut T`) require `unsafe { }`. The same-pointee weakening cast (`*mut T as *const T`) stays safe. See [Raw pointers](#raw-pointers) for the full namespace.

`unsafe` permits operations that ownership, bounds and type checking would reject. It does not switch off the effect system: the enclosing function's effects are still required.

#### `undocumented_unsafe` lint

Every `unsafe` block is an assertion by the author: "I have checked the invariant the safe parts of the language cannot check." The compiler warns, by default, when that assertion is undocumented.

**Rule.** An `unsafe { ... }` block is preceded by a line comment (or doc comment) whose first non-whitespace content after `//` begins with `Safety:` (case-insensitive), followed by at least one word:

```kara
// Safety: p points into a live buffer of at least one u32.
unsafe { *p }
```

Each `unsafe` block in a function needs its own comment: different invariants deserve different rationales.

**`unsafe fn` declarations** require a `# Safety` section in the doc comment:

```kara
/// Compute `x.offset(n)` without bounds checking.
///
/// # Safety
///
/// `n` must be within the allocation `x` points into.
unsafe fn offset_unchecked[T](x: *const T, n: i64) -> *const T { ... }
```

**`unsafe extern "ABI" { ... }` blocks** require the same `# Safety` section, attached to the block. It documents the foreign-side contract the declarations assert: typically the C header the bindings came from, the library's expected linkage and version, and any preconditions callers must uphold.

```kara
/// Bindings for the libfoo 2.x C API.
///
/// # Safety
///
/// Callers must ensure libfoo has been initialized with `foo_init` before
/// invoking any of these. The bindings track libfoo's symbol set as of
/// 2.4.0; older versions may be missing `foo_close`.
unsafe extern "C" {
    fn foo_init() -> i32 with allocates(Heap);
    fn foo_close() with writes(FileSystem);
}
```

**Suppression.** `#[allow(undocumented_unsafe)]` at the block, function or module level silences the warning for exploratory code. `#[deny(undocumented_unsafe)]`, or `-D warnings` in CI, makes it an error. The stdlib ships with the lint at warn level and a `Safety:` comment on every `unsafe` block.

#### `unsafe_op_in_unsafe_fn` rule

The body of an `unsafe fn` is **not** an implicit `unsafe { ... }` block. Unsafe operations inside the body still need their own `unsafe { ... }` block. The two uses of `unsafe` mean different things:

- **The `unsafe fn` declaration** announces a precondition to callers: "I have requirements the type system cannot verify; satisfy them before calling me." Callers acknowledge it at each call: `unsafe { foo() }`.
- **An `unsafe { ... }` block inside the body** is a trust assertion by the implementer at one operation: "I have checked the invariant at this exact step."

Treating the whole body as one implicit `unsafe { }` block would lose the per-operation rationale that makes each block auditable. A 50-line `unsafe fn` may contain one unsafe dereference and 49 lines of safe code; a reviewer needs to see which line is the trust boundary.

```kara
/// Read the `n`-th element of `xs` without a bounds check.
///
/// # Safety
///
/// `n` must be less than `xs.len()`.
unsafe fn read_unchecked(xs: Slice[i64], n: i64) -> i64 {
    let p = xs.as_ptr();            // safe
    // Safety: the caller guarantees n < xs.len(), so p.offset(n) is in bounds.
    unsafe { *p.offset(n) }
}
```

**Hard rule, no opt-out.** A bare unsafe operation directly in an `unsafe fn` body is a compile error, not a warning. There is no `#[allow(unsafe_op_in_unsafe_fn)]`. Each `unsafe { ... }` block carries its own `Safety:` comment per the [`undocumented_unsafe` lint](#undocumented_unsafe-lint).

**Calling another `unsafe fn` is itself an unsafe operation.** The call needs `unsafe { other_unsafe_fn() }`, even inside an `unsafe fn` body. The obligation an `unsafe fn` declares is owed by its callers; it does not pass on to callees that have their own preconditions.

```kara
/// Copy `n` values from `src` to `dst`.
///
/// # Safety
///
/// `src` and `dst` must be valid for `n` elements and must not overlap.
unsafe fn copy_unchecked[T: Copy](src: *const T, dst: *mut T, n: i64) {
    // Safety: the caller guarantees src and dst are valid for n elements and do not overlap.
    unsafe {
        for i in 0..n {
            dst.offset(i).write(src.offset(i).read());
        }
    }
}
```

One inner `unsafe { ... }` around a tight loop is the common pattern. The outer `unsafe fn` says "calling me has a precondition"; the inner block says "I have checked it for these operations."

**Interaction with `unsafe extern { }`.** Calling a function declared in an `unsafe extern { ... }` block is not an unsafe operation. The trust boundary for foreign imports is the declaration block, not each call (see [`unsafe extern` blocks](#unsafe-extern-blocks)). This is the deliberate asymmetry between `unsafe fn` (a precondition at every call) and `unsafe extern { }` (trust asserted once, at the declaration).

### Raw pointers

The raw pointer types are `*const T` and `*mut T`. Kāra constructs raw pointers through one namespace, `ptr`, and forbids casting a reference to a raw pointer. The two design goals:

1. **No accidental reference creation.** In Rust before 1.82 and in many C-like languages, `&value as *const T` looks like a pointer construction but first creates a reference, with that type's aliasing rules, and casts it second. If `value` is unaligned or aliased, the reference is already undefined behaviour before the cast runs. Kāra rejects the cast outright: raw pointers are constructed without a reference ever existing.
2. **One namespace for all pointer construction.** `ptr.const`, `ptr.mut`, `ptr.null`, `ptr.null_mut`, `ptr.dangling` and `ptr.dangling_mut` all live in the `ptr` module.

```kara
unsafe extern "C" {
    fn write(fd: i32, buf: *const u8, count: usize) -> isize with writes(FileSystem);
}

fn log_byte(b: u8) with writes(FileSystem) {
    let p: *const u8 = ptr.const(b);      // address of the local `b`, no reference created
    write(2, p, 1);
}

fn header_field_addr(h: Header) -> *const u32 {   // `Header` is not `Copy`, so `h` borrows
    ptr.const(h.field)                    // address of a field, no reference created
}
```

#### Construction: `ptr.const` and `ptr.mut`

**`ptr.const(place)` and `ptr.mut(place)` take a place expression, not a value.** The argument is parsed as an expression but typechecked as a place: a binding, field access, index expression or dereference. The operation evaluates to the address of that place, with the place's type as `T`. No reference is constructed and no temporary is materialised.
- If the argument is not a place: `error[E_PTR_CONST_REQUIRES_PLACE]: ptr.const requires a place expression (a binding, field, or index); the argument here is a value expression`.
- `ptr.mut` also requires the place to be mutably reachable: `error[E_PTR_MUT_REQUIRES_MUTABLE_PLACE]: ptr.mut requires a mutably-reachable place`.

**Casting a reference to a raw pointer is rejected.** The cast `r as *const T`, where `r` is a `ref T`, is well-formed syntax and a type error at the `as` operator: `error[E_REF_TO_RAW_PTR_CAST_FORBIDDEN]: cannot cast a reference to a raw pointer; use ptr.const(...) or ptr.mut(...) to construct a raw pointer directly without creating a reference`. The diagnostic carries a fix replacing the cast with `ptr.const(...)`. Only the origin casts `ref T` → `*const T` and `mut ref T` → `*mut T` are forbidden outright.

**Raw-to-raw casts split by what the cast asserts:**
- The same-pointee **weakening** cast `*mut T as *const T` is safe. It only drops write capability on the same allocation.
- The same-pointee **strengthening** cast `*const T as *mut T` (it mints write capability the source does not carry) and every **pointee-changing** cast (`*T as *U`, `T ≠ U`) require an `unsafe { }` block: `error[E_PTR_CAST_REQUIRES_UNSAFE]`.
- There is **no implicit `*mut T` → `*const T` coercion** at calls or assignments. The weakening is always an explicit, safe cast. The resulting `expected '*const T', found '*mut T'` mismatch carries a machine-applicable fix inserting ` as *const T`, so `karac fix` closes it mechanically. This is the shape every binding hits when it passes a filled buffer to a `*const` foreign parameter.
- Pointer-to-integer and integer-to-pointer casts are forbidden and go through the provenance API ([Pointer provenance](#pointer-provenance)).

**Construction is safe; dereference is unsafe.** Building a raw pointer with `ptr.const(...)` needs no `unsafe { }` block, because construction cannot cause undefined behaviour. These operations require `unsafe { }`:
- reading or writing through the pointer (`*p`, `p.read()`, `p.write(v)`);
- pointer arithmetic (`p.offset(n)`, where `n` is an `i64` count of elements, like every size and offset);
- unaligned reads (`p.read_unaligned()`);
- casts that change the pointee type.

Keeping construction safe means hot paths, such as taking a field address inside a tight loop, carry no per-iteration `unsafe { }` ceremony, while every actual access stays marked.

**The `ptr` module:**

| Function | Signature | Notes |
|---|---|---|
| `ptr.const(place)` | special form: `place: T` → `*const T` | Address of a place, immutable raw. |
| `ptr.mut(place)` | special form: `place: mut T` → `*mut T` | Address of a mutably-reachable place. |
| `ptr.null()` | `() -> *const T` | The null `*const T`. |
| `ptr.null_mut()` | `() -> *mut T` | The null `*mut T`. |
| `ptr.dangling()` | `() -> *const T` | A non-null but invalid pointer aligned to `T.align_of()`. For `MaybeUninit`-style placeholders. |
| `ptr.dangling_mut()` | `() -> *mut T` | The mutable counterpart. |
| `ptr.is_null(p: *const T) -> bool` | safe | Equality with the null pointer. |
| `ptr.addr(p: *const T) -> usize` | safe | Address as an integer, **without** exposing provenance. |
| `ptr.with_addr(p: *const T, addr: usize) -> *const T` | safe | Reseat `p`'s address to `addr`, keeping `p`'s provenance. |
| `ptr.with_addr_mut(p: *mut T, addr: usize) -> *mut T` | safe | Mutable counterpart. |
| `ptr.expose(p: *const T) -> usize` | safe | Address as an integer; **also** adds the provenance to the exposed set. |
| `ptr.expose_mut(p: *mut T) -> usize` | safe | Mutable counterpart. |
| `ptr.from_exposed(addr: usize) -> *const T` | unsafe | Synthesise a pointer from a previously exposed address. |
| `ptr.from_exposed_mut(addr: usize) -> *mut T` | unsafe | Mutable counterpart. |

`ptr.with_addr` and `ptr.with_addr_mut` are not yet implemented.

`null`, `null_mut`, `dangling`, `dangling_mut`, `from_exposed` and `from_exposed_mut` are ordinary generic functions. Their pointee type `T` comes from the expected type, usually an annotation (`let p: *const u8 = ptr.null();`), because Kāra has no call-site type arguments ([§8](#8-type-inference-and-generics)). Only `const` and `mut` are place-taking special forms.

**Why a `ptr` module rather than special syntax.** Rust's `&raw const x` form was rejected because the leading `&` makes it look like ordinary borrow creation, and the whole point is that it is not a borrow. A call-shaped form is unambiguous to a reviewer: `ptr.const(x)` is a pointer constructor. The cost, that `ptr` is a module name local bindings can shadow, is minor.

#### Pointer provenance

Kāra adopts the **strict-provenance** model (Rust RFC 3559) and forbids the silent `*const T as usize` / `usize as *mut T` casts. The model rests on two facts the cast form conflates:

1. **A pointer is not a number.** A pointer carries an **address** (the integer the CPU dereferences) and **provenance** (a static record of which allocation it came from, which alias analysis uses to bound where it can validly point). Casting a pointer to `usize` discards provenance; casting back must invent it, which is the central footgun.
2. **Two pointer-to-integer uses need different operations.** Pointer *tagging* reads the address bits, changes them and reseats them with the original provenance, never leaving the allocation. Pointer *round-tripping* through opaque integer storage (FFI, hash tables keyed on addresses, intrusive data structures) must preserve provenance through an explicit "expose" channel.

The `ptr` module provides both as named operations:

```kara
// Tagging: take the address, set the low bit, write it back. Provenance preserved end to end.
let p: *mut u32 = ptr.mut(my_buffer);
let tagged: *mut u32 = ptr.with_addr_mut(p, ptr.addr(p) | 1);
let cleaned: *mut u32 = ptr.with_addr_mut(tagged, ptr.addr(tagged) & !1);

// Round trip through opaque storage: the source provenance must be exposed explicitly.
let exposed: usize = ptr.expose(p);          // p's provenance is now in the exposed set
store_in_intrusive_table(exposed);
// ... later ...
let recovered: *const u32 = unsafe { ptr.from_exposed(read_from_table()) };
```

**`addr` versus `expose`.** Both return the same bits. `addr` is a pure read: the alias analyser may prove that `p` was never round-tripped. `expose` tells the analyser that the pointer's provenance has escaped and must be assumed reachable from any later `from_exposed`. Calling `addr` and then `from_exposed` on the same address is **undefined behaviour**, even though the integer is identical. Use `addr` for tagging and `expose` for round-tripping.

**`with_addr` versus `from_exposed`.** `with_addr(p, a)` keeps `p`'s provenance and replaces only its address. It cannot reach memory `p` could not already reach, so it is safe. `from_exposed(a)` synthesises a pointer with no static provenance; it may alias any allocation whose pointer was previously exposed, so it is `unsafe`. The caller asserts that the address came from a matching prior `expose` and that the pointee is still live.

**Cast rejection.** The four casts `*const T as usize`, `*mut T as usize`, `usize as *const T` and `usize as *mut T` are type errors:
- Pointer to integer: `error[E_PTR_TO_INT_CAST_FORBIDDEN]: cannot cast a pointer to an integer; use ptr.addr(p) for the address bits or ptr.expose(p) if the pointer will be round-tripped through integer storage`. The fix offers `ptr.addr(p)`, the weaker operation.
- Integer to pointer: `error[E_INT_TO_PTR_CAST_FORBIDDEN]: cannot cast an integer to a pointer; use ptr.with_addr(base_ptr, addr) to reseat an existing pointer's address, or ptr.from_exposed(addr) to round-trip a previously-exposed address`. There is no automatic fix: the choice depends on whether a base pointer is in scope.

Casts between integer types, and between two raw pointer types, are unaffected. Only crossing between pointer and integer is forbidden.

**Why only `from_exposed` is `unsafe`.** It is the one operation that can break alias analysis, because it claims provenance the compiler cannot verify. `addr` and `expose` only read a pointer. `with_addr` reseats an address but cannot escape the source pointer's provenance. The precondition on `from_exposed`: the integer came from a prior `expose` of a pointer to a currently live allocation, and the resulting pointer is used only within that allocation.

### Uninitialised memory: `MaybeUninit[T]`

`MaybeUninit[T]` is Kāra's only primitive for uninitialised memory. It distinguishes, at the type level, "storage typed as `T` whose bits are not yet a valid `T`" from `T` itself. Reads of a `MaybeUninit[T]` are not reads of `T`, so the compiler does not apply `T`'s invariants to them. Extracting a `T` requires `assume_init(own self) -> T` (or the borrow forms `assume_init_ref` and `assume_init_mut`), an `unsafe fn` with the precondition "the bits are now a valid `T`".

`MaybeUninit[T]` is not yet implemented.

```kara
// stdlib (sketch)
struct MaybeUninit[T] { /* opaque storage, sized like T, aligned like T */ }

impl[T] MaybeUninit[T] {
    pub fn uninit() -> Self            // valid in safe code: the wrapper hides the bits
    pub fn zeroed() -> Self where T: Pod  // valid in safe code, but only when zero bits are a valid T

    pub unsafe fn assume_init(own self) -> T
    pub unsafe fn assume_init_ref(self) -> ref T
    pub unsafe fn assume_init_mut(mut ref self) -> mut ref T

    pub fn write(mut ref self, val: own T) -> mut ref T
    pub fn as_ptr(self) -> *const T
    pub fn as_mut_ptr(mut ref self) -> *mut T
}
```

**No `mem.uninitialized()`.** Kāra never offers a primitive that hands out a `T` whose bits are uninitialised. Holding such a `T` is immediate undefined behaviour for every type with a validity invariant (`bool`, `char`, references, enum discriminants, `NonZero`, anything with a niche). The function does not exist under any name, and `import std.mem.uninitialized` is an unresolved-name error.

**`mem.zeroed()` is bounded by `T: Pod`.** Zero-filling is well defined exactly when the all-zeros bit pattern is a valid `T`. The stdlib `Pod` marker trait ([§9](#9-traits)) is the precise bound. It is implemented for the integer and float primitives, `bool` (zero is `false`), and tuples, arrays and structs of `Pod`. It is not implemented for any type with a validity invariant: references, function pointers, `NonZero`, enums with no zero-discriminant variant, `Option[ref T]`. With the bound, `mem.zeroed()` returns a `T` directly, in safe code, with `T` taken from the expected type:

```kara
let buf: Array[u8, 4096] = mem.zeroed();    // ok: u8 is Pod, all zeros is valid
let n: NonZero[u32] = mem.zeroed();         // error: NonZero[u32] is not Pod
```

Without the bound, `mem.zeroed()` would have the same hazard as `mem.uninitialized()`: a zeroed `Option[ref T]` is a null reference, which the type system says cannot exist.

**Stdlib hygiene.** Stdlib code that needs uninitialised storage uses `MaybeUninit[T]` only; no path produces a `T` from raw uninitialised bytes without `assume_init`. Use sites: `Vec.with_capacity(n)` allocates `n` slots of `MaybeUninit[T]` and writes into them as `push` is called; `MaybeUninit.uninit_array()` returns an `Array[MaybeUninit[T], N]`, with `T` and `N` from the expected type; `mem.replace(dest, src)` swaps without ever holding an uninitialised `T`. The `Array.uninit` primitive of [§5](#5-types) is `MaybeUninit.uninit_array` under a friendlier name.

### Intrinsics

The stdlib is written in Kāra and layered `core` / `alloc` / `std` ([library/README.md](library/README.md)). Underneath it sits a small set of intrinsics that the compiler implements:
- raw allocation and deallocation;
- pointer read, write and offset;
- `memcpy`;
- the size and alignment of a type;
- the runtime hasher;
- I/O.

**Rules.**
- The intrinsics live in the module `std.intrinsics`. Only standard-library packages may import it; an import from any other package is an error.
- Intrinsics that read or write raw memory are `unsafe fn`s, so each call sits in an `unsafe` block under the rules above.
- `#[compiler_builtin]` marks a standard-library declaration whose calls the compiler implements. It is rejected outside the standard library.

User code reaches this functionality only through the safe standard-library APIs built on it. `T.size_of()` and `T.align_of()` are such APIs: ordinary, safe functions that any package may call. Like every size, they return an `i64`.

### `#[repr]`: ABI layout

`#[repr]` controls **ABI layout**, for protocol correctness and C interop. v1 has three forms:

| Form | Effect |
|---|---|
| `#[repr(C)]` | C ABI layout: field order preserved, C padding rules. |
| `#[repr(u8)]`, `#[repr(i32)]` and the other integer types | Fix the enum discriminant representation. |
| `#[repr(transparent)]` | A single-field wrapper has the same layout as its field. See [`#[repr(transparent)]`](#reprtransparent). |

`#[repr(packed)]` and `#[repr(align(N))]` are deferred to the systems track.

```kara
#[repr(C)]
struct UsbDescriptor {
    length:           u8,
    descriptor_type:  u8,
    bcd_usb:          u16,
    device_class:     u8,
}

#[repr(u8)]
enum UsbClass {
    Audio    = 0x01,
    Hid      = 0x03,
    MassStorage = 0x08,
}

#[repr(transparent)]
distinct type Fd = i32;   // same layout as i32: safe for FFI newtypes
```

**Integer discriminant forms.** `#[repr(u8)]`, `#[repr(u16)]`, `#[repr(u32)]` and `#[repr(i32)]` cover the common C and C++ enum sizes. They are required on an `enum` passed across an FFI boundary, where the discriminant size is part of the ABI contract.

#### Enum discriminant runtime surface

C-like enums (no payload) expose their discriminant and variant list at runtime through a small, generated surface. Serialisers, command-line parsers, protocol encoders and FFI glue need it.

**`.discriminant()`.** Every C-like enum gets a generated `fn discriminant(self) -> D`, where `D` is the repr type (`u8` for `#[repr(u8)]`, `i32` for `#[repr(i32)]`, and `u32` for a C-like enum without `#[repr]`). The method is a read, has no effect, and is available wherever the enum is nameable. A method is used rather than an `as u8` cast because the cast would have to work on every enum-to-integer conversion, mixing the safe case (read the number you chose) with cases that need `unsafe`. A dedicated name keeps call sites self-documenting.

```kara
#[repr(u8)]
enum UsbClass { Audio = 0x01, Hid = 0x03, MassStorage = 0x08 }

let c = UsbClass.Hid;
let byte: u8 = c.discriminant();   // 0x03
```

**Payload enums have no `.discriminant()` in v1.** An enum with a variant carrying fields (`Shape.Circle { radius: f64 }`) does not get it. The compiler may elide the discriminant through niche optimization, lay out variants non-contiguously, and move the tag between versions; a reader would commit to layout stability. A protocol that needs a stable "which variant" integer on a payload enum writes `fn tag(self) -> u8` with an explicit `match`. Payload variants may still carry **declared** wire values under `#[repr(intN)]` or `#[repr(C)]` (see [Explicit discriminants on payload variants](#explicit-discriminants-on-payload-variants)); those values are the literals the `tag` method's arms use. Lifting this restriction later is a non-breaking addition.

**`TryFrom[D]` for integer-to-enum conversion.** Every C-like `#[repr(intN)]` enum gets a generated `impl TryFrom[intN] for Foo`, returning `Err(DiscriminantError.OutOfRange { value })` for a value that names no variant. A C-like enum without `#[repr]` gets no such impl, because its compiler-chosen discriminants are not stable. There is no `From[intN]`, because most integers are out of range; `TryFrom` is the honest signature.

```kara
let raw: u8 = read_from_wire();
match UsbClass.try_from(raw) {
    Ok(class)  => dispatch(class),
    Err(_)     => unknown_class_response(),
}
```

`DiscriminantError` is a stdlib error type (`enum DiscriminantError { OutOfRange { value: D } }`, parameterized by the repr type). There is no unchecked conversion in v1; adding one later is additive.

**`.values()`.** Every C-like enum gets a generated type-level function `fn values() -> Slice[Self]` returning every variant in declaration order: `UsbClass.values()` yields `[UsbClass.Audio, UsbClass.Hid, UsbClass.MassStorage]`. The slice is a view of static data, so it borrows nothing and may go anywhere ([core-semantics.md §5.3](core-semantics.md#5-references-and-views-c5)). It serves command-line listings, round-trip tests and tables. Payload enums do not have it: they have no finite set of values.

```kara
for c in UsbClass.values() {
    println(f"{c.discriminant()}: {c}");
}
```

**Semver lock.** Declaring `#[repr(intN)]`, even without explicit values, commits to stable discriminant *values* across minor versions.
- Adding a variant at the end is minor-version-safe.
- Changing a variant's value (explicit or compiler-assigned), reordering variants that lack explicit values, or removing a variant is a breaking change.
- A C-like enum without `#[repr]` has compiler-chosen discriminants that may shift between versions. A caller using `.discriminant()` on it relies on a local property, not a semver contract, and `karac explain` on such a call says so.
- Explicit values (`Audio = 0x01`) are part of the API, not only of the current release's codegen.

`#[repr(intN)]`, `.discriminant()`, the generated `TryFrom` and `.values()` are the complete v1 surface for C-like enums at protocol and FFI boundaries. Payload-tag exposure, an unchecked conversion and per-variant metadata are additive later.

#### Explicit discriminants on payload variants

Wire-protocol enums often give every opcode a fixed integer, whether or not it carries data: Modbus, USB descriptors, custom RPCs, CBOR-style tagged messages. Without a way to pin tag values on a payload enum, the only spelling left is a manual tag-plus-union struct, which gives up pattern matching and exhaustiveness for what is still a tagged enum. Kāra accepts explicit discriminants on payload variants under one repr contract.

**Surface.** Under `#[repr(intN)]` (`u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64`) or `#[repr(C)]`, any variant (unit, tuple or struct-shaped) may carry `= INT_CONST`:

```kara
#[repr(u8)]
enum Op {
    Reset                       = 0x01,
    Connect { addr: u32 }       = 0x05,
    Send(Bytes)                 = 0x06,
    Disconnect                  = 0xFF,
}
```

- The value is a constant expression ([§4](#4-modules-and-packages)): literals, arithmetic on literals, and references to other integer `const`s.
- Negative values are legal under a signed repr.
- A value outside the repr type's range: `error[E_DISCRIMINANT_OUT_OF_RANGE]`, naming the repr type and the value.
- A duplicate value: `error[E_DUPLICATE_DISCRIMINANT]`, naming both variants.

**All or nothing.** Within one enum, either every variant declares an explicit discriminant or none does: `error[E_PARTIAL_EXPLICIT_DISCRIMINANTS]: enum 'NAME' mixes explicit and implicit discriminants; declare a value on every variant or remove the explicit values and rely on declaration order`. The rule is load-bearing. Rust lets un-annotated variants count on from the previous explicit value, so inserting a variant in the middle silently shifts every later tag, which breaks downstream consumers with no source-level warning. Here every tag value in the source is the tag value on the wire.

**`#[repr]` requirement.** Explicit discriminants on a payload-carrying variant are legal only under `#[repr(intN)]` or `#[repr(C)]`. On an enum without `#[repr]`, or under `#[repr(transparent)]`: `error[E_PAYLOAD_DISCRIMINANT_REQUIRES_REPR]: explicit discriminants on payload variants require '#[repr(intN)]' or '#[repr(C)]'; without one, the discriminant location is unspecified and the value commitment is unreachable`. Field-less enums without `#[repr]` may still write `Audio = 0x01`.

**No runtime surface.** `.discriminant()`, the generated `TryFrom[intN]` and `.values()` stay unavailable on payload enums ([Enum discriminant runtime surface](#enum-discriminant-runtime-surface)). The explicit discriminant **declares the wire value**: the integer `N` stands for this variant in any external serialization. It does not fix where the discriminant byte lives in memory, and the compiler emits no reader or writer for it. Code that consumes the values writes a `tag` method:

```kara
impl Op {
    fn tag(self) -> u8 {
        match self {
            Op.Reset                  => 0x01,
            Op.Connect { .. }         => 0x05,
            Op.Send(_)                => 0x06,
            Op.Disconnect             => 0xFF,
        }
    }
}
```

The compiler does not check that the `match` literals equal the declared discriminants: synthesising that body would need a payload layout commitment that v1 does not make. The all-or-nothing rule and duplicate rejection still let a reviewer spot drift, since both sit at the type's definition.

**Semver lock.** As for field-less `#[repr(intN)]` enums, every declared value is public API. Changing a variant's value, removing a variant or removing the `#[repr]` attribute breaks callers; appending a variant with a fresh, non-conflicting value is minor-version-safe.

**Later.** A `#[derive(Discriminant)]` on a payload enum with all-explicit discriminants could generate `fn tag(self) -> intN` from the declared values and enforce consistency. It is not in v1, because it needs the per-variant payload layout commitment rejected above. Adding it later breaks no code.

#### `#[repr(transparent)]`

`#[repr(transparent)]` guarantees that a single-field wrapper has **exactly the ABI shape** of its field. The wrapper and its inner type are identical at every C-ABI boundary: as arguments, as results, as struct fields, behind `*const T`, and in slot layout. It lets distinct wrapper types take part in FFI without an unchecked conversion at every call.

**Why it exists.** Two needs meet in one attribute:

1. **FFI newtypes.** `Fd`, `Pid`, `MilliSeconds`, `SocketHandle`: kernel-ABI integers a program wants kept apart in the type system (so `close(get_pid())` is a compile error) but identical on the wire.
2. **The orphan-rule newtype escape.** Wrapping a foreign type in a local `distinct type` is the standard way to add a foreign trait ([§9](#9-traits)). When the wrapper is transparent, its layout is the foreign type's, so references round-trip through the wrapper without copies.

**Naming.** The attribute is `#[repr(transparent)]`, following Rust and the `repr` family. A separate `#[ffi_transparent]` was rejected: the orphan-rule use is not FFI, and two names for one mechanism would invite confusion.

**Permitted carriers:**
- **`distinct type Foo = Inner`**: the dedicated newtype mechanism, and the recommended form for FFI.
- **A single-field `struct`** (`struct Wrapper { inner: Inner }`). It may also carry zero-sized fields (see below).
- **A single-variant `enum` with one field** (`enum Foo { V(Inner) }`). Rare; included for parity with Rust.

**Forbidden carriers**, each rejected at typecheck:
- a `struct` with more than one non-zero-sized field: `error[E_REPR_TRANSPARENT_REQUIRES_SINGLE_FIELD]`;
- a `struct` with no fields: `error[E_REPR_TRANSPARENT_REQUIRES_FIELD]`;
- an `enum` with more than one variant, or with a payload-less variant: `error[E_REPR_TRANSPARENT_ENUM_NOT_SINGLE_VARIANT]`;
- `#[repr(transparent)]` combined with any other repr (`#[repr(C)]`, `#[repr(intN)]`): `error[E_REPR_TRANSPARENT_EXCLUSIVE]`. `transparent` *is* the layout claim; another repr would contradict or duplicate it.

**Zero-sized neighbours.** A transparent struct may carry further fields **only if each has size zero and alignment one**: `PhantomData[T]`, `()`, `Array[T, 0]`, an empty unit struct. The non-zero-sized field's layout becomes the struct's layout; the companions matter only to the type system (for example, `PhantomData[T]` ties the wrapper to `T`). The rule is per field: two non-zero-sized fields are rejected even if their sizes add up to one field's worth.

```kara
// OK: exactly one non-zero-sized field; PhantomData carries no bytes.
#[repr(transparent)]
struct Tagged[T] {
    inner: i32,
    _marker: PhantomData[T],
}
```

**FFI use.**

```kara
#[repr(transparent)]
distinct type Fd = i32;

unsafe extern "C" {
    fn close(fd: i32) -> i32 with writes(FileSystem);
}

fn close_socket(fd: Fd) -> i32 {
    close(fd.raw())   // .raw() is the explicit unwrap at the boundary
}
```

Passing a wrapper to a foreign parameter of the inner type needs an explicit `.raw()`. The ABI shapes are identical, but the type system still tells them apart, and the unwrap is the visible record at the call. To avoid the unwrap, declare the foreign signature in terms of the wrapper.

**Inner reprs pass through.** `transparent` does not stack with other reprs, but the inner type's repr carries through. A transparent wrapper around a `#[repr(C)] struct Inner` has `Inner`'s C layout. `#[repr(transparent)] distinct type Bytes = Array[u8, 16]` is FFI-compatible with `uint8_t[16]`.

**Orphan-rule escape.** A wrapper that only adds a foreign trait does not need `#[repr(transparent)]`: the orphan rule only requires a local type. `transparent` is recommended when the wrapper sits in FFI positions, or when outside tooling (debuggers, serialisers) treats the inner type's layout as the contract.

```kara
// Display is foreign (stdlib) and Vec[i64] is foreign: the orphan rule applies.
distinct type DisplayVec = Vec[i64];

impl Display for DisplayVec { /* ... */ }
```

**Derives.** `#[derive(...)]` on a transparent carrier follows the carrier's own rules: derives that walk the fields (`Eq`, `PartialEq`, `Hash`, `Clone`, `Copy`, `Debug`) work as on the underlying carrier. `Display` is not derivable. On a `distinct type`, the distinct-type derive rules apply ([§5](#5-types)).

### `extern "C"`

Kāra calls C through the C ABI. Foreign-import declarations (names of foreign symbols with no Kāra body) live inside an `unsafe extern "C" { ... }` block, and each declares its effects:

```kara
unsafe extern "C" {
    fn write(fd: i32, buf: *const u8, count: usize) -> isize
        with writes(FileSystem);
}
```

`"C"` is the only ABI in v1. Other ABI strings are reserved: writing one is a "not yet supported" compile error, never a silent fallback to `"C"`. A panic never crosses the boundary: a panic aborts the process ([core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8)). Exporting Kāra functions under the C ABI is deferred to the systems track.

`usize` and `isize` exist for FFI signatures; idiomatic Kāra uses `i64` for sizes ([§5](#5-types)).

#### `unsafe extern` blocks

Every foreign-import declaration lives inside an `unsafe extern "ABI" { ... }` block. Two item kinds are legal inside: foreign function declarations (`fn name(...) -> T with E;`, or `pure fn name(...) -> T;`) and opaque foreign types (`type Foo;`, see [Opaque foreign types](#opaque-foreign-types)). The `unsafe` keyword here is not an `unsafe { }` expression; it marks the declarations as a trust boundary the compiler cannot verify. The block's author asserts that:
- the declared signatures match the foreign symbols' actual ABI;
- the declared effects describe what the foreign code does (see [Effects of `extern` functions](#effects-of-extern-functions));
- each foreign symbol exists at link time and is the function the declaration claims;
- each opaque type names a real C type, and pointers the C side hands over are valid.

None of this is checkable. Putting every foreign import inside an `unsafe extern { }` block makes the trust boundary visible: everything inside is a foreign contract, everything outside is checked.

**Calls need no `unsafe { }`.** The trust assertion is made once, at the declaration. Calls are ordinary expressions, and the declared effects flow into the caller's effect set as usual. This is the deliberate asymmetry with [`unsafe fn`](#unsafe_op_in_unsafe_fn-rule), whose precondition is acknowledged at every call.

**No standalone form.** A single foreign import uses the block form with one item; `extern "C" fn name(...);` on its own is a parse error:

```kara
unsafe extern "C" {
    fn getpid() -> i32;     // no `with` clause: has every effect
}
```

The extra lines are deliberate: binding one foreign function has the same shape as binding a hundred, and related symbols grouped under one trust boundary is the intended idiom.

#### Effects of `extern` functions

The compiler has no body to analyse, so the declaration is the whole story ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10), item 2):
- An `extern` function's effects are exactly the effects in its `with` clause.
- An `extern` function declared `pure` has no effects.
- An `extern` function with neither has every effect.

```kara
unsafe extern "C" {
    pure fn strlen(s: CStr) -> i64;
}
```

Only `extern` declarations take `pure`, because only there does a missing `with` clause mean every effect rather than none.

**Trust, not verify.** Effects declared on an `extern` are declarations of record, not hypotheses to check. This is the one exception to the rule that declared and inferred effects must match ([§12](#12-effects)): there is no inferred side, because there is no body. A declaration of `fn foo() with reads(Network);` adds `reads(Network)` to every caller's inferred set, and the compiler does not verify that `foo` touches the network. A wrong declaration is the programmer's responsibility, and the `unsafe` on the block is the visible record of it.

```kara
// Compiles even though the C function touches a file, not a socket.
// The compiler cannot see foo's body; the declaration is trusted.
unsafe extern "C" {
    fn foo() with reads(Network);
}
```

**Declare every effect that applies**, including the ones that are easy to overlook:
- **`blocks`** on any C function that may park the OS thread: `sleep`, `read` on a blocking descriptor, `pthread_mutex_lock` under contention, synchronous DNS lookups, any call that waits on a kernel object. Undeclared blocking on a worker pool thread is the canonical way to freeze a Kāra runtime.
- **`panics`** on any C function that may end the process: `abort`, `exit`, `assert`, functions that may `longjmp` out or trigger a terminating signal handler. On an `extern`, `panics` means the function may terminate the program.
- **`allocates(Heap)`** on any C function that calls `malloc`, `calloc` or `realloc`, directly or transitively: most non-trivial C APIs (`getaddrinfo`, `strdup`, `fopen`, almost every parser library). It matters wherever `#[no_effect(allocates(Heap))]` forbids allocation ([§12](#12-effects)).

When in doubt, over-declare. A too-large effect set costs the caller some freedom; a too-small one is unsound and can break the runtime model silently. Omitting the `with` clause is the largest declaration of all. The compiler cannot reject an under-declared `extern`, because the declaration is trusted. The linter flags common omissions when a name matches a known blocking or allocating libc entry; the match is a hint, not a substitute for reading the C documentation.

FFI calls always run on real OS threads. The stdlib wraps raw `extern` declarations in safe, typed functions.

#### Linking foreign libraries

The `unsafe extern` block says *which symbols* a program imports. Where the library lives is a project and environment fact, so it goes in the manifest. A `kara.toml` `[link]` table supplies it:

```toml
[link]
libs = ["LLVM-18"]                              # -lLLVM-18
search-paths = ["/opt/homebrew/opt/llvm@18/lib"] # -L/opt/homebrew/opt/llvm@18/lib
```

- `libs` are bare linker stems (`"LLVM-18"`, not `"libLLVM-18.dylib"`), appended to the native `cc` link line as `-l<name>`.
- `search-paths` are appended as `-L<path>`, before the `-l` flags.
- Both follow the emitted object and the runtime archive on the link line, so the object's undefined foreign symbols resolve against these libraries.
- An absent or empty `[link]` table leaves the link line unchanged.
- The table applies to native targets only; wasm builds link with `wasm-ld`, which ignores it.
- The compiler does not check that a library exists when it reads the manifest. A missing library or path is an ordinary linker error at build time.

#### Opaque foreign types

Many C APIs hand back a *handle*: a pointer to a struct whose layout the library does not publish. `FILE*`, `xmlNode*`, `sqlite3*`, `EGLDisplay` and `cairo_surface_t*` are examples. Kāra binds them with an opaque type declaration inside an `unsafe extern { }` block:

```kara
unsafe extern "C" {
    #[kara_name("CFile")]
    type FILE;                   // C's `FILE`, named `CFile` in Kāra (CN-8)

    fn fopen(path: *const u8, mode: *const u8) -> *mut CFile
        with reads(FileSystem) writes(FileSystem);

    fn fclose(stream: *mut CFile) -> i32
        with writes(FileSystem);

    fn fread(buf: *mut u8, size: usize, count: usize, stream: *mut CFile) -> usize
        with reads(FileSystem);
}
```

**`type FILE;`** declares an unsized type, `CFile` on the Kāra side ([CN-8](#rules)), whose size, alignment and fields are unknown to Kāra. It is not a definition: no body, no fields, no methods, no derives. The C side guarantees the type exists and the pointers it hands over are valid; Kāra treats every `*mut FILE` as an opaque handle.

**Why a separate kind.** A `struct` has fields and a known layout. An opaque type has no known layout at all: the compiler cannot allocate one, copy one or compute its size. The only thing it supports is being pointed to. Modelling it as a struct with a placeholder field would either lie about the size or invent a layout the library is free to change.

**Permitted operations:**

| Operation | Status |
|---|---|
| Pointer types `*const Foo`, `*mut Foo` | Permitted wherever a pointer type is. |
| Reference types `ref Foo`, `mut ref Foo`, and a borrowed parameter `x: Foo` | Permitted; references are sized pointers and the borrow rules apply normally. |
| FFI signatures using the above | Permitted as parameter, return and field types, whenever the type is behind a pointer or reference. |
| `ptr.const(...)` / `ptr.mut(...)` on a place of type `Foo` | **Not** permitted. No place of type `Foo` can exist in Kāra code; opaque values only enter as `*mut Foo` from FFI. |

**Forbidden operations**, each rejected at typecheck:
- By-value bindings (`let f: CFile = ...`): `error[E_OPAQUE_TYPE_REQUIRES_INDIRECTION]: opaque foreign type 'CFile' has no known size; values of opaque types must appear behind a pointer or reference (*mut CFile, ref CFile, mut ref CFile)`.
- By-value parameters or returns (`fn f(x: own CFile)`, `fn f() -> CFile`): the same diagnostic.
- A by-value field (`struct S { f: CFile }`): the same diagnostic. A field behind a pointer (`struct S { f: *mut CFile }`) is fine.
- Field access through a `*mut CFile`: `error[E_OPAQUE_TYPE_NO_FIELDS]`. The compiler cannot know whether a field exists.
- Method calls: the same diagnostic. Methods cannot be defined on an opaque type.
- `CFile.size_of()`, `CFile.align_of()`: `error[E_OPAQUE_TYPE_NO_KNOWN_SIZE]`.
- Construction: no syntax names `CFile` as a value, so no diagnostic is needed.
- Pattern matching on an opaque place: as for field access.
- `#[derive(...)]` and `impl Trait for Foo`: `error[E_OPAQUE_TYPE_NO_INHERENT_OR_TRAIT_IMPLS]`. Trait conformance comes through a Kāra wrapper type (for example `distinct type SafeFile = *mut CFile` with `impl Drop for SafeFile`).
- Generics (`type Foo[T];`): a parse error, `error[E_OPAQUE_TYPE_GENERIC_FORBIDDEN]`. C has no generic types; a Kāra wrapper carries any parameter.

**Visibility.** An opaque type has the visibility of any other item and lives in the type namespace; `import lib.CFile` works. Several `unsafe extern { }` blocks may declare the same name only if all mean the same C type. A duplicate is `warning[W_OPAQUE_TYPE_REDECLARED]`, not an error, because the C side has the final word.

**Cleanup.** Opaque types have no `Drop` and no destructor; the language never calls one on a `*mut Foo`. C-side cleanup goes through the C function (`fclose`). The idiom is a Kāra wrapper that owns the pointer and calls the C cleanup in its `Drop`:

```kara
distinct type SafeFile = *mut CFile;

impl Drop for SafeFile {
    fn drop(mut ref self) {
        if not ptr.is_null(self.raw()) {
            fclose(self.raw());
        }
    }
}

fn open_file(path: CStr) -> Result[SafeFile, IoError] {
    let raw = fopen(path.as_ptr(), c"r".as_ptr());
    if ptr.is_null(raw) { return Err(IoError.from_errno()); }
    Ok(SafeFile(raw))
}
```

**Effects.** An opaque type declaration has no effects; it is a type. The functions that produce and consume its pointers declare theirs as usual ([Effects of `extern` functions](#effects-of-extern-functions)).

**Stability.** Because the layout is unknown, an opaque declaration survives any change to the C struct's fields. That is the point of opaque handles in C APIs. When a library *does* publish its layout in a header, bind it as a `#[repr(C)] struct` and gain field access. Declaring the same C type both ways (a `#[repr(C)] struct` and an opaque `type`) is a coherence error, reported at link time.

#### C strings

A C function that takes a `const char *` expects NUL-terminated bytes. Kāra `String` and `Str` values are not NUL-terminated, so their `.as_ptr()` must not be passed to such a parameter. Pass `.as_ptr()` of a `CStr` instead:
- a `c"..."` literal ([§3](#3-lexical-structure)) for constant text: type `ref CStr`, stored in read-only data, no allocation;
- a `CString`, built from a `String` with `to_cstring()`, for text known only at run time ([library/strings.md](library/strings.md)).

```kara
unsafe extern "C" {
    fn puts(s: *const u8) -> i32 with writes(Stdout);
}

fn greet(name: String) -> Result[(), NulError] {
    puts(c"hello".as_ptr());
    let c_name = name.to_cstring()?;
    puts(c_name.as_ptr());
    Ok(())
}
```

### Host functions

A `host fn` declaration names a function **provided by the compilation host**: a C-ABI library on native, the embedder on `wasm_wasi`. It is the one language-level surface for host-bound functions. The compiler lowers it per target, so a library author writes one declaration and the binding works on every target.

#### Why `host fn` exists beside `extern "C"`

`extern "C"` is Kāra's raw C-ABI door: a foreign symbol name, C-shaped parameters, a direct call on native. On WebAssembly a plain `extern "C"` gets no import entry, so it must resolve at link time.

`host fn` is one layer higher. The declaration is target-neutral, and the compiler lowers it:
- **native:** to an `extern "C"` call with the same signature, the same code path as a hand-written `extern "C"`;
- **`wasm_wasi`:** to a WebAssembly import entry in the **`kara_host`** import-module namespace, which the embedder supplies at instantiation.

The browser and Component Model lowerings arrive with the [web track](deferred.md#web); the `host fn` source surface is the same on all of them.

#### Syntax

```kara
effect resource Sensor;

#[derive(Clone, Copy)]
struct DeviceHandle { raw: i32 }   // an opaque-handle newtype

host fn device_open(id: i32) -> DeviceHandle with writes(Sensor);
host fn device_read(dev: DeviceHandle) -> i64 with reads(Sensor);
host fn log_line(text_ptr: *const u8, text_len: i64) with writes(Stdout);
```

No body: the declaration ends with `;`. It sits at module scope and takes attributes and visibility like any other item. A string crosses as a `(pointer, length)` pair.

#### Effects are required

Every `host fn` **must declare its effects** with a `with` clause; a `host fn` without one is an error. As for `extern "C"`, the declared set is a trusted declaration of record ([Effects of `extern` functions](#effects-of-extern-functions)), and callers inherit it through ordinary inference. The programmer declares every effect explicitly, including `blocks` and `panics` when they apply.

#### Parameter and return types

`host fn` parameters and returns are restricted to:
1. **Primitive types:** integer and float types, `bool`, `char`, `()`, pointer types.
2. **`Copy` types:** user-defined types that are `Copy` (small structs of primitives).
3. **Opaque-handle newtypes:** user-defined single-field structs wrapping a primitive, giving stronger types to host-allocated resources (file descriptors, device handles). The host identity lives in the single scalar field.

**Not permitted in v1:**
- **Owned non-`Copy` values**, as an `own T` parameter or a return. Moving a heap-owned Kāra value across a host call raises ownership-transfer questions that do not compose with host code.
- **Borrowed parameters**: a bare parameter of a non-`Copy` type, and `mut ref T`. Borrow rules are a Kāra compiler property; the host cannot be asked to honour them.
- **Generic `host fn` declarations.** Monomorphizing across the host boundary needs cooperation from the host-side binding layer that does not exist.

Richer interfaces are ordinary Kāra functions on top. A function that takes `text: Str` converts it to a `(pointer, length)` pair and calls the `host fn`.

#### Handles and ownership

Handles are plain scalars. Copying one increments no count; dropping one runs no destructor. The host owns the resource's lifetime. Libraries provide safe lifecycle management in ordinary code, for example a wrapper struct whose `Drop` calls a `host fn` release function.

#### Relationship to `extern "C"`

`extern "C"` stays. It is the low-level primitive for direct C-ABI binding and for cases where the programmer wants no compiler-mediated lowering. On native, `host fn` and `extern "C"` produce the same call for equivalent signatures. `host fn` is the recommended surface for anything that must be portable across targets.

#### Lowering on `wasm_wasi`

`karac build --target=wasm_wasi` lowers each `host fn` to an import entry in the `kara_host` namespace:
- **No glue file.** WASI hosts (wasmtime, node and others) each have their own import-object or linker interface. The embedder instantiates the module with both namespaces: `WebAssembly.instantiate(mod, { ...wasi.getImportObject(), kara_host: {...} })` under node, `linker.func_wrap("kara_host", "<name>", ...)` under wasmtime.
- **Boundary types are core WebAssembly types.** The contract is the module's import signatures: `i64`, `u64`, `isize` and `usize` as wasm `i64`; everything else, wasm32 pointers included, as `i32`, `f32` or `f64`. Strings cross as `(pointer, length)` pairs read from the exported linear memory. How these appear in the host language is the host's convention.
- **`extern "C"` gets no import entry.** An unresolved `extern "C"` declaration stays an undefined-symbol link error. Only `host fn` opts into host-provided resolution.

---

## 16. Targets and compilation

### Targets

#### The v1 target set

Kāra v1 has a **closed set** of compilation targets:

| Target | Purpose |
|---|---|
| `native` | Host-architecture executable or library through LLVM: CLI tools, servers, compute programs. |
| `wasm_wasi` | A WebAssembly module for WASI hosts (wasmtime, node and other headless runtimes). |

The set is closed so that the effect checker's target-to-resource table is finite and `karac check` has a bounded number of configurations. The [web track](deferred.md#web) adds `wasm_browser` and the [GPU track](deferred.md#gpu) adds `gpu`. Opening the set to user-defined targets is additive.

An item that exists on some targets only is marked with `#[cfg(target: ...)]` ([§4](#4-modules-and-packages)). There is no runtime `if target == ...` form.

#### Resources each target provides

Each target provides a subset of the built-in resources ([§12](#12-effects)). A function whose inferred effect set uses a resource the target does not provide is rejected when compiling for that target.

| Resource | `native` | `wasm_wasi` |
|---|---|---|
| `FileSystem` | ✓ | ✓ |
| `Stdin` / `Stdout` / `Stderr` | ✓ | ✓ |
| `Env` | ✓ | ✓ (limited) |
| `Network` | ✓ | ✓ |
| `Clock` | ✓ | ✓ |
| `RandomSource` | ✓ | ✓ |
| `Heap` | ✓ | ✓ |
| `ProcessTable` | ✓ | no |

User-defined resources have no target affinity. The target gate does not see a user resource directly, only the built-in resources it reaches transitively.

#### Effect-driven target gating

A function compiles for target `T` when its full inferred effect set uses only resources `T` provides. This is the main mechanism for cross-target correctness. Shared code that uses only target-agnostic effects is portable with no annotations.

```kara
// Target-agnostic: uses only `Network` and a user resource.
// Compiles for native and wasm_wasi.
pub fn fetch_and_parse(url: String) -> Result[Data, AnyError]
    with sends(Network) receives(Network) reads(Cache) blocks
{
    let raw = net.fetch(url)?;
    let cached = cache.get(url)?;
    merge(raw, cached)
}
```

A function that reaches `ProcessTable` (spawning a process, for example) compiles for `native` only. Compiling it for `wasm_wasi` is an error at the target-gate phase.

The compiler runs the target gate **after** effect inference, once per target in the build matrix. A function reachable from an entry point that uses a resource the target lacks is error `E0411`, which shows the chain from the entry point to the resource use. It is an error whenever the build or check is for that target; there is no warning form.

#### Checking several targets

When a package declares several targets, `karac check` type-checks and effect-checks the source tree **once per target**, with that target's resource set, and tags each diagnostic with its target.

- **Declaration.** The manifest declares `[build] targets = ["native", "wasm_wasi"]`, an array of target names. It is distinct from `[build].target`, a rustc-style triple that selects the per-target manifest overlay for `karac build`.
- **Errors.** Unknown names and duplicates are hard manifest errors. A soft warning would let a typo silently drop a target from a CI matrix.
- **Discovery.** `karac check <file>` finds the manifest by walking upward from the file's directory, the same rule as `karac run`.
- **Override.** `karac check <file> --targets=<list|all>`.
- **Each pass sees one target.** Each pass re-parameterizes the provided-resource set and the `#[cfg(target: ...)]` filtering, so it sees exactly what that target's build would see.
- **Deduplication.** A finding identical on every target is reported once, in an "all targets" group (text) or a `shared_diagnostics` array (JSON): it is a target-agnostic bug. JSONL output streams each target between `target_start` and `target_complete` events and leaves deduplication to the consumer.
- **One target.** A single declared target is not a matrix but still runs with that target's resource set: a `wasm_wasi`-only package is checked against `wasm_wasi`, not the host.

#### CPU baseline

Within the `native` target, karac keeps a **per-target-triple CPU baseline table** that mirrors `rustc`'s target defaults. LLVM's `"generic"` setting emits ARMv8.0-A on aarch64 and the original AMD64 baseline on x86_64: conservative, and right for fleets with varied CPUs. Where the hardware is curated (every `aarch64-apple-darwin` machine is Apple Silicon M1 or newer), the generic baseline is strictly worse, with no portability benefit.

| Target triple | Default `cpu` | Default `features` | Rationale |
|---|---|---|---|
| `aarch64-apple-darwin` | `apple-m1` | (M1 ISA implied by CPU) | Apple-controlled fleet; M1 is the lowest shipping Apple Silicon. M2/M3/M4/M5 are supersets. |
| `aarch64-unknown-linux-gnu` | `generic` | `+v8a,+outline-atomics` | Fleet variance (Pi, Graviton 1–4, NXP, Ampere) requires a conservative default. |
| `x86_64-unknown-linux-gnu` | `x86-64` | (none) | Original AMD64 baseline; broadest server compatibility. |
| `x86_64-apple-darwin` | `core2` | (none) | Matches `rustc` for the Intel-Mac target. |
| _(any other triple)_ | `generic` | (none) | Safe fallback: a portable binary beats one that will not load. |

**Contract.** A `karac build` with no override produces a binary that runs on every CPU the target triple promises. Raising the baseline (`apple-m1` to `apple-m4`, `x86-64` to `x86-64-v3`) narrows the deploy set for sharper code. That is the user's explicit choice, never a silent default change.

**CPU override**, in precedence order:
1. `--target-cpu=<name>` on `karac build`, in single-file and project mode;
2. `KARAC_TARGET_CPU=<name>`;
3. `[profile.release] target-cpu = "<name>"` in `kara.toml` (found by the `karac run` walk-up rule in single-file mode; the project's manifest in project mode).

An unknown CPU name for the active target is a hard error listing the supported names. (LLVM's own behaviour, warning and falling back to `generic`, is exactly the silent baseline loss this prevents.) `karac build --target-cpu=help` prints the annotated per-target list, including the wasm32 registry. An override replaces the CPU only; the table's feature string stays, so aarch64 Linux keeps `+outline-atomics` under `--target-cpu=neoverse-v1`. `--target-cpu=native` is available for deploys where the runtime hardware matches the build host; it is never the default.

**Feature override.** `--target-features="+feat,-feat,..."` adjusts the baseline's feature set, with the same precedence resolved independently of the CPU: `--target-features=<list>`, then `KARAC_TARGET_FEATURES=<list>`, then `[profile.release] target-features = "<list>"`.
- The list is appended after the table's default features, and LLVM resolves duplicates last-wins, so `-outline-atomics` really disables a table default.
- Every token needs an explicit `+` or `-` and must name a feature in LLVM's registry for the active target; anything else is a hard error with the supported list. `karac build --target-features=help` prints it.
- The two knobs compose: `--target-cpu=neoverse-v1 --target-features=-sve` builds the Neoverse baseline with SVE code generation suppressed.

The baseline is the floor every function compiles against. Per-function feature dispatch (multiversioning) is deferred to the systems track.

### Concurrency across targets

`par {}`, `par for`, `TaskGroup` and channels are specified once, at the source level ([§13](#13-concurrency)). How they run is a lowering choice per target; the source language does not change.

**Commitments on every target:**
1. **Task semantics.** Ownership transfer into a task, effects charged at the spawning site, and completion are the same on `native` and `wasm_wasi`.
2. **Channel ownership transfer.** A sent value moves to the receiver. Using it after the send is a compile error on every target.
3. **`par {}` and `par for`** mean the same on every target. A target that cannot run branches in parallel runs them one after another, with no source change.
4. **Data-race freedom** is a language guarantee ([core-semantics.md §11.6](core-semantics.md#11-concurrency-c9)) and holds on every target.

**Lowering:**

| Target | `par`, `TaskGroup` and channel lowering |
|---|---|
| `native` | OS threads and the runtime scheduler. |
| `wasm_wasi` | Cooperative and sequential, on the main thread. |

On `wasm_wasi` the runtime carries a cooperative FIFO scheduler. Spawning a task enqueues it, a join point runs the queue, and `par {}` runs its branches in source order. There are no extra threads and no shared memory.

Channel `send`, `recv` and `try_recv` work on every target. On `native`, `recv` blocks until a value arrives or every `Sender` has been dropped; a `Sender` moved into a task is dropped by that task, so a producer-consumer pair ends when the producer does. On `wasm_wasi`, `recv` on an empty channel yields to the other tasks; if no task can run, the program panics with kind `deadlock` ([Panic record](#panic-record)).

Shared-memory threads on WebAssembly (`--features wasm-threads`) and compiler-managed transparent threading on WebAssembly belong to the [web track](deferred.md#web). The commitments above keep both open without a source change.

### Build artifacts

`karac build --target T` produces a target-specific artifact set under `dist/`. The layout is stable, so downstream tools can integrate without per-project adaptation.

- **`native`.** A single executable, or a static or dynamic library, at `dist/native/<pkg>` (no extension on Unix; `.exe` on Windows). Library builds use the platform's archive format.
- **`wasm_wasi`.** A core WebAssembly module for WASI preview 1 at `dist/wasm/<pkg>.wasm`, with the package name from `kara.toml`; a single-file build writes `<stem>.wasm` in the working directory. The module exports `_start`, which runs `main`. Each `host fn` is an import in the `kara_host` namespace ([Lowering on `wasm_wasi`](#lowering-on-wasm_wasi)). The Component Model form, browser bindings and the export of other functions arrive with the [web track](deferred.md#web), and the `--bindings` option returns with them.
- **Release builds** run `strip -x` on the linked binary and pass linker dead-code elimination flags: `-Wl,-dead_strip` on macOS, and `-Wl,--gc-sections` with `-ffunction-sections -fdata-sections` on Linux ELF.

A structured manifest describing the artifact set (`.karapack`) is deferred to the [tooling track](deferred.md#tooling).

### Build profiles

`kara.toml` has two build profiles: `[profile.dev]` for ordinary builds and `[profile.release]` for `karac build --release`. Settings that differ between debug and release builds, such as `runtime_debug_metadata` ([Debugger contract](#debugger-contract)), go in these tables.

Project profiles, which restrict a whole project (`embedded`, `kernel`), are deferred to the [systems track](deferred.md#systems) and return under a different key.

### Compilation model

An AI author compiles often, and a slow compiler bottlenecks the loop. The architecture keeps fast iteration possible without requiring a full incremental compilation framework in v1.

#### Pipeline

```
lex → parse → desugar → resolve → typecheck → typed HIR
typed HIR → MIR builder (per function, monomorphised)
MIR → borrow check, drop elaboration, effect check
MIR → MIR interpreter (the reference semantics)
MIR → LLVM → object file → linked executable
```

- **Typed HIR.** Every node has a node id. Every item has a definition id, qualified by module and package. Types are interned, and each call records its resolved callee and type substitution.
- **MIR** is built per (definition, substitution), as a pure function of the typed HIR, so it can be cached. It has locals, places (a local plus a projection path), operands and rvalues; the statements `Assign`, `StorageLive`, `StorageDead` and `SetDiscriminant`; and the terminators `Goto`, `SwitchInt`, `Call`, `Return`, `Drop` and `Abort`.
- **No unwind edges.** A panic aborts ([core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8)). Every `Call` and `Drop` terminator carries a cleanup target, which is always `Abort` in v1.
- **Checks on MIR.** Borrow checking, drop elaboration ([core-semantics.md §7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)) and effect checking ([core-semantics.md §12](core-semantics.md#12-effects-soundness-defaults-c10)) run on MIR, on resolved calls.
- **The MIR interpreter is the reference semantics.** It tracks every allocation, so a double drop, a use after move, a leak or a read of a dead place is a deterministic error at a MIR location. It runs without LLVM.
- **MIR → LLVM** produces native code: type layouts, drop and clone glue, debug info and linking.

#### Principles

**1. Function-local analysis by default.** Type checking, effect inference (within a strongly connected component of mutually recursive private functions), ownership checking and monomorphization requests all work on function-sized units with explicit, named inputs. Whole-module traversals need a justification. This is the shape a future incremental layer would key on; whole-module sweeps would lock out function-keyed caching.

**2. The SCC is the cache unit for effect inference.** Effect inference already groups mutually recursive private functions into strongly connected components to reach a fixed point. That SCC is the natural invalidation unit for a future incremental layer, so the implementation keeps SCC boundaries.

**3. Named dependencies between phases.** Each analysis result records what it consumed: called function signatures, resource declarations in scope, trait impls visible at the call site, generic instantiations requested. A future incremental layer can then invalidate correctly without rederiving the dependency graph.

**4. No numeric speed targets before measurement.** Targets picked without baseline data ("100k lines/sec", "<500 ms incremental rebuild") are either trivially met or impossible. The MIR interpreter is not a performance target. First targets come from measurements on non-trivial Kāra programs.

**Known-risky passes.** These are the places where quadratic behaviour is easiest to introduce:
- **Monomorphization.** Kept function-local and lazy: a generic function is instantiated only for the type arguments a call site requests.
- **Effect inference SCC fixed point.** Each SCC iterates over a finite lattice; the bound (functions in the SCC times atoms in scope) keeps the pass linear in practice, and the computation stays local to the SCC.
- **Borrow checking and drop elaboration.** Dataflow over each function's MIR. It stays linear as long as it is function-local.

**Full incremental compilation is a future optimization**, not a v1 requirement. Query infrastructure with dependency tracking (Salsa-style) doubles compiler complexity. The principles above let that layer be added later without rewriting the passes.

#### Codegen containment

**The MIR → LLVM stage is the only part of the compiler that knows LLVM.** Every earlier stage, and the MIR interpreter, treats the backend as a black box. Syntax trees, types, effect records, ownership facts and MIR are plain data with no LLVM types inside.

This matters for three reasons:
1. **Backend independence of the analyses.** Type checking, effect inference and ownership checking are properties of the source program, not of the code generator. Coupling LLVM types into them would mix a backend representation into language-level analysis and make the analyses harder to test alone.
2. **A contained backend swap.** Moving to another code generation substrate (for example MLIR, in the [tooling track](deferred.md#tooling)) is surgery on one stage, not a compiler rewrite.
3. **One analysis pipeline, several consumers.** The MIR interpreter and the LLVM backend consume the same MIR.

**Maintainership invariant.** Contributors must not put LLVM types into syntax-level, analysis-level or MIR structures. A stage that needs to pass a hint to code generation (a layout decision, a vectorization annotation) does so through plain-data hint records that the LLVM stage consumes.

#### Observable phases

The streaming diagnostic protocol ([Streaming phase events](#streaming-phase-events)) reports these phases:

```
lex → parse → resolve → typecheck → effect → ownership → concurrency → codegen
```

Phase names are part of the public contract: renaming one is a breaking change for any client of `--output=jsonl`. Internal iteration within a phase is not observable. Internal stages report under these names: desugaring reports as `parse`, and MIR building and borrow checking report as `ownership`.

### Execution model

Kāra has one semantics, defined by the MIR interpreter, and two ways to run a program: on the MIR interpreter, or as native code from MIR → LLVM. Both execute the same MIR, so a program behaves the same under either; only speed differs.

#### `karac build`

The default path: source → MIR → LLVM object → native executable linked with `cc`, with the runtime archive linked statically. The optimization level is configurable (`-O0` to `-O3`; default `-O2`). The CPU baseline follows [CPU baseline](#cpu-baseline). Bounds-check elision ([Bounds checks and `get_unchecked`](#bounds-checks-and-get_unchecked)) is what lets loops vectorize.

#### `karac run`

`karac run` builds a program with the same semantics as `karac build` and then runs it. Every error `karac build` reports stops the run with exit code 1 before the program starts. Which backend executes the program, and so how fast it starts, is an implementation choice; the behavior is the same.

`karac run --interp` runs the program on the reference MIR interpreter.

#### `karac test`

`karac test` runs each test in its own process, so a panicking test fails alone ([§14](#14-testing), [core-semantics.md §10.4](core-semantics.md#10-panics-and-errors-c8)). `--interp` selects the MIR interpreter, as for `karac run`.

#### Panic locations

**Level 2: DWARF, instruction level (v1).** Every LLVM instruction is tagged with its source span. The panic path resolves machine addresses to source `file:line:col` through the emitted DWARF, so users see `panic at file:line:col in fn_name`.

**Level 3: rustc-style snippets (planned).** On the same DWARF data, panics print source snippets with caret pointers and node-aware messages (`this index operation panicked` rather than `panicked at <addr>`). It may follow v1 in a patch release rather than block it.

#### Bounds checks and `get_unchecked`

Slice and array indexing in safe code is bounds-checked at every access. Emitted naively, the check puts a side exit in every loop body, which stops LLVM's loop passes (autovectorizer, loop-invariant code motion, scalar replacement) from firing. The result is that `-O3` gains nothing over `-O2` on loop-heavy code.

**(a) LLVM-friendly bounds checks.** Each check is emitted in a form LLVM's range analyses (SCEV, GVN) can prove redundant when the index is in bounds by the loop structure:
- a compare and branch against the slice's `len`, with the panic block marked cold and the success-path range given to LLVM through `llvm.assume` where appropriate;
- for idiomatic loops (`for i in 0..xs.len() { xs[i] }`, nested strided loops), LLVM then hoists or removes the check, as it does for `rustc`;
- a constant index known to be in bounds (`xs[3]` with `3 < xs.len()` known statically) skips the check before LLVM sees the code.

This is invisible to users: the check is still semantically present in safe code.

**(c) The `unsafe { xs.get_unchecked(i) }` escape hatch.** When the programmer knows an index is in bounds but the compiler cannot prove it, `Slice[T].get_unchecked(i)` opts out. It is an `unsafe fn` taking an `i64` index and returning `ref T`, with the caller obliged to ensure `i < self.len()`, the same contract as Rust's `get_unchecked`. The `unsafe` block at the call documents the reason ([`undocumented_unsafe` lint](#undocumented_unsafe-lint)). Most cases close under (a); this is the recourse for the rest.

```kara
// (a): idiomatic loop; LLVM proves the bounds check redundant
fn sum(xs: Slice[i64]) -> i64 {
    let mut total: i64 = 0;
    for i in 0..xs.len() {
        total += xs[i]; // bounds check present in IR; LLVM removes it from the loop bound
    }
    total
}

// (c): manual escape hatch when (a) cannot prove the index
fn sum_strided(xs: Slice[i64], stride: i64, n: i64) -> i64 {
    let mut total: i64 = 0;
    for k in 0..n {
        // Safety: the caller guarantees k * stride < xs.len() for every k in 0..n
        unsafe { total += *xs.get_unchecked(k * stride); }
    }
    total
}
```

**(b)**, a compiler-side bounds-check elimination pass, is deferred to the [tooling track](deferred.md#tooling).

### Panic handler

A panic aborts the process, and no `Drop`, `defer` or `errdefer` runs ([§10](#10-errors-and-panics), [core-semantics.md §10.2](core-semantics.md#10-panics-and-errors-c8)). A custom panic handler is the only user code that runs on a panic: after the [panic record](#panic-record) is written to stderr and before the process exits with code 101. Programs that must release a hardware handle, feed a watchdog or dump telemetry use it.

A program defines at most one handler. The `#[panic_handler]` attribute alone enables it; there is no manifest setting.

```kara
#[panic_handler]
fn on_panic(info: PanicInfo) {
    Watchdog.feed_one_last_time();
    eprintln(f"PANIC: {info.message} at {info.location}");
}
```

- The handler borrows a `PanicInfo` and runs once for the whole program, not once per frame. A single coordination point is the right shape for this job.
- A panic inside the handler ends the process at once, with exit code 101. The handler is not run again.
- It cannot resume execution. When it returns, the process exits with code 101.

---

## 17. Tooling contract

Kāra is designed to be written by AI. The compiler's outputs are therefore a contract, not a convenience: structured diagnostics with machine-applicable fixes, a streaming event protocol, queries over what the compiler inferred, `karac fix` and `karac explain`, a debugger contract, the panic record and canonical formatting. Whether each output is guaranteed or only reported follows [§2](#2-specification-layers).

### Structured compiler output

All diagnostics are available as JSON with machine-applicable fix diffs. AI agents apply routine fixes mechanically: effect annotations, ownership adjustments, missing pattern arms.

```bash
$ karac build --output=json
{
  "program_effects": ["reads(FileSystem, Env, Stdin)", "writes(Stdout, Stderr, UserDB)", "sends(Network)"],  // null if build did not reach effect analysis
  "diagnostics": [{
    "id": "d1",
    "severity": "error", "primary": true,
    "code": "E0500", "category": "ownership",
    "concept": "ownership/move-semantics",
    "file": "src/order.kara", "line": 42, "column": 9,
    "message": "`user` moved on line 38, used again here.",
    "hints": [{
      "description": "clone `user` before the move",
      "diff": {"file": "src/order.kara", "line": 37,
               "old": "process(user);", "new": "process(user.clone());"}
    }]
  }, {
    "id": "d2",
    "severity": "error", "primary": true,
    "code": "E0400", "category": "effects",
    "concept": "effects/public-function-declaration",
    "file": "src/order.kara", "line": 12, "column": 1,
    "message": "public function `process` effect declaration incomplete",
    "inferred_effects": ["reads(UserDB)", "writes(Cache)"],
    "declared_effects": ["reads(UserDB)"],
    "derivation": [
      {"file": "src/order.kara", "line": 12, "reason": "process calls update_user"},
      {"file": "src/user.kara",  "line": 47, "reason": "update_user calls cache.store"},
      {"file": "src/cache.kara", "line": 89, "reason": "cache.store has writes(Cache)"}
    ],
    "hints": [{
      "description": "add missing effect to declaration",
      "diff": {"file": "src/order.kara", "line": 12,
               "old": "pub fn process(id: u64) -> Report with reads(UserDB) {",
               "new": "pub fn process(id: u64) -> Report with reads(UserDB) writes(Cache) {"}
    }]
  }]
}
```

Diagnostic codes come from per-phase bands, and every code is catalogued for `karac explain` ([§2](#2-specification-layers)).

#### Diagnostic quality commitments

The JSON document is the compiler's only output channel for signal. Five rules govern its shape.

**1. Signal on stdout, noise on stderr.** The JSON document goes to stdout and holds only actionable information: errors, warnings and the `program_effects` summary. Build progress ("Compiling foo", "Linking..."), phase timings and cache messages go to stderr in human-readable form. AI agents read stdout and parse one JSON document with nothing to filter. Developers running `karac build` interactively see progress on stderr. Timings and profiling data, when requested, go to a separate file (`--emit-timings=timings.json`), never into the diagnostic stream.

**2. Root-cause grouping with `primary` and `consequence_of`.** Every diagnostic carries an `"id"` and `"primary": true | false`. A consequence (caused by an earlier root error, such as `user.name` being inaccessible because `user` was moved) carries `"primary": false` and `"consequence_of": "<id>"`. AI agents fix primaries first; consequences often disappear once the root is fixed.

```json
{ "id": "d1", "primary": true,  "code": "E0500", "message": "`user` moved on line 38..." }
{ "id": "d2", "primary": false, "consequence_of": "d1",
  "message": "`user.name` not accessible: `user` already moved" }
```

**3. A cascade cap of 20 primary errors per file.** When a file reaches the cap, the compiler emits a `"truncated"` marker and reports nothing further for that file; other files continue normally. Consequences do not count against the cap. The cap applies only to `severity: "error"`; warnings are never capped.

```json
{ "truncated": { "file": "src/order.kara", "suppressed_primary_errors": 14 } }
```

A hard cap, rather than stopping at the first error, lets AI agents apply independent fixes in one pass. Twenty primaries is enough for parallel fixing and small enough to avoid a wall of errors.

**4. A `concept` field linking to the specification.** Every diagnostic carries `"concept": "<id>"`, a stable string such as `"ownership/move-semantics"` or `"effects/inference-boundaries"`. The [concept table](#concept-ids) maps each id to the section that specifies it, so an AI agent fetches the authoritative section without guessing. Ids are used instead of URLs because URLs rot. When this document is reorganized, the table changes and the ids do not.

**5. A `derivation` chain for inferred diagnostics.** A diagnostic that comes from transitive inference (effect inference, ownership checking, type inference) includes an ordered `"derivation"`: a list of `{file, line, reason}` entries showing the compiler's chain of reasoning. For an effect error it is the call chain that accumulated the unexpected effect.

| Commitment | Errors | Warnings |
|---|---|---|
| `primary` / `consequence_of` | required | optional |
| Cascade cap (20 per file) | errors only | no cap |
| `concept` field | required | required |
| `derivation` chain | when inferred | when inferred |

Performance notes and concurrency reports join this table when their tracks return.

#### Concept ids

| Concept id | Section |
|---|---|
| `ownership/move-semantics` | [§11 The core rules in brief](#the-core-rules-in-brief); [core-semantics.md §3](core-semantics.md#3-moves) |
| `effects/public-function-declaration` | [§12 Inference and declarations](#inference-and-declarations) |
| `effects/inference-boundaries` | [§12 Inference and declarations](#inference-and-declarations) |
| `ownership/rc-fallback` | Retired: there is no implicit sharing ([core-semantics.md §6.4](core-semantics.md#6-sharing-c7)) |
| `performance/allocation-hoisting` | Deferred with performance diagnostics to the [tooling track](deferred.md#tooling) |

A new id is added to this table when a diagnostic first uses it.

#### Error return traces

When `?` propagates an `Err` through several call frames, debug builds record the path. The trace appears with the diagnostic in `--output=json` and as a note in terminal output. A stack trace shows where execution was; an error return trace shows where the error travelled.

```json
{
  "diagnostics": [{
    "severity": "runtime_error",
    "message": "Err(DbError.ConnectionTimeout) propagated to top level",
    "error_return_trace": [
      { "file": "src/db.kara",   "line": 84, "expr": "conn.query(sql)?" },
      { "file": "src/repo.kara", "line": 31, "expr": "db.find_user(id)?" },
      { "file": "src/api.kara",  "line": 12, "expr": "repo.get_user(id)?" }
    ]
  }]
}
```

- **Debug builds only.** `--release` strips the bookkeeping. There is no knob: this is a development tool.
- **Implementation.** A thread-local ring buffer of `(file, line, expr_text)` entries, 64 deep by default. Each `?` site calls a compiler-generated intrinsic that pushes an entry on `Err` and clears the buffer on `Ok`.
- **Depth limit.** When the buffer is full, the oldest entry is dropped and the output sets `"trace_truncated": true`. The trace always shows the most recent frames.
- **A chain split across parallel tasks reports that it is unavailable.** The buffer is per thread, so when a `?` chain runs across `par` branches or `TaskGroup` tasks on the worker pool, no single buffer holds the whole chain. The output then reads `Error return trace: unavailable (the ? chain was split across parallel tasks)` instead of a fragment, because which frames land on the exiting thread varies from run to run. Carrying the trace across tasks is deferred.

### Streaming phase events

The batch `--output=json` document is emitted once, at the end of a build. A client that iterates quickly, or wants to act on one phase's result while the next runs, uses the streaming mode, which emits one event per line as the build progresses. Everything the batch mode produces is also in the stream, and a complete stream reconstructs the batch document.

**Invocation.** `karac build --output=jsonl`. The name follows the JSON Lines convention.

**Format.** Each line is one JSON object, terminated by `\n` (not CRLF), UTF-8, not pretty-printed, with a `"type"` field naming the event. There is no wrapper object or array. The stream works with `tail -f` and line-by-line JSON parsers.

```
{"type":"build_start","build_id":"01HQ...","timestamp":"2026-04-11T10:23:04Z"}
{"type":"phase_start","phase":"lex","scope":{"files":["src/main.kara"]}}
{"type":"phase_complete","phase":"lex","errors":0,"warnings":0,"notes":0}
{"type":"phase_start","phase":"parse"}
{"type":"diagnostic","phase":"parse","id":"d1","primary":true,"severity":"error","code":"E0099","message":"unexpected token `}`","file":"src/main.kara","line":12,"column":3}
{"type":"phase_complete","phase":"parse","errors":1,"warnings":0,"notes":0}
{"type":"phase_skipped","phase":"typecheck","reason":"parse errors in input","blocking":["d1"]}
{"type":"phase_skipped","phase":"effect","reason":"typecheck did not run","blocking":["d1"]}
{"type":"phase_skipped","phase":"ownership","reason":"typecheck did not run","blocking":["d1"]}
{"type":"build_complete","build_id":"01HQ...","success":false,"total_errors":1,"total_warnings":0,"program_effects":null}
```

**Event types (the minimum set of six):**

| Event | Cardinality | Payload |
|---|---|---|
| `build_start` | once per build | `build_id`, `timestamp`, CLI arguments echoed |
| `phase_start` | once per observable phase | `phase`, optional `scope` (files, functions) |
| `phase_complete` | once per observable phase that ran | `phase`, `errors`, `warnings`, `notes` |
| `phase_skipped` | zero or more | `phase`, `reason`, `blocking` (the ids of earlier diagnostics that forced the skip) |
| `diagnostic` | zero or more | The shape of an entry in the batch `diagnostics` array, plus a `phase` field naming the emitting phase |
| `build_complete` | once per build | `build_id`, `success`, `total_errors`, `total_warnings`, `program_effects` (null if the build did not reach effect analysis) |

Sub-phase events (monomorphization requests, SCC rounds, cache hits, individual LLVM passes) are deliberately **not** in the protocol: they flood the stream and are internal. If tools need them, they go to separate files like `--emit-timings`, never into the diagnostic stream.

**Observable phases.** One event pair (`phase_start` / `phase_complete`, or `phase_skipped`) per phase listed in [Observable phases](#observable-phases). Clients must accept phase names they do not recognize and treat them as opaque, so later versions can add phases without breaking old clients.

**Fail-fast: the predecessor-usable-output rule.** Each phase runs unless its predecessor produced no usable output. "Usable" depends on the phase:
- `lex` always runs.
- `parse` runs even after lex errors; the parser recovers over the error tokens.
- `resolve` runs if the parser produced a syntax tree for at least one top-level item.
- `typecheck` runs if resolve resolved any items. Unresolved names cascade as typecheck errors, which is expected.
- `effect` runs if typecheck produced any typed items.
- `ownership` runs if typecheck produced any typed items; it does not depend on effect output.
- `concurrency` runs if both `effect` and `ownership` completed.
- `codegen` runs only if **every** analysis phase completed with zero errors.

A skipped phase emits `phase_skipped` with a `reason` and the `blocking` diagnostic ids, so the client sees exactly which diagnostics cut the build short.

**The client decides when to stop.** There is no `--stop-on-first-error` flag. A client that wants fail-fast stops reading at the first `phase_complete` with `errors > 0`; a client that wants the full capped report reads to `build_complete`. If the client disconnects, pipe backpressure pauses the compiler; no cancellation protocol is needed.

**The cascade cap is the same in both modes.** A file that reaches the cap emits its `truncated` marker as a `diagnostic` event in the phase that reached it.

**The stream is a superset of the batch document.** From a complete stream, a client rebuilds the batch document by:
1. collecting every `diagnostic` event, in order, into `diagnostics`;
2. taking `program_effects` from `build_complete`;
3. optionally building a `phases` array from the `phase_complete` and `phase_skipped` events.

Four invariants:
1. Every batch diagnostic appears in the stream, at the emitting phase's boundary.
2. Every stream diagnostic appears in the batch document.
3. `build_complete.total_errors` equals the sum of `phase_complete.errors` over the phases that ran.
4. The cascade cap behaves identically in both modes.

Every batch diagnostic also carries the `phase` field, so one client library can consume either mode into the same representation.

### Compiler query API

Programmatic access to what the compiler inferred:

```bash
$ karac query effects src/order.kara.process_order        # inferred and declared effects
$ karac query ownership src/order.kara.process_order      # declared parameter modes, body usage classification
$ karac query monomorphization src/order.kara             # per-generic instance counts and type arguments
$ karac query queries                                     # the compiler queries report
```

Each query returns JSON. AI agents ask directly instead of compiling, parsing and guessing.

**Queryability is a commitment.** Every inferred decision (effects, the body-level ownership classification and the would-be tighter mode it implies, the monomorphization surface) must be reachable through `karac query`, not only through a diagnostic. Parameter modes are declared, not inferred; the ownership query reports the body-level classification behind would-be-mode diagnostics. Each answer includes a `derivation` chain explaining why, in the same shape as diagnostic derivations. A compiler pass that makes an inferred decision without a query is incomplete.

`karac query queries` returns the compiler queries report: the optimization decisions the compiler hedged on, and how to resolve them ([§2 Compiler queries](#compiler-queries)).

**Scope: batch, one-shot, narrow.** A client runs `karac query ...`; the compiler runs enough of the pipeline to answer and replies once. There is no subscription, no recomputation on file edits, and no broader surface ("type of the expression at this position", "visible trait impls here"). A reactive query-based compiler of the rust-analyzer class is a direction for after v1. The function-local principles of the [Compilation model](#compilation-model) are the substrate that would make it feasible.

**`karac query monomorphization` output.** One entry per generic function, with its instance count and the resolved type arguments of each instance:

```json
{
  "scope": "src/order.kara",
  "by_generic": [
    {
      "generic": "std.iter.map",
      "instance_count": 7,
      "instances": [
        {"types": ["Vec[Order]", "Receipt"], "site": "src/order.kara:42:9"},
        {"types": ["Vec[Order]", "Summary"], "site": "src/order.kara:51:9"}
      ]
    }
  ],
  "totals": {"generic_count": 12, "instance_count": 47}
}
```

### `karac fix` and `karac explain`

**`karac fix`** applies the machine-applicable diffs from the structured output. It is the primary fix path for an AI author: run `karac check --output=json`, apply `karac fix`, and feed the remaining diagnostics back.

`karac fix` also applies the style lints, each with a machine-applicable fix:
- a counter `while` loop becomes `for i in a..b`;
- `x = x + e` becomes `x += e`;
- a final `return e;` becomes `e`;
- a redundant `.iter()` or `.to_string()` on a literal is removed;
- a redundant `ref` on a parameter is removed: `x: ref T` means the same as `x: T` ([§7](#parameter-modes)).

**`karac explain`** explains a diagnostic code from the code catalogue. For an item, it shows the inferred effect set with its derivation, and the would-be parameter mode beside the declared one ([§2](#2-specification-layers)).

### Debugger contract

A debugger attaching to a Kāra binary needs more than DWARF: the runtime model of `par {}` blocks and `TaskGroup` tasks has shapes a generic stack walker cannot see. This subsection is the language-level contract between the compiler and runtime and any debugger, profiler or task inspector. Building those tools (gdb or lldb plugins, a DAP server, profiler interfaces, time-travel debugging) is out of scope for v1. The v1 deliverable is the contract and the runtime metadata, so that tools have a stable surface.

**Three layers:**
1. **Debug info (DWARF).** Required for any debugger to attach, but not Kāra-specific; LLVM does most of the work.
2. **The runtime stack model under `par {}` and tasks.** The Kāra-specific contract, below.
3. **Effect introspection at runtime.** Useful for advanced tools, not load-bearing; after v1.

**The contract: four elements.** A binary built with runtime debug metadata (on by default in `[profile.dev]`, off in `[profile.release]`, set by `runtime_debug_metadata = true|false` in either [build profile](#build-profiles)) emits four runtime structures, reachable through `std.runtime`:

1. **Static spawn-site ids per `par {}` block.** Each `par {}` block gets a compile-time `SpawnSiteId` (a `u32`). The id is per block, not per worker: workers within one block are anonymous and interchangeable. A tool that wants one worker combines the block id with the worker's thread-local index. Ids are stable across runs of the same binary and stored in its metadata table.
2. **A parent-frame reference per worker.** Every worker frame created by a `par {}` block or `TaskGroup.spawn()` points back to the frame that created it, which makes the structured-concurrency invariant (the parent outlives its children) observable from any child. The root task's parent reference is a sentinel tagged `"root"`. There is no free `spawn` ([§13](#13-concurrency)), so every task has this shape.
3. **An await-chain pointer per suspended task.** A task suspended at a waiting point carries a pointer to the task it waits on, or to the I/O handle it is blocked on, as a typed `WaitTarget` enum. The pointer is per task, because tasks have distinct awaiters and the wait tree cannot be rebuilt from per-block data.
4. **`std.runtime.list_tasks()` and `std.runtime.list_par_blocks()`.** `list_tasks()` returns every suspended task with its `WaitTarget`, source location and effect summary. `list_par_blocks()` returns every active `par {}` block with its `SpawnSiteId`, worker count and each worker's `file:line`. In a binary without the metadata both return an empty list, not an error, so generic tools can try and degrade.

Tasks suspend only once coroutines arrive with the services track (M4a); until then element 3 and `list_tasks()` have nothing to report.

**Gating.** The structures cost memory and some CPU on every spawn. `std.runtime.has_debug_metadata() -> bool` reports whether a binary carries them.

**Stability.** The four structures are stable within a major version. Fields may be added; an existing field changes shape only through an edition-gated migration.

### Panic record

A panic produces one **panic record** ([core-semantics.md §10.2](core-semantics.md#10-panics-and-errors-c8)). Its fields are part of the language contract, so tools can group panics across compiler versions.

**Fields:**
1. **Kind.** One of `explicit`, `index_out_of_bounds`, `overflow`, `div_by_zero`, `unwrap`, `borrow_conflict`, `assertion`, `alloc_failure`, `unreachable` and `deadlock`. The first nine cover the cases of [core-semantics.md §10.1](core-semantics.md#10-panics-and-errors-c8); `deadlock` is a `recv` on sequential `wasm_wasi` when no task can run ([Concurrency across targets](#concurrency-across-targets)). Kinds are only ever added. Tools deduplicate on the kind and the location.
2. **Message.** The panic's message, or `"<no message>"`.
3. **Location.** `file:line:col`. For a panic raised inside a standard-library function marked `#[track_caller]` ([§10](#10-errors-and-panics)), it is the caller's location.
4. **Error-return trace.** The path of the error being propagated, in debug builds ([Error return traces](#error-return-traces)).

**Where it goes.** A panic follows core-semantics.md §10.2's order:
1. The record goes to stderr in human-readable form. Its text may change freely.
2. The custom panic handler runs, if the program has one ([Panic handler](#panic-handler)).
3. The process exits with code 101.

Under `karac run` and `karac test`, the runner asks the program for the same record as one JSONL event on stderr, so a tool reads the panic without parsing text. There is no crash file.

**Stability.** The JSONL event is stable within a major version. Fields may be added and older tools ignore them; an existing field changes shape only through an edition-gated migration.

### Canonical formatting

`karac fmt` produces deterministic output. It is **syntactically** canonical, in the scope of `gofmt` or `rustfmt`, not a semantic normalizer:
- declarations are canonicalized (methods sorted, imports sorted);
- whitespace, brace placement and ordering are normalized;
- AI edits produce clean semantic diffs, not reordering noise.

`a + b` and `b + a` stay as written: the formatter normalizes structure and layout, not expressions. The syntax tree is designed to be normalizable.

### Lint levels

Lints are warnings the user can suppress, escalate or expect. Every lint has a stable name (`deprecated`, `undocumented_unsafe`, `redundant_suffix`, ...). Four attributes set a lint's level over a syntactic scope:

| Attribute | Effect on the named lint inside the scope |
|---|---|
| `#[allow(NAME)]` | Suppress it: never emitted in this scope. |
| `#[warn(NAME)]` | Emit at warning level (the default for most lints). |
| `#[deny(NAME)]` | Emit at error level: the build fails. |
| `#[expect(NAME)]` | Suppress it *and* track whether it fired. If it did **not** fire anywhere in the scope, emit `unfulfilled_lint_expectation` at the `#[expect]` attribute. |

**Scope.** The four attach to any item, block, statement or module-level position. A level set on an outer scope cascades inward; an inner attribute overrides it for the named lint only. One attribute may name several lints: `#[allow(deprecated, redundant_suffix)]`. Naming the same lint twice in one scope is `error[E_DUPLICATE_LINT_LEVEL]`.

**Build-wide policy.** `karac build` accepts `-A NAME`, `-W NAME`, `-D NAME` and `-F NAME` (forbid: like deny, and further `#[allow]` overrides are not allowed). `-D warnings` is the canonical "warnings are errors" CI flag. Lint policy in `kara.toml` comes after v1, with the [tooling track](deferred.md#tooling); attributes and these flags cover v1.

**`#[expect]`** is the cleanup signal:

```kara
fn migrate_caller() {
    #[expect(deprecated)]
    old_api();                    // deprecated today: suppressed, expectation fulfilled
}
```

If a later refactor replaces the call with a non-deprecated function, `deprecated` stops firing in the scope and the compiler emits:

```
warning[unfulfilled_lint_expectation]: `#[expect(deprecated)]` did not fire; remove the attribute
  --> src/migrator.kara:42:5
   |
42 |     #[expect(deprecated)]
   |     ^^^^^^^^^^^^^^^^^^^^^
   = help: replace with the appropriate behavior or remove the attribute entirely
```

`unfulfilled_lint_expectation` is itself a lint and may be suppressed with `#[allow(unfulfilled_lint_expectation)]`, mainly for code generators that emit `#[expect]` defensively. `#[expect(unfulfilled_lint_expectation)]` is circular and is `error[E_EXPECT_ON_UNFULFILLED]`. `#[expect]` is not a polite `#[allow]`: on a lint that may or may not fire (depending on configuration, say), it reports `unfulfilled_lint_expectation` whenever the lint is quiet.

**Names are stable.** Lint names are reported behavior ([§2](#2-specification-layers)). A new lint is non-breaking only if it is warn-by-default and its name is new. Renaming or removing a lint needs an edition boundary and a changelog entry. An attribute naming an unknown lint is the `unknown_lint` warning (suppressible with `#[allow(unknown_lint)]`), so `#[allow(...)]` and `#[expect(...)]` in older code never break a build.

**Built-in lints.** The registry (name, default level, trigger, suppression) is generated from the compiler and listed by `karac lint --list`. The v1 set includes `deprecated` ([§11](#deprecated)), `mutual_recursion_note`, `redundant_suffix`, `float_in_serialized_type`, `undocumented_unsafe`, `unfulfilled_lint_expectation`, `unknown_lint`, `unused_effect` (allowed by default, [§12](#inference-and-declarations)), `lossless_cast`, which flags a lossless `as` cast ([§5](#5-types)), and the style lints of [`karac fix`](#karac-fix-and-karac-explain). `unsafe_op_in_unsafe_fn` is a hard rule, not a lint, and takes no level ([§15](#unsafe_op_in_unsafe_fn-rule)).

### Attributes

The attribute list is generated from the compiler's attribute registry, the same registry the attribute checker uses, so the list and the compiler cannot drift. A bare-name attribute that is not in the registry is `error[E_UNKNOWN_ATTRIBUTE]`.

The v1 core set:

| Attribute | Purpose | Section |
|---|---|---|
| `#[derive(...)]` | Generate trait impls. | [§9](#9-traits) |
| `#[default]` | Mark the default variant for `#[derive(Default)]` (a derive helper). | [§9](#default-and-default) |
| `#[serde(...)]` | Rename, skip or default a field, and choose an enum's tagging, for `#[derive(Serialize, Deserialize)]` (a derive helper). | [§9](#serialize-and-deserialize) |
| `#[cfg(...)]` | Conditional compilation, including `target:`. | [§4](#4-modules-and-packages) |
| `#[repr(...)]` | ABI layout: `C`, an integer type, `transparent`. | [§15](#repr-abi-layout) |
| `#[must_use]` | Warn when a value is discarded. | [§11](#must_use) |
| `#[non_exhaustive]` | Let a public type gain variants or fields: other packages need a wildcard arm or `..`. | [§11](#non_exhaustive) |
| `#[deprecated]` | Warn at every use of an item scheduled for removal. | [§11](#deprecated) |
| `#[track_caller]` | Report a panic at the caller's location. | [§10](#10-errors-and-panics) |
| `#[panic_handler]` | Mark the custom panic handler. | [Panic handler](#panic-handler) |
| `#[allow]`, `#[warn]`, `#[deny]`, `#[expect]` | Lint levels. | [Lint levels](#lint-levels) |
| `#[unstable]` | Mark an API as unstable; using it requires `#[allow(unstable_api)]`. | this section |
| `#[no_effect(...)]` | Forbid an effect in a function and everything it calls. | [§12](#no_effect) |
| `#[kara_name("...")]` | Give a foreign function or type a Kāra name that follows the case rules. | [§3](#rules) |
| `#[compiler_builtin]` | A standard-library declaration implemented by the compiler. Standard library only. | [Intrinsics](#intrinsics) |

Tests are `test "name" { }` blocks, not an attribute ([§14](#14-testing)).

**Arguments are written like a call's.** An attribute is a bare name (`#[non_exhaustive]`), or a name with a parenthesized list of positional arguments and `key: value` arguments, the same shape as a call with named arguments ([§7](#named-and-default-parameters)): `#[repr(C)]`, `#[kara_name("CFile")]`, `#[serde(rename: "server_name")]`, `#[deprecated(since: "1.2.0", note: "...")]`. There is no `#[name = value]` form and no `key = value` argument. Inside `#[cfg(...)]` each condition is `key: value` (`target: "native"`, `target_os: "linux"`), and `not`, `any` and `all` take conditions as positional arguments, so `any(target_os: "linux", target_os: "macos")` may repeat a key.

**Attribute paths have one segment.** A multi-segment path such as `#[karac::proto]` is an error in v1; tool namespaces are deferred.

The `#[diagnostic::*]` namespace, tool-namespaced attributes (`#[tool::name]`), performance diagnostics, and the attributes of the deferred tracks (layout, systems, GPU, web, providers) are in [deferred.md](deferred.md).

---

## 18. Prior art

| Language or system | What Kāra takes |
|---|---|
| **Rust** | Ownership and moves, enums and pattern matching, traits and coherence, `Result[T, E]` with `?`. Kāra keeps Rust's lifetime elision as the whole story and drops named lifetimes. |
| **Swift** | Parameters that borrow by default, with consumption written (`own`, Swift's `consuming`); exclusivity enforcement that is static where it can be and dynamic for shared objects ([core-semantics.md §6.2](core-semantics.md)); `@escaping` for stored closures; value types beside reference types (`struct` beside `shared struct`). |
| **Koka** | An effect system, simplified: fixed verbs, no handlers. |
| **Go, Trio, Swift concurrency** | Structured concurrency and, from the services track, cooperative cancellation at I/O calls. |
| **Zig, Kotlin** | Lossless widening of mixed integer operands. |
| **Julia** | Named parameters after `;` in the parameter list. |
| **Python, Kotlin, JavaScript** | String interpolation (`f"..."`). |
| **Hylo, Mojo** | Value semantics without lifetime annotations, and borrowing parameters by default. |
| **Dafny, F\*** | Verification (deferred). |
| **Unity DOTS, Bevy** | Data-oriented layout (deferred). |
