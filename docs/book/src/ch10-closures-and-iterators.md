# Closures and Iterators

## Closures

Anonymous functions that can capture variables from their environment:

```kara
let add = |a: i64, b: i64| a + b;
println(add(2, 3));    // 5

let threshold = 10;
let is_big = |n: i64| n > threshold;    // captures `threshold`
println(is_big(15));   // true
```

Closure parameters take type annotations just like `fn` parameters. When a closure is passed straight into a combinator like `.map()`, the element type flows in and the annotation can be omitted (see *Iterators* below); a closure bound to a `let` with no other context needs the annotation.

### Closures as parameters

Functions can accept closures:

```kara,ignore
fn apply_twice(f: Fn(i64) -> i64, x: i64) -> i64 {
    f(f(x))
}

let double = |n| n * 2;
println(apply_twice(double, 3));    // 12
```

### Closures and effects

Closures inherit effects from the code they contain. A closure that calls `println` carries a `writes(Stdout)` effect. The effect system tracks this through higher-order functions — no surprise side effects hiding in callbacks.

## Sorting

The most common place you'll hand a closure to the standard library is sorting.
`Vec` sorts **in place**, so the binding must be `mut`:

```kara
let mut v = [3, 1, 4, 1, 5, 9, 2, 6];
v.sort();                       // natural ascending order
```

For any other order, `sort_by` takes a comparator closure `|a, b| ...` that
returns an ordering. Produce one by comparing two elements with `.cmp()`:

```kara
v.sort_by(|a, b| a.cmp(b));     // ascending — same as v.sort()
v.sort_by(|a, b| b.cmp(a));     // descending — flip the operands
```

`a.cmp(b)` answers "how does `a` order relative to `b`?", so putting `b` first
reverses the direction. The same shape sorts by a derived key — compare the keys
instead of the whole elements:

```kara
// pairs: Vec[(i64, i64)] — order by the second component, descending
pairs.sort_by(|a, b| b.1.cmp(a.1));
```

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

Because closures carry the effects of their bodies (see above), iterator chains stay honest about effects too: a `map` whose closure writes to stdout contributes `writes(Stdout)` to the enclosing function's effect row. The same `map` / `filter` / `take` combinators apply whether the elements come from an in-memory `Vec` or an effectful source — one combinator library, no separate `Stream` type.

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

```kara,ignore
let result = data
    |> parse
    |> validate
    |> transform;
```
