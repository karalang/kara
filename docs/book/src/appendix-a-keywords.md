# Appendix A: Keywords

The following words are reserved by the Kāra language. You cannot use them as identifiers.

## Declaration keywords

| Keyword | Purpose |
|---------|---------|
| `fn` | Declare a function |
| `struct` | Declare a struct |
| `enum` | Declare an enum |
| `trait` | Declare a trait |
| `impl` | Implement a trait or add methods to a type |
| `type` | Declare a type alias |
| `distinct` | Declare a distinct (newtype) alias |
| `const` | Declare a compile-time constant |
| `mod` | Declare a module |
| `use` | Bring a name into scope |
| `import` | Import an external package |
| `extern` | Declare a foreign function or type |
| `shared` | Mark a struct or enum as reference-semantics (RC) |
| `layout` | Declare a physical memory layout for a struct |
| `group` | Group fields within a layout block |
| `effect` | Declare an effect system definition |
| `resource` | Declare an effect resource |
| `verb` | Declare an effect verb |
| `alias` | Declare that two resource names refer to the same underlying resource |

## Visibility keywords

| Keyword | Purpose |
|---------|---------|
| `pub` | Public — visible to external consumers |
| `private` | Private — visible only within the current directory |

*(Default visibility — no keyword — is project-internal: visible to all files in the project.)*

## Control flow keywords

| Keyword | Purpose |
|---------|---------|
| `if` | Conditional branch |
| `else` | Fallthrough branch for `if` |
| `match` | Pattern-matching switch |
| `while` | Condition-driven loop |
| `for` | Iterator-driven loop |
| `in` | Separator between pattern and iterable in `for` |
| `loop` | Infinite loop |
| `return` | Early return from a function |
| `break` | Exit from a loop |
| `continue` | Skip to the next loop iteration |
| `defer` | Run a block when the enclosing scope exits (success path) |
| `errdefer` | Run a block when the enclosing scope exits via `?`-propagated error |
| `asm` | Inline assembly block |
| `global_asm` | Module-level assembly block |

## Binding keywords

| Keyword | Purpose |
|---------|---------|
| `let` | Declare a local binding |
| `mut` | Mark a binding or parameter as mutable |

## Ownership and borrowing keywords

| Keyword | Purpose |
|---------|---------|
| `own` | Explicit owned parameter mode (rarely needed — owned is the default) |
| `ref` | Borrow a value (read-only reference) |
| `weak` | Weak reference into an RC type |
| `lock` | Lock resource |

## Effect keywords

| Keyword | Purpose |
|---------|---------|
| `reads` | Effect: reads from a resource |
| `writes` | Effect: writes to a resource |
| `sends` | Effect: sends to a resource |
| `receives` | Effect: receives from a resource |
| `allocates` | Effect: allocates from a resource |
| `panics` | Effect: may panic |
| `blocks` | Effect: may block the calling thread |
| `suspends` | Effect: may yield to the scheduler |
| `with` | Introduce an effect annotation or effect variable |
| `transparent` | Mark an effect as transparent (not attributed to callers) |
| `stable` | Mark an effect annotation as part of the public API contract |
| `seq` | Sequential block |
| `par` | Parallel block (branches may execute concurrently) |
| `yield` | Yield a value from a generator |

## Type system keywords

| Keyword | Purpose |
|---------|---------|
| `as` | Type cast or trait disambiguation |
| `where` | Introduce generic bounds or refinement-type predicates |
| `dyn` | Dynamic dispatch through a trait object |
| `Self` | The type of the current `impl` block or trait |
| `self` | The receiver value within a method |

## Contract keywords

| Keyword | Purpose |
|---------|---------|
| `requires` | Precondition contract on a function |
| `ensures` | Postcondition contract on a function |
| `invariant` | Invariant check at the end of every method in an `impl` block |

## Safety keywords

| Keyword | Purpose |
|---------|---------|
| `unsafe` | Mark a block or function as bypassing safety checks |

## Concurrency and context keywords

| Keyword | Purpose |
|---------|---------|
| `providers` | Introduce a provider scope |
| `independent` | Declare that two resources are independent for conflict analysis |

## Literal keywords

| Keyword | Purpose |
|---------|---------|
| `true` | Boolean true |
| `false` | Boolean false |

## Reserved for future use

These words are reserved now; using them as identifiers is a compile error.

| Keyword | Planned use |
|---------|-------------|
| `f16` | Half-precision float (Phase 7+) |
| `bf16` | Brain-float (Phase 7+) |

## Primitive type names

These are lexer-level keywords, not identifiers. They are always in scope and require no import.

`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `f32`, `f64`, `bool`, `char`, `!` (the never type)
