# Structs and Enums

## Structs

A struct groups related data together:

```kara
struct Point {
    x: f64,
    y: f64,
}

fn main() {
    let origin = Point { x: 0.0, y: 0.0 };
    let p = Point { x: 3.0, y: 4.0 };

    println(f"({p.x}, {p.y})");
}
```

Struct names are Type-class identifiers (PascalCase); field names are Value-class (snake_case). The compiler enforces both — see [Naming identifiers](./ch02-variables-and-types.md#naming-identifiers) in chapter 2.

### Methods on structs

Use `impl` blocks to attach behavior:

```kara
struct Rectangle {
    width: f64,
    height: f64,
}

impl Rectangle {
    fn area(ref self) -> f64 {
        self.width * self.height
    }

    fn is_square(ref self) -> bool {
        self.width == self.height
    }

    fn new(width: f64, height: f64) -> Rectangle {
        Rectangle { width, height }
    }
}

fn main() {
    let r = Rectangle.new(10.0, 5.0);
    println(f"Area: {r.area()}");
    println(f"Square? {r.is_square()}");
}
```

### Newtype wrappers

For a lightweight wrapper around a single existing type, use a `distinct type`:

```kara
distinct type Meters = f64;
distinct type Seconds = f64;
```

These are distinct types — you can't accidentally pass `Meters` where `Seconds` is expected, even though both wrap `f64`. (See [Appendix C](./appendix-c-derivable-traits.md) for `#[derive(Arithmetic)]`, which restores `+`/`-`/`*` on a `distinct type`.)

## Enums

An enum defines a type that can be one of several variants:

```kara
enum Direction {
    North,
    South,
    East,
    West,
}

fn describe(d: Direction) -> String {
    match d {
        Direction.North => "going up",
        Direction.South => "going down",
        Direction.East => "going right",
        Direction.West => "going left",
    }
}
```

### Enums with data

Variants can carry data — this is what makes Kāra enums algebraic data types:

```kara
enum Shape {
    Circle(f64),                    // radius
    Rectangle(f64, f64),            // width, height
    Triangle { a: f64, b: f64, c: f64 },  // named fields
}

fn area(shape: Shape) -> f64 {
    match shape {
        Shape.Circle(r) => 3.14159 * r * r,
        Shape.Rectangle(w, h) => w * h,
        Shape.Triangle { a, b, c } => {
            let s = (a + b + c) / 2.0;
            (s * (s - a) * (s - b) * (s - c)).sqrt()
        }
    }
}
```

### Option and Result

Two enums are so fundamental they're in the prelude — available everywhere without import:

```kara
enum Option[T] {
    Some(T),
    None,
}

enum Result[T, E] {
    Ok(T),
    Err(E),
}
```

`Option` represents a value that might not exist. `Result` represents an operation that might fail. You'll use them constantly:

```kara
fn find_user(id: u64) -> Option[User] {
    // returns Some(user) or None
}

fn parse_number(s: String) -> Result[i64, ParseError] {
    // returns Ok(number) or Err(error)
}
```

We'll cover error handling patterns in depth in [Chapter 7](./ch07-error-handling.md).

## Shared types

By default, structs and enums have value semantics — assigning or passing them moves or copies the data. For types that need reference semantics (shared ownership, graph structures), prefix with `shared`:

```kara
shared struct Node {
    value: i64,
    children: Vec[Node],
}
```

A `shared struct` is automatically reference-counted. Multiple owners can point to the same data without explicit `Rc` or `Arc` wrappers. The compiler picks the right reference-counting strategy behind the scenes.

Use `shared` when your data naturally has multiple owners. Use regular structs (the default) for everything else.

### Mutable fields

A field of a `shared struct` is read-only unless you declare it `mut`. Because a
shared value can have several owners, mutation through a shared reference is
opt-in per field — marking a field `mut` is how you say "this one is meant to
change in place":

```kara
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],   // reassignable; `val` is not
}
```

(Plain value structs don't need this — there you control mutation with a `mut`
binding, `let mut p = Point { ... }`. The per-field `mut` is specific to
`shared`, where the binding alone can't decide it.)

### A linked list

The recursive `shared struct` + `Option` pair is the standard singly-linked
list — each node owns the next, and `None` marks the end:

```kara
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

// Build a list from a slice, tail to head, preserving order.
fn from_slice(xs: Slice[i64]) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = xs.len() - 1;
    while i >= 0 {
        head = Some(ListNode { val: xs[i], next: head });
        i = i - 1;
    }
    head
}
```

Walk it by unwrapping each node. `if let` peels off one `Some` and recurses on
`.next`; `while let` does the same thing iteratively, rebinding the cursor until
it hits `None`:

```kara
fn sum(node: Option[ListNode]) -> i64 {
    if let Some(n) = node {
        n.val + sum(n.next)
    } else {
        0i64
    }
}

fn length(node: Option[ListNode]) -> i64 {
    let mut count = 0i64;
    let mut cur = node;
    while let Some(n) = cur {
        count = count + 1;
        cur = n.next;
    }
    count
}
```

Trees are the same shape with two children instead of one (`mut left`,
`mut right`) — see the `TreeNode` example in [Ownership](./ch12-ownership.md#shared-types).
