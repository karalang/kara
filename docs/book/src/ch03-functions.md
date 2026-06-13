# Functions

## Declaring functions

Functions are declared with `fn`, parameters are `name: Type`, and the return type follows `->`:

```kara
fn add(a: i64, b: i64) -> i64 {
    a + b
}

fn greet(name: String) {
    println(f"Hello, {name}!");
}
```

- The last expression in the body is the return value. No `return` keyword needed.
- If a function doesn't return a value, omit the `-> Type`.
- Use `return` for early exits:

```kara
fn first_positive(numbers: Vec[i64]) -> Option[i64] {
    for n in numbers {
        if n > 0 {
            return Some(n);
        }
    }
    None
}
```

## Expressions, not statements

Almost everything in Kāra is an expression that produces a value. `if/else` is an expression:

```kara
fn abs(x: i64) -> i64 {
    if x >= 0 { x } else { -x }
}
```

`match` is an expression:

```kara
fn describe(n: i64) -> String {
    match n {
        0 => "zero",
        1..=9 => "single digit",
        _ => "big number",
    }
}
```

This means you rarely need temporary variables — you can use control flow inline wherever a value is expected.

## Parameter modes: the compiler helps

Here's a function that reads a string but doesn't consume it:

```kara
fn char_count(text: String) -> usize {
    text.len()
}
```

You wrote `text: String`, but the compiler notices that `text` is only read, never moved or mutated. It automatically infers that `text` should be passed by reference. The caller doesn't make a copy; `char_count` borrows the string.

You can also be explicit:

```kara
fn char_count(text: ref String) -> usize {
    text.len()
}
```

Both versions behave identically. The inference just saves you the annotation. We'll cover ownership in depth in [Chapter 12](./ch12-ownership.md).

## Methods

Functions can be attached to types using `impl` blocks:

```kara
struct Circle {
    radius: f64,
}

impl Circle {
    fn area(ref self) -> f64 {
        3.14159 * self.radius * self.radius
    }

    fn scale(mut ref self, factor: f64) {
        self.radius = self.radius * factor;
    }

    fn new(radius: f64) -> Circle {
        Circle { radius }
    }
}
```

- `ref self` — the method borrows the value (reads only).
- `mut ref self` — the method borrows mutably (can modify fields).
- No `self` parameter — it's an associated function (like a static method). Call it as `Circle.new(5.0)`.

Methods use **Universal Function Call Syntax (UFCS)**. These two calls are the same:

```kara
let c = Circle.new(5.0);
c.area()           // method syntax
Circle.area(c)    // function syntax — same thing
```
