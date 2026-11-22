# Traits and Generics

## Traits

A trait defines shared behavior — a set of methods that different types can implement:

```kara,name=b-area
trait Area {
    fn area(self) -> f64;
}
```

Types implement traits with `impl`:

```kara,uses=b-area
struct Circle {
    radius: f64,
}

struct Rectangle {
    width: f64,
    height: f64,
}

impl Area for Circle {
    fn area(self) -> f64 {
        3.14159 * self.radius * self.radius
    }
}

impl Area for Rectangle {
    fn area(self) -> f64 {
        self.width * self.height
    }
}
```

### Default methods

Traits can provide default implementations:

```kara
trait Describable {
    fn name(self) -> String;

    fn description(self) -> String {
        f"A thing called {self.name()}"
    }
}
```

Types that implement `Describable` must provide `name`, but get `description` for free. They can override it if they want.

## Generics

Generics let you write code that works with any type. Kāra uses `[T]` syntax — not `<T>`:

```kara
fn first[T](items: Vec[T]) -> Option[ref T] {
    items.get(0)
}
```

This works for `Vec[i64]`, `Vec[String]`, `Vec[User]` — anything.

### Generic structs

```kara
struct Pair[A, B] {
    first: A,
    second: B,
}

let p = Pair { first: "hello", second: 42 };
```

### Generic with trait bounds

Constrain what types are allowed:

```kara
fn largest[T: Ord + Clone](items: Vec[T]) -> T {
    let mut best = items[0].clone();
    for item in items {
        if item > best {
            best = item.clone();
        }
    }
    best
}
```

`T: Ord` means "T must implement the Ord trait" — so we know `>` works, and
`+ Clone` is what lets `items[0].clone()` take a copy: `items[0]` is a
BORROW of an element, and a container cannot be left with a hole in it, so
a non-`Copy` element has to be cloned (or borrowed) rather than moved out.
As shown here, multiple bounds use `+`:

```kara
fn print_sorted[T: Ord + Display](items: own Vec[T]) {
    let mut sorted = items;
    sorted.sort();
    for item in sorted {
        println(item);
    }
}
```

### Why `[T]` instead of `<T>`?

No ambiguity with comparison operators. `Vec[i32]` can't be misread as "is Vec less than i32." No turbofish needed. The tradeoff is that `[` does double duty for generics and indexing, but the parser disambiguates by context:

- **Type positions** (annotations, return types): `Vec[i64]` is always generic.
- **Expression positions**: `arr[0]` is always an index. A function never takes type arguments where it is called: `sort(data)` infers `T` from `data`, and where nothing can, an annotation pins it (`let v: Vec[i32] = Vec.new();`). Writing `sort[i32](data)` is an error that says so.

## Putting it together

Here's a generic function with a trait bound and a return type:

```kara,run
fn find[T: Eq](items: Vec[T], target: T) -> Option[i64] {
    for i in 0..items.len() {
        if items[i] == target {
            return Some(i);
        }
    }
    None
}

fn main() {
    let names = ["Alice", "Bob", "Charlie"];
    match find(names, "Bob") {
        Some(i) => println(f"Found at index {i}"),
        None => println("Not found"),
    }
}
```

The compiler infers `T = String` from the arguments. No annotation needed at the call site.
