# Appendix A: Keywords

Kāra keeps its hard keywords few, so that common words stay usable as names.
This appendix follows design.md § Keywords, which is the authority.

## Hard keywords

A hard keyword starts an item, a statement or an expression, or is an operator
spelled as a word. It can't be used as an identifier, except as a raw
identifier (`r#match`) or as a member name (below).

| Keyword | Purpose |
|---------|---------|
| `fn` | Declare a function |
| `struct` | Declare a struct |
| `enum` | Declare an enum |
| `trait` | Declare a trait |
| `impl` | Implement a trait or add methods to a type |
| `type` | Declare a type alias |
| `const` | Declare a compile-time constant |
| `import` | Import a module or a name from one |
| `extern` | Declare foreign functions or types |
| `unsafe` | Mark a block or function as bypassing safety checks |
| `pub` | Public: visible outside the package |
| `private` | Private: visible only within the current directory |
| `let` | Declare a local binding |
| `mut` | Mark a binding as mutable, or a parameter or argument as written through |
| `if`, `else` | Conditional branch |
| `match` | Pattern-matching switch |
| `while` | Condition-driven loop |
| `for`, `in` | Iterator-driven loop |
| `loop` | Infinite loop |
| `return` | Early return from a function |
| `break` | Exit from a loop |
| `continue` | Skip to the next loop iteration |
| `defer` | Run a block when the enclosing scope exits, on every exit (normal, `return`, `break` or `?`) |
| `errdefer` | Run a block when the enclosing scope exits with an error (`?` or returning `Err` / `None`) |
| `ref` | Borrow a value (read-only reference) |
| `as` | Type cast |
| `and`, `or`, `not` | Logical operators |
| `true`, `false` | Boolean literals |
| `self` | The receiver value within a method |
| `Self` | The type of the current `impl` block or trait |
| `dyn` | Dynamic dispatch through a trait object: `dyn Trait` |

*(Default visibility, with no keyword, is project-internal: visible to every
file in the project.)*

`use` and `mod` are not keywords. Modules come from the directory tree, and
imports use `import`.

## Contextual keywords

A contextual keyword is a keyword only in one position, and an ordinary
identifier everywhere else. A field called `reads` or a variable called `test`
is fine.

| Word | A keyword only when it |
|------|------------------------|
| `par` | comes directly before `{`, `for` or `(` |
| `union` | starts an item |
| `distinct` | comes directly before `type` |
| `marker` | comes directly before `trait` |
| `shared`, `sync` | come directly before `struct` or `enum` |
| `weak`, `frozen` | prefix a type or a receiver |
| `freeze` | prefixes an expression |
| `escaping` | prefixes a function type or a `dyn` type |
| `own` | comes directly before a parameter's type, before `self`, or before an argument type in a function type (`Fn(own T) -> R`); it marks an owned parameter |
| `host` | comes directly before `fn` at item position |
| `test` | comes directly before a string literal at module scope |
| `where` | follows a signature or a type head |
| `with` | starts an effect clause |
| `effect`, `resource` | start an effect resource declaration |
| `reads`, `writes`, `sends`, `receives`, `allocates`, `panics`, `blocks` | appear inside an effect clause or an effect attribute such as `#[no_effect(...)]` |

So these are legal:

```kara,ignore
struct IoStats { reads: i64, writes: i64, blocks: i64 }
fn union(mut ref self, a: i64, b: i64) { ... }
fn create_server(host: String, port: u16) { ... }
```

Words that belong to deferred features (`lock`, `seq`, `layout`, `group`,
`stable`, `alias`, `independent`, `providers`, `requires`, `ensures`,
`invariant`, `suspends`) are not keywords in v1. They come back as contextual
keywords with their features.

## Keywords as member names

A member name may be any keyword without `r#` in two positions where no keyword
can start anything: directly after `.` (`opt.or(fallback)`, `handle.await`),
and as the name of a method or associated function declared in an `impl` or
`trait`. A free function, a local, a field or a type with a keyword's name
needs `r#`.

## Reserved for future use

These words are reserved now, so a later feature can use them without breaking
code. Using one as an identifier is `error[E_RESERVED_KEYWORD]`, and the fix is
`r#name`.

| Keyword | Why reserved |
|---------|--------------|
| `async`, `await` | Kāra marks suspension with the `suspends` effect rather than function colouring |
| `become` | Guaranteed tail calls |
| `box` | An owned heap-pointer primitive |
| `comptime` | Compile-time evaluation |
| `do` | Block-expression sugar |
| `final`, `override`, `virtual` | Subclass-style markers, common in code ported from OOP languages |
| `gen`, `yield` | Generators |
| `asm`, `global_asm` | Inline and module-level assembly |
| `move` | Rust's capture-by-value keyword; Kāra infers capture modes |
| `priv` | Too easily confused with `private` |
| `pure` | Its one use in v1: before `fn` in an `extern` block, it declares that the foreign function has no effects |
| `try` | `try { ... }` blocks |
| `typeof` | Type-of queries |

## Primitive type names

`i8`, `i16`, `i32`, `i64`, `i128`, `u8`, `u16`, `u32`, `u64`, `u128`, `f32`,
`f64`, `bool`, `char` and `!` (the never type) are always in scope and need no
import.
