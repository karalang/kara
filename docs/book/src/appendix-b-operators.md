# Appendix B: Operators and Symbols

## Arithmetic operators

| Operator | Meaning | Trait |
|----------|---------|-------|
| `a + b` | Add | `Add` |
| `a - b` | Subtract | `Sub` |
| `a * b` | Multiply | `Mul` |
| `a / b` | Divide | `Div` |
| `a % b` | Remainder | `Rem` |

All arithmetic operators lower to trait-method calls after type checking. Writing `a + b` where `A` does not implement `Add` is a type error that names the missing trait, not the operator.

Integer arithmetic uses trap-on-overflow semantics in `app` and `lib` profiles. Use named methods for explicit control:

| Method | Behavior |
|--------|----------|
| `a.checked_add(b)` → `Option[T]` | Returns `None` on overflow |
| `a.saturating_add(b)` | Clamps to `T::MAX` / `T::MIN` |
| `a.wrapping_add(b)` | Two's-complement wraparound |
| `a.overflowing_add(b)` → `(T, bool)` | Wraparound + overflow flag |

## Bitwise operators

| Operator | Meaning | Trait |
|----------|---------|-------|
| `a & b` | Bitwise AND | `BitAnd` |
| `a \| b` | Bitwise OR | `BitOr` |
| `a ^ b` | Bitwise XOR | `BitXor` |
| `a << b` | Left shift | `Shl` |
| `a >> b` | Right shift | `Shr` |

## Comparison operators

| Operator | Meaning | Trait |
|----------|---------|-------|
| `a == b` | Equal | `PartialEq` |
| `a != b` | Not equal | `PartialEq` |
| `a < b` | Less than | `PartialOrd` |
| `a <= b` | Less than or equal | `PartialOrd` |
| `a > b` | Greater than | `PartialOrd` |
| `a >= b` | Greater than or equal | `PartialOrd` |

## Logical operators

| Operator | Meaning |
|----------|---------|
| `a && b` | Short-circuit logical AND |
| `a \|\| b` | Short-circuit logical OR |
| `!a` | Logical NOT |

## Assignment operators

| Operator | Meaning |
|----------|---------|
| `a = b` | Assign |
| `a += b` | Add-assign |
| `a -= b` | Subtract-assign |
| `a *= b` | Multiply-assign |
| `a /= b` | Divide-assign |
| `a %= b` | Remainder-assign |
| `a &= b` | Bitwise-AND-assign |
| `a \|= b` | Bitwise-OR-assign |
| `a ^= b` | Bitwise-XOR-assign |
| `a <<= b` | Left-shift-assign |
| `a >>= b` | Right-shift-assign |

## Range operators

| Operator | Meaning | Example |
|----------|---------|---------|
| `a..b` | Half-open range `[a, b)` | `0..10` |
| `a..=b` | Closed range `[a, b]` | `1..=5` |
| `a..` | Range from `a` to end | `slice[2..]` |
| `..b` | Range from start to `b` (exclusive) | `slice[..4]` |
| `..` | Full range | `slice[..]` |

## Other operators and symbols

| Symbol | Meaning |
|--------|---------|
| `?` | Propagate an `Err` result early (error shorthand) |
| `a \|> f` | Pipe `a` as the first argument to `f` |
| `a ?? b` | Nil-coalesce: return `a` if it is `Some`, else `b` |
| `a?.b` | Optional chaining: access `.b` only if `a` is `Some` |
| `a as T` | Cast `a` to type `T` |
| `*` | Prefix dereference (planned — `*r` where `r: ref T` yields `T`) |
| `_` | Wildcard pattern or unnamed placeholder |
| `..` | Struct-update spread in struct literals |
| `->` | Return-type annotation in function signatures |
| `=>` | Pattern arm separator in `match` |
| `::` | Path separator in qualified names |
| `@` | Attribute prefix |
| `#[...]` | Attribute on a declaration |

## Numeric literal suffixes

Force the type of a literal where inference cannot propagate from a binding annotation.

| Suffix | Type |
|--------|------|
| `42i8` | `i8` |
| `42i16` | `i16` |
| `42i32` | `i32` |
| `42i64` | `i64` (default for integer literals) |
| `42u8` | `u8` |
| `42u16` | `u16` |
| `42u32` | `u32` |
| `42u64` | `u64` |
| `1.0f32` | `f32` |
| `1.0f64` | `f64` (default for float literals) |

Unsuffixed integer literals default to `i64`; unsuffixed float literals default to `f64`. In a binary expression, an unsuffixed literal may be promoted to the type of its suffixed sibling.

## String literal prefixes

| Prefix | Meaning |
|--------|---------|
| `"..."` | Plain string literal |
| `f"..."` | Interpolated string — `{expr}` inserts the `Display` value of `expr` |
| `f"...{expr:?}..."` | Interpolated with `Debug` formatting |
| `b"..."` | Byte string (future) |
| `r"..."` | Raw string — no escape processing (reserved, not yet implemented) |
