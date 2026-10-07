# Core types: `Option`, `Result` and numbers

The types themselves are in [design.md §5](../design.md#5-types): what `Option[T]` and `Result[T, E]` are, the numeric types and their arithmetic. The operators `?` and `??` are in [design.md §6](../design.md#6-expressions-and-statements) and [design.md §10](../design.md#10-errors-and-panics), which also covers `Error` and `AnyError`. This file lists the methods.

Methods that can panic (`unwrap`, `expect`, `unwrap_err`) report the caller's source location ([design.md §10](../design.md#10-errors-and-panics)).

## `Option[T]`

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
| `and_then` | `fn and_then[U](own self, f: own OnceFn(own T) -> Option[U]) -> Option[U]` | |
| `or` | `fn or(own self, other: own Option[T]) -> Option[T]` | |
| `or_else` | `fn or_else(own self, f: own OnceFn() -> Option[T]) -> Option[T]` | |
| `filter` | `fn filter(own self, pred: own OnceFn(T) -> bool) -> Option[T]` | `None` unless the value passes |
| `ok_or` | `fn ok_or[E](own self, err: own E) -> Result[T, E]` | |
| `ok_or_else` | `fn ok_or_else[E](own self, f: own OnceFn() -> E) -> Result[T, E]` | `f` runs only on `None` |
| `zip` | `fn zip[U](own self, other: own Option[U]) -> Option[(T, U)]` | `Some` only if both are |
| `take` | `fn take(mut ref self) -> Option[T]` | Moves the value out and leaves `None` |
| `replace` | `fn replace(mut ref self, value: own T) -> Option[T]` | Stores `value` and returns the old contents |
| `get_or_insert_with` | `fn get_or_insert_with(mut ref self, f: own OnceFn() -> T) -> mut ref T` | Fills a `None` with `f()`, then returns a reference to the value |
| `as_ref` | `fn as_ref(self) -> Option[ref T]` | A view of the value, without moving it |
| `cloned` | `fn cloned(own self) -> Option[T]` on `Option[ref T]`, `where T: Clone` | An owned copy of a viewed value: `m.get(k).cloned() ?? d` gives a default for a non-`Copy` value |
| `flatten` | `fn flatten(own self) -> Option[T]` on `Option[Option[T]]` | |

## `Result[T, E]`

| Method | Signature | Notes |
|---|---|---|
| `is_ok` | `fn is_ok(self) -> bool` | |
| `is_err` | `fn is_err(self) -> bool` | |
| `ok` | `fn ok(own self) -> Option[T]` | Drops an error |
| `err` | `fn err(own self) -> Option[E]` | Drops a value |
| `map` | `fn map[U](own self, f: own OnceFn(own T) -> U) -> Result[U, E]` | |
| `map_err` | `fn map_err[F](own self, f: own OnceFn(own E) -> F) -> Result[T, F]` | |
| `and_then` | `fn and_then[U](own self, f: own OnceFn(own T) -> Result[U, E]) -> Result[U, E]` | |
| `or_else` | `fn or_else[F](own self, f: own OnceFn(own E) -> Result[T, F]) -> Result[T, F]` | |
| `unwrap` | `fn unwrap(own self) -> T` | Panics on `Err` |
| `expect` | `fn expect(own self, msg: Str) -> T` | Panics on `Err` with `msg` |
| `unwrap_err` | `fn unwrap_err(own self) -> E` | Panics on `Ok` |
| `unwrap_or` | `fn unwrap_or(own self, default: own T) -> T` | |
| `unwrap_or_else` | `fn unwrap_or_else(own self, f: own OnceFn(own E) -> T) -> T` | |

`.context(msg)`, which turns an error into an `AnyError` with a context message, is in [design.md §10](../design.md#error-and-anyerror).

## Numbers

**Overflow method families.** Every integer type has four families of named arithmetic methods, for each of `add`, `sub`, `mul`, `div`, `rem`, `neg`, `shl`, `shr` and `pow`:

| Family | Returns | On overflow or out-of-range |
|---|---|---|
| `checked_add(y)`, `checked_div(y)`, `checked_pow(e)`, ... | `Option[Self]` | `None` |
| `wrapping_add(y)`, `wrapping_neg()`, ... | `Self` | Two's-complement wrap; a shift amount is taken modulo the bit width |
| `saturating_add(y)`, ... | `Self` | Clamps to the type's minimum or maximum |
| `overflowing_add(y)`, ... | `(Self, bool)` | The wrapped result and whether it overflowed |

What the plain operators do on overflow, and the other numeric rules, are in [design.md §5](../design.md#numeric-semantics).

**Float to integer.** There are two forms ([design.md §5](../design.md#float-to-integer-conversion)):
- `f as iN` saturates: an out-of-range value gives the nearest end of the range, and NaN gives 0.
- `iN.checked_from(f: f64) -> Option[iN]` returns `None` for NaN or an out-of-range value, and otherwise `Some` of the value truncated toward zero. It exists on every integer type; an `f32` argument widens to `f64` losslessly.

**`total_cmp`.** `f32` and `f64` have `fn total_cmp(self, other: Self) -> Ordering`, the IEEE 754 `totalOrder` comparison. It orders every value, NaN included, so it can sort floats directly:

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

Every `parse` returns the one error type, a prelude enum:

```kara
enum ParseError {
    Empty,          // the input is empty
    InvalidDigit,   // the input holds a character that cannot appear in a number of this type
    Overflow,       // the value is above the type's maximum
    Underflow,      // the value is below the type's minimum
    Invalid,        // any other malformed input
}
```

`ParseError` implements `Error` and `Display`. So `?` converts it into `AnyError` ([design.md §10](../design.md#error-and-anyerror)), and into the caller's own error type through a `From` impl.

### Printing

`Display` for `f32` and `f64` prints the shortest digits that read back as the same value, so float output round-trips:
- **Always a `.` or an exponent**, so a float never reads as an integer: `println(1.0)` prints `1.0`, `println(0.1)` prints `0.1`, and `println([1.0, 2.5])` prints `[1.0, 2.5]`.
- **Exponent form when the decimal exponent is below −4 or at least 16**, as Python's `repr` does: `1e16` prints `1e+16`, and `0.00001` prints `1e-05`. Otherwise plain decimal: `0.0001` prints `0.0001`.
- **The special values** print `NaN`, `inf` and `-inf`.

`to_string()` and f-string interpolation use the same rendering.
