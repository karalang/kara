# Core types: `Option`, `Result`, `Ordering` and numbers

The types themselves are in [design.md §5](../design.md#5-types): what `Option[T]` and `Result[T, E]` are, the numeric types and their arithmetic. The operators `?` and `??` are in [design.md §6](../design.md#6-expressions-and-statements) and [design.md §10](../design.md#10-errors-and-panics), which also covers `Error` and `AnyError`. This file lists the methods.

Methods that can panic (`unwrap`, `expect`, `unwrap_err`) report the caller's source location ([design.md §10](../design.md#10-errors-and-panics)).

## `Option[T]`

**Ordering.** Where `T` is ordered, `None` is less than every `Some(x)`, and two `Some`s compare by their values, so `sort()` puts `None` first. Where the types are comparable, `Ok(x)` is less than every `Err(e)`, and two `Ok`s or two `Err`s compare by their values. This is the order Rust, Haskell and OCaml give (review-thread ruling 2026-11-08; 0.8.1 and earlier put `None` last, B-2026-11-08-24).

| Method | Signature | Notes |
|---|---|---|
| `is_some` | `fn is_some(self) -> bool` | |
| `is_none` | `fn is_none(self) -> bool` | |
| `is_some_and` | `fn is_some_and(self, pred: own OnceFn(T) -> bool) -> bool` | `true` if `Some` and the value passes |
| `unwrap` | `fn unwrap(own self) -> T` | Panics on `None` |
| `expect` | `fn expect(own self, msg: Str) -> T` | Panics on `None` with `msg` |
| `unwrap_or` | `fn unwrap_or(own self, default: own T) -> T` | Like `opt ?? default`, except that `default` is evaluated before the call; `??` evaluates its right side only on `None` |
| `unwrap_or_else` | `fn unwrap_or_else(own self, f: own OnceFn() -> T) -> T` | `f` runs only on `None` |
| `unwrap_or_default` | `fn unwrap_or_default(own self) -> T where T: Default` | |
| `map` | `fn map[U](own self, f: own OnceFn(own T) -> U) -> Option[U]` | |
| `map_or` | `fn map_or[U](own self, default: own U, f: own OnceFn(own T) -> U) -> U` | |
| `map_or_else` | `fn map_or_else[U](own self, default: own OnceFn() -> U, f: own OnceFn(own T) -> U) -> U` | `default` runs only on `None` |
| `and_then` | `fn and_then[U](own self, f: own OnceFn(own T) -> Option[U]) -> Option[U]` | |
| `and` | `fn and[U](own self, other: own Option[U]) -> Option[U]` | `None` if `self` is; otherwise `other`. `self`'s value drops |
| `or` | `fn or(own self, other: own Option[T]) -> Option[T]` | |
| `or_else` | `fn or_else(own self, f: own OnceFn() -> Option[T]) -> Option[T]` | |
| `filter` | `fn filter(own self, pred: own OnceFn(T) -> bool) -> Option[T]` | `None` unless the value passes |
| `ok_or` | `fn ok_or[E](own self, err: own E) -> Result[T, E]` | |
| `ok_or_else` | `fn ok_or_else[E](own self, f: own OnceFn() -> E) -> Result[T, E]` | `f` runs only on `None` |
| `zip` | `fn zip[U](own self, other: own Option[U]) -> Option[(T, U)]` | `Some` only if both are |
| `take` | `fn take(mut ref self) -> Option[T]` | Moves the value out and leaves `None` |
| `replace` | `fn replace(mut ref self, value: own T) -> Option[T]` | Stores `value` and returns the old contents |
| `get_or_insert` | `fn get_or_insert(mut ref self, value: own T) -> mut ref T` | Fills a `None` with `value`, then returns a reference to the value; `value` drops if `self` was `Some` |
| `get_or_insert_with` | `fn get_or_insert_with(mut ref self, f: own OnceFn() -> T) -> mut ref T` | Fills a `None` with `f()`, then returns a reference to the value |
| `as_ref` | `fn as_ref(self) -> Option[ref T]` | A view of the value, without moving it |
| `as_deref` | `fn as_deref(self) -> Option[Str]` on `Option[String]`, and `-> Option[Slice[T]]` on `Option[Vec[T]]` | A view of an owned string or vector, without moving or allocating: `req.header("X-Token").as_deref() == Some(TOKEN)` with `TOKEN: Str`. An `Option[String]` never compares with an `Option[Str]` directly, because comparison looks through each operand but not inside type arguments ([design.md](../design.md)). `karac fix` inserts `.as_deref()` on the owned side (review-thread ruling 2026-11-10, B-2026-11-09-97) |
| `cloned` | `fn cloned(own self) -> Option[T]` on `Option[ref T]`, `where T: Clone` | An owned copy of a viewed value: `m.get(k).cloned() ?? d` gives a default for a non-`Copy` value |
| `flatten` | `fn flatten(own self) -> Option[T]` on `Option[Option[T]]` | |

`is_ok` and `is_err` are `Result` methods. On an `Option` they are deprecated spellings of `is_some` and `is_none`: `karac check` warns and `karac fix` rewrites them. They become an error at the next minor release, as `vec!` and `mut` on a plain field do (review-thread ruling 2026-11-09, B-2026-11-09-21).

## `Result[T, E]`

| Method | Signature | Notes |
|---|---|---|
| `is_ok` | `fn is_ok(self) -> bool` | |
| `is_err` | `fn is_err(self) -> bool` | |
| `ok` | `fn ok(own self) -> Option[T]` | Drops an error |
| `err` | `fn err(own self) -> Option[E]` | Drops a value |
| `map` | `fn map[U](own self, f: own OnceFn(own T) -> U) -> Result[U, E]` | |
| `map_err` | `fn map_err[F](own self, f: own OnceFn(own E) -> F) -> Result[T, F]` | |
| `map_or` | `fn map_or[U](own self, default: own U, f: own OnceFn(own T) -> U) -> U` | |
| `map_or_else` | `fn map_or_else[U](own self, default: own OnceFn(own E) -> U, f: own OnceFn(own T) -> U) -> U` | `default` runs only on `Err`, given the error |
| `and_then` | `fn and_then[U](own self, f: own OnceFn(own T) -> Result[U, E]) -> Result[U, E]` | |
| `or_else` | `fn or_else[F](own self, f: own OnceFn(own E) -> Result[T, F]) -> Result[T, F]` | |
| `unwrap` | `fn unwrap(own self) -> T` | Panics on `Err` |
| `expect` | `fn expect(own self, msg: Str) -> T` | Panics on `Err` with `msg` |
| `unwrap_err` | `fn unwrap_err(own self) -> E` | Panics on `Ok` |
| `expect_err` | `fn expect_err(own self, msg: Str) -> E` | Panics on `Ok` with `msg` |
| `unwrap_or` | `fn unwrap_or(own self, default: own T) -> T` | |
| `unwrap_or_else` | `fn unwrap_or_else(own self, f: own OnceFn(own E) -> T) -> T` | |
| `unwrap_or_default` | `fn unwrap_or_default(own self) -> T where T: Default` | The error is dropped |

`.context(msg)`, which turns an error into an `AnyError` with a context message, is in [design.md §10](../design.md#error-and-anyerror).

## `Ordering`

`Ord.cmp` returns an `Ordering`: `Less`, `Equal` or `Greater` ([design.md §5](../design.md#5-types)). Besides the predicates `is_lt`, `is_le`, `is_gt`, `is_ge` and `is_eq`, it has three methods for building comparisons:

| Method | Signature | Notes |
|---|---|---|
| `reverse` | `fn reverse(self) -> Ordering` | Swaps `Less` and `Greater`, so a comparator ending in `.reverse()` sorts descending |
| `then` | `fn then(self, other: Ordering) -> Ordering` | `self` unless it is `Equal`, otherwise `other` |
| `then_with` | `fn then_with(self, f: Fn() -> Ordering) -> Ordering` | Like `then`, but calls `f` only when `self` is `Equal` |

To sort by two keys, compare by the first and chain the second with `then`. This sorts by score, highest first, and breaks ties by name:

```kara
struct Player { name: String, score: i64 }

let mut ps: Vec[Player] = [
    Player { name: "cleo", score: 7 },
    Player { name: "ana", score: 9 },
    Player { name: "bo", score: 7 },
];
ps.sort_by(|a, b| a.score.cmp(b.score).reverse().then(a.name.cmp(b.name)));
// ana 9, bo 7, cleo 7
```

Use `then_with` when the second key is costly to compute: `a.len().cmp(b.len()).then_with(|| expensive(a).cmp(expensive(b)))`.

### `Reverse`

`std.cmp.Reverse[T]` reverses the order of one key inside a larger one, where a comparator cannot reach: a heap's item, or a `sort_by_key` / `max_by_key` key.

```kara
import std.cmp.Reverse;

struct Reverse[T] { pub value: T }
```

`Reverse[T]` implements `PartialEq`, `Eq`, `PartialOrd` and `Ord` when `T` does, by `T`'s order reversed, and `Hash`, `Clone`, `Copy`, `Debug` and `Display` when `T` does. It is built and matched as a struct: `pq.push((Reverse { value: score }, name))`, `let (Reverse { value: s }, n) = pq.pop().unwrap();`, `v.sort_by_key(|p| (Reverse { value: p.score }, p.name.clone()))`. There is no other constructor. Kāra has no tuple structs, so Rust's `Reverse(x)` is written `Reverse { value: x }` and its `r.0` is `r.value`. `karac fix` makes those three rewrites, and `Reverse<T>` to `Reverse[T]`, keeping Rust's `use std::cmp::Reverse` as the import (review-thread ruling 2026-11-11, B-2026-11-10-41).

## Numbers

**Overflow method families.** Every integer type has four families of named arithmetic methods, for each of `add`, `sub`, `mul`, `div`, `rem`, `neg`, `shl`, `shr` and `pow`, with the same meaning as Rust's methods of those names. `saturating_` has only `add`, `sub`, `mul`, `div`, `pow` and, on signed types, `neg`, since a remainder or a shift has no bound to clamp to. Every other operand is of type `Self`, except a shift amount or a `pow` exponent, which is a `u32`. A zero divisor panics in every family but `checked_`, which gives `None`:

| Family | Returns | On overflow or out-of-range |
|---|---|---|
| `checked_add(y)`, `checked_div(y)`, `checked_pow(e)`, ... | `Option[Self]` | `None` |
| `wrapping_add(y)`, `wrapping_neg()`, ... | `Self` | Two's-complement wrap; a shift amount is taken modulo the bit width, so `x.wrapping_shl(n)` equals `x.wrapping_shl(n % bits)` |
| `saturating_add(y)`, ... | `Self` | Clamps to the type's minimum or maximum |
| `overflowing_add(y)`, ... | `(Self, bool)` | The wrapped result and whether it overflowed; a shift overflows when its amount is at least the bit width |

**Bit counts.** `count_ones()`, `count_zeros()`, `leading_zeros()` and `trailing_zeros()` exist on every integer type and return `i64`, as every count does in Kāra ([design.md §5](../design.md#5-types)), not Rust's `u32`. So `x.count_ones() == k` and `total += x.count_ones()` need no cast with `k` and `total` of type `i64`. A shift amount or a `pow` exponent stays `u32` (above). (Review-thread ruling 2026-10-30, B-2026-10-29-19.)

**Byte order.** As Rust's: `x.to_ne_bytes()`, `x.to_le_bytes()` and `x.to_be_bytes()` give an integer's bytes as an `Array[u8, N]`, `N` its byte width, in native, little-endian or big-endian order, and `<int>.from_ne_bytes(b)`, `from_le_bytes(b)` and `from_be_bytes(b)` read one back from an `Array[u8, N]` of that width. A wire format names its order: a SOCKS5 or IP port is `port.to_be_bytes()` and `u16.from_be_bytes([hi, lo])`. (B-2026-11-10-45.)

What the plain operators do on overflow, and the other numeric rules, are in [design.md §5](../design.md#numeric-semantics).

**Char digits.** As Rust's: `c.to_digit(radix) -> Option[u32]` is the digit `c` stands for below the radix, `c.is_digit(radix) -> bool` whether it stands for one, and `char.from_digit(num: u32, radix: u32) -> Option[char]` the inverse, `0`..`9` then `a`..`z` for a `num` below the radix and `None` from the radix up. A radix outside 2..=36 panics in all three.

**Float methods.** `f32` and `f64` have these methods, each returning `Self` unless noted. Kāra's power method is `pow`, not Rust's `powf`.

| Group | Methods |
|---|---|
| Rounding | `floor()`, `ceil()`, `round()`, `trunc()`, `fract()` |
| Sign and size | `abs()`, `signum()`, `copysign(y)`, `min(y)`, `max(y)`, `clamp(lo, hi)`, `recip()` |
| Powers and roots | `pow(y)`, `powi(n: i32)`, `sqrt()`, `cbrt()`, `hypot(y)` |
| Exponentials and logarithms | `exp()`, `exp2()`, `exp_m1()`, `ln()`, `ln_1p()`, `log2()`, `log10()` |
| Trigonometry | `sin()`, `cos()`, `tan()`, `asin()`, `acos()`, `atan()`, `atan2(x)`, `sinh()`, `cosh()`, `tanh()`, `asinh()`, `acosh()`, `atanh()`, `to_degrees()`, `to_radians()` |
| Bits and text | `to_bits()` (the `u64` pattern of the value as an `f64`, at both widths), `to_bits32()` (the `u32` pattern of the value rounded to `f32`), `to_string()`. The inverses are the integer methods `bits_as_f64()` and `bits_as_f32()` |

**Float to integer.** There are two forms ([design.md §5](../design.md#float-to-integer-conversion)):
- `f as iN` saturates: an out-of-range value gives the nearest end of the range, and NaN gives 0.
- `iN.checked_from(f: f64) -> Option[iN]` returns `None` for NaN or an out-of-range value, and otherwise `Some` of the value truncated toward zero. It exists on every integer type; an `f32` argument widens to `f64` losslessly.

**`total_cmp`.** `f32` and `f64` have `fn total_cmp(self, other: Self) -> Ordering`, called as `x.total_cmp(y)` or passed as `f64.total_cmp`. It is IEEE 754 `totalOrder` with NaN canonicalized as `F64` does it ([design.md §5](../design.md#5-types)): every NaN equals every other NaN and sorts after every other value, whatever its sign and payload, so `(-NaN).total_cmp(-inf)` is `Greater`. Raw `totalOrder` would let a NaN's sign decide the order, and that sign differs between a runtime and a constant-folded `z / z`. It orders every value, NaN included, so it can sort floats directly:

```kara
let mut w: Vec[f64] = [2.5, -1.0, 0.25];
w.sort_by(f64.total_cmp);
```

For floats as `Map` keys or in `Ord` contexts, use the total-order types `F32` and `F64` ([design.md §5](../design.md#5-types)).

### Parsing

Each integer and float type has `fn parse(s: Str) -> Result[Self, ParseError]`. It is called on the target type, since calls take no type arguments:

```kara
match i64.parse(line.trim()) {
    Ok(n) => println(f"You guessed {n}"),
    Err(e) => println(f"not a number: {e}"),
}
```

`.ok()` turns the result into an `Option` when the error is not needed.

`parse` ignores whitespace before and after the number, so `i64.parse(" 5\n")` is `Ok(5)` and a line read with its newline parses as it is. A sign may come only first: `"+5"` parses, `"-"` and `"1-2"` fail with `InvalidDigit`, and a negative number for an unsigned type fails with `Underflow`.

Every `parse` returns the one error type, a prelude enum:

```kara
enum ParseError {
    Empty,          // the input is empty
    InvalidDigit,   // the input holds a character that cannot appear in a number of this type
    Overflow,       // the value is above the type's maximum
    Underflow,      // the value is below the type's minimum
    Invalid,        // any other malformed input (a float's answer; an integer reports InvalidDigit)
}
```

`ParseError` implements `Error` and `Display`. So `?` converts it into `AnyError` ([design.md §10](../design.md#error-and-anyerror)), and into the caller's own error type through a `From` impl.

### Printing

`Display` for `f32` and `f64` prints the shortest digits that read back as the same value, so float output round-trips:
- **Always a `.` or an exponent**, so a float never reads as an integer: `println(1.0)` prints `1.0`, `println(0.1)` prints `0.1`, and `println([1.0, 2.5])` prints `[1.0, 2.5]`.
- **Exponent form when the decimal exponent is below −4 or at least 16**, as Python's `repr` does: `1e16` prints `1e+16`, and `0.00001` prints `1e-05`. Otherwise plain decimal: `0.0001` prints `0.0001`.
- **A tie goes to the even last digit**, as Python's `repr` does. When two equally short decimals both read back and are equally near the value, the one whose last digit is even is printed: `-75455689800034.625` prints `-75455689800034.62`, not `.63`. Ties occur only where the value has few fraction bits (B-2026-10-23-4, decided 2026-10-26).
- **The special values** print `NaN`, `inf` and `-inf`.

`to_string()` and f-string interpolation use the same rendering.
