# Collections

## Sequence literals

The bare form `[1, 2, 3]` creates a `Vec` — growable, heap-allocated, pushed into, returned from functions:

```kara
let names = ["Alice", "Bob", "Charlie"];   // Vec[String]
let numbers = [1, 2, 3];                   // Vec[i64]
```

When you want a different collection, write its name as a prefix:

```kara
let xs = Array[1, 2, 3];         // Array[i64, 3] — fixed-size, stack-allocated
let s  = Set[1, 2, 3];           // Set[i64]
let m  = Map["a": 1, "b": 2];    // Map[String, i64]
```

This form works anywhere a value is expected — function arguments, return values — where a binding's type annotation can't reach.

## Vec

The growable array. The most common collection:

```kara
let mut numbers = Vec.new();
numbers.push(1);
numbers.push(2);
numbers.push(3);

// Or initialize with values:
let names = ["Alice", "Bob", "Charlie"];

for name in names {
    println(name);
}

println(names[0]);          // "Alice"
println(names.len());       // 3
```

## Arrays

Fixed-size, stack-allocated. Size is part of the type — `Array[i64, 4]` and `Array[i64, 5]` are different types.

```kara
let xs = Array[10, 40, 20, 30];          // Array[i64, 4] — size and type inferred
let scores = Array[0; 4];                // Array[i64, 4] — four zeros via repeat form

// Or declare with an annotation:
let data: Array[i64, 4] = [10, 40, 20, 30];

let mut buf: Array[u8, 256] = [0; 256];  // annotation propagates u8 into elements
buf[0] = 100;
buf[1] = 85;
```

## Map

Key-value pairs:

```kara
let mut ages = Map.new();
ages.insert("Alice", 30);
ages.insert("Bob", 25);

// Or initialize with values:
let scores = Map["Alice": 10, "Bob": 7];

match ages.get("Alice") {
    Some(age) => println(f"Alice is {age}"),
    None => println("Not found"),
}
```

## Set

Unique values:

```kara
let mut seen = Set.new();
seen.insert("hello");
seen.insert("world");
seen.insert("hello");    // no effect, already present

// Or initialize with values:
let colors = Set["red", "green", "blue"];

println(seen.len());     // 2
```

## Tuples

Fixed-size, mixed-type groups:

```kara
let pair = (42, "hello");
let (number, text) = pair;

fn min_max(items: Vec[i64]) -> (i64, i64) {
    // return both at once
    (items.min(), items.max())
}
```
