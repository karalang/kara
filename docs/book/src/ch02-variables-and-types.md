# Variables and Types

## Bindings

Variables in Kāra are declared with `let`:

```kara
let x = 42;
let name = "Kāra";
let pi = 3.14159;
let active = true;
```

The compiler infers the type from the value. You can also annotate explicitly:

```kara
let x: i32 = 42;
let name: String = "Kāra";
```

## Mutability

Bindings are immutable by default. To allow reassignment, use `let mut`:

```kara
let x = 5;
// x = 10;  // compile error: x is immutable

let mut y = 5;
y = 10;     // ok
```

This is a deliberate default. Most values don't need to change, and immutability helps the compiler reason about your code — for ownership, for parallelization, and for correctness.

## Primitive types

Kāra has the numeric types you'd expect:

| Type | Description |
|------|-------------|
| `i8`, `i16`, `i32`, `i64` | Signed integers |
| `u8`, `u16`, `u32`, `u64` | Unsigned integers |
| `f32`, `f64` | Floating-point numbers |
| `bool` | `true` or `false` |
| `char` | A Unicode scalar value: `'a'`, `'\n'`, `'\u{1F600}'` |

Integer literals default to `i64`, floats to `f64`. If you annotate a different type, the compiler checks that the literal fits:

```kara
let small: u8 = 255;    // ok
// let overflow: u8 = 256;  // compile error: 256 doesn't fit in u8
```

### Numeric literals

Numbers can use underscores for readability and different bases:

```kara
let million = 1_000_000;
let flags = 0b1010_0011;   // binary
let color = 0xFF_AA_00;    // hex
let permissions = 0o755;   // octal
```

## Strings

Kāra has two string types:

- `String` — an owned, heap-allocated UTF-8 string.
- `StringSlice` — a borrowed view into a string (like Rust's `&str`).

```kara
let greeting = "Hello";              // String
let multiline = """
    This is a
    multi-line string.
""";
```

### String interpolation

Prefix a string with `f` to embed expressions:

```kara
let name = "world";
let msg = f"Hello, {name}!";

let x = 10;
let y = 20;
println(f"{x} + {y} = {x + y}");  // "10 + 20 = 30"
```

## Type conversions

Kāra does not do implicit type conversions (except narrowing integer literals to annotated types). Use `as` for numeric casts:

```kara
let x: i64 = 1000;
let y: i32 = x as i32;
```

## Shadowing

You can re-declare a binding with the same name. The new binding shadows the old one:

```kara
let x = 5;
let x = x + 1;       // x is now 6
let x = x * 2;       // x is now 12
```

Shadowing lets you transform a value through a series of steps without `mut`. Each `let` creates a new binding — the old one is gone.

## Naming identifiers

Kāra enforces identifier naming at the compiler level — it's a grammar rule, not a style guide. Every identifier belongs to one of three **case classes**:

- **Type class** — PascalCase. Structs, enums, enum variants, traits, generic type parameters: `String`, `UserAccount`, `IoError`, `T`.
- **Value class** — snake_case, or a leading `_` for intentionally-unused bindings. Functions, parameters, fields, modules, and `let` bindings inside function bodies: `read_to_string`, `user_count`, `_tmp`.
- **Const class** — ALL_UPPER with underscores. Module-level `let` and `let mut` bindings: `MAX_RETRIES`, `TIMEOUT_MS`.

```kara
struct UserAccount { ... }                // Type class
fn read_to_string(path: String) { ... }   // Value class
let count = 0;                            // Value class (function body)
```

`fn ReadFile()`, `struct user_account`, and `let pi = 3.14` at module scope are all compile errors. One quirk worth knowing: multi-word types treat acronyms as words — `HttpClient`, not `HTTPClient`; `IoError`, not `IOError`. That keeps the classification unambiguous at a glance.

Note: `_` on its own isn't a name — it's a wildcard used in patterns, `let _ = expr`, pipes, and `with _` effects. Only *leading* `_` (like `_tmp`) is a valid identifier you can read later.

The point of enforcing this is that every Kāra codebase reads the same way — no per-project casing debates, no PR bikeshedding, no re-tuning when you move between libraries.

## Module-level bindings

You can declare bindings at the top of a file, outside any function:

```kara
let MAX_RETRIES: i32 = 5;
let TIMEOUT_MS: i64 = 60 * 1000;
let APP_NAME: String = "myapp";
```

Module-level bindings must be initialized with compile-time constant expressions — no function calls, no I/O, no allocations. This is a deliberate restriction: there's no hidden code running before `main`, no initialization order bugs, no startup effects you can't see.

Values that need runtime initialization (config files, database connections) are constructed inside `main` and passed down. We'll cover this pattern in later chapters.
