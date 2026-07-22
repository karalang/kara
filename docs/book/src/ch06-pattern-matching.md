# Pattern Matching

Pattern matching is one of Kāra's most expressive features. The `match` expression lets you destructure data and branch on its shape — and the compiler guarantees you handle every case.

## Basic matching

```kara
fn classify(n: i64) -> String {
    match n {
        0 => "zero",
        1 | 2 | 3 => "small",
        4..=9 => "medium",
        _ => "large",
    }
}
```

- `|` matches multiple values.
- `..=` matches inclusive ranges.
- `_` is the wildcard — matches anything.

## Destructuring enums

This is where `match` really shines:

```kara
enum Message {
    Quit,
    Echo(String),
    Move { x: i64, y: i64 },
}

fn handle(msg: Message) {
    match msg {
        Message.Quit => println("Goodbye."),
        Message.Echo(text) => println(f"Echo: {text}"),
        Message.Move { x, y } => println(f"Moving to ({x}, {y})"),
    }
}
```

Each variant's data is extracted directly into variables. No casting, no type-checking at runtime — the compiler knows the structure at compile time.

## Destructuring structs

```kara
struct Point {
    x: f64,
    y: f64,
}

fn describe(p: Point) -> String {
    match p {
        Point { x: 0.0, y: 0.0 } => "origin",
        Point { x, y: 0.0 } => f"on the x-axis at {x}",
        Point { x: 0.0, y } => f"on the y-axis at {y}",
        Point { x, y } => f"at ({x}, {y})",
    }
}
```

## Nested patterns

Patterns compose. Match on the shape of nested data:

```kara,ignore
fn get_name(user: Option[User]) -> String {
    match user {
        Some(User { name, .. }) => name,
        None => "anonymous",
    }
}
```

`..` ignores the remaining fields you don't care about.

## Guards

Add conditions with `if`:

```kara
fn classify_temp(temp: f64) -> String {
    match temp {
        t if t < 0.0 => "freezing",
        t if t < 20.0 => "cold",
        t if t < 30.0 => "comfortable",
        _ => "hot",
    }
}
```

The variable `t` binds the matched value, and the `if` clause adds an extra condition.

## Exhaustiveness

The compiler requires that `match` covers every possible case. This is enforced at compile time:

```kara
enum Color {
    Red,
    Green,
    Blue,
}

fn name(c: Color) -> String {
    match c {
        Color.Red => "red",
        Color.Green => "green",
        // compile error: non-exhaustive match — Color.Blue not covered
    }
}
```

This is especially powerful with enums: if you add a new variant, the compiler tells you everywhere you need to handle it. No silent bugs from forgotten cases.

## let patterns

You can destructure in `let` bindings too:

```kara,ignore
let Point { x, y } = get_point();
let (first, second) = get_pair();
```

## if let

For when you only care about one variant:

```kara,ignore
if let Some(user) = find_user(42) {
    println(f"Found: {user.name}");
}
```

This is cleaner than a full `match` when you'd just ignore the other cases.
