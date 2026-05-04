# Closures and Iterators

> *This chapter is a work in progress.*

## Closures

Anonymous functions that can capture variables from their environment:

```kara
let add = |a, b| a + b;
println(add(2, 3));    // 5

let threshold = 10;
let is_big = |n| n > threshold;    // captures `threshold`
println(is_big(15));   // true
```

### Closures as parameters

Functions can accept closures:

```kara
fn apply_twice(f: Fn(i64) -> i64, x: i64) -> i64 {
    f(f(x))
}

let double = |n| n * 2;
println(apply_twice(double, 3));    // 12
```

### Closures and effects

Closures inherit effects from the code they contain. A closure that calls `println` carries a `writes(Stdout)` effect. The effect system tracks this through higher-order functions — no surprise side effects hiding in callbacks.

## Iterators

Iterators let you process sequences lazily:

```kara
let numbers = [1, 2, 3, 4, 5];

let doubled = numbers
    .iter()
    .map(|n| n * 2)
    .filter(|n| n > 4)
    .collect();

// doubled = [6, 8, 10]
```

Iterators work the same over effectful sources. `io.lines()`, `fs.read_lines(path)`, and future channel/consumer adaptors return `impl Iterator` too — the source's effects (`reads(Stdin)`, `blocks`, `receives(Kafka)`, …) flow through `map` / `filter` / `take` into the enclosing function's effect row. One trait, one combinator library; no separate `Stream` type.

### Common iterator methods

```kara
items.map(|x| transform(x))       // transform each element
items.filter(|x| predicate(x))    // keep elements that match
items.fold(init, |acc, x| ...)    // reduce to a single value
items.any(|x| x > 10)             // true if any element matches
items.all(|x| x > 0)              // true if all elements match
items.enumerate()                  // pairs of (index, value)
items.zip(other)                   // pairs from two iterators
items.take(n)                      // first n elements
items.skip(n)                      // skip first n elements
```

### The pipe operator with iterators

The pipe operator `|>` chains transformations naturally:

```kara
let result = data
    |> parse
    |> validate
    |> transform;
```
