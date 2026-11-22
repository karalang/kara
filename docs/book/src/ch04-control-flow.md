# Control Flow

## if / else

`if` is an expression — it produces a value:

```kara,uses=b-score
let status = if score >= 90 { "excellent" } else { "keep going" };
```

For side effects, use it as a statement:

```kara,uses=b-temperature
if temperature > 100 {
    println("Warning: overheating!");
} else if temperature > 80 {
    println("Running warm.");
} else {
    println("All good.");
}
```

No parentheses around the condition. Braces are always required.

## Loops

### `while`

```kara
let mut count = 0;
while count < 10 {
    println(count);
    count = count + 1;
}
```

### `for` loops

`for` iterates over anything iterable:

```kara
let names = ["Alice", "Bob", "Charlie"];
for name in names {
    println(f"Hello, {name}!");
}
```

With ranges:

```kara
// 0, 1, 2, 3, 4
for i in 0..5 {
    println(i);
}

// 0, 1, 2, 3, 4, 5 (inclusive)
for i in 0..=5 {
    println(i);
}
```

### `loop`

An infinite loop. Use `break` to exit:

```kara,uses=b-try-connect
let mut attempt = 0;
let result = loop {
    attempt = attempt + 1;
    if try_connect() {
        break "connected";
    }
    if attempt >= 3 {
        break "failed";
    }
};
```

`loop` is an expression — `break value` sets the value of the whole loop.

### `break` and `continue`

```kara
for i in 0..100 {
    if i % 2 == 0 {
        continue;    // skip even numbers
    }
    if i > 10 {
        break;       // stop after 10
    }
    println(i);      // prints 1, 3, 5, 7, 9
}
```

## match

Pattern matching is one of the most powerful tools in Kāra. At its simplest, it's a better `switch`:

```kara
let day = 3;
let name = match day {
    1 => "Monday",
    2 => "Tuesday",
    3 => "Wednesday",
    4 => "Thursday",
    5 => "Friday",
    6 | 7 => "Weekend",
    _ => "Invalid",
};
```

But `match` goes far beyond this — it works with enums, structs, nested data, and guards. We'll cover it fully in [Chapter 6](./ch06-pattern-matching.md).

## Chaining calls

Kāra has no pipe operator: `|>` is rejected with a note saying so. To read a
chain of steps in the order they happen, name the intermediate values, or use
method calls, which already read left to right:

```kara,uses=b-pipeline-values
let parsed = transform(data);
let checked = validate(parsed);
let result = save(checked);

let total = line.trim().split(",").count();
```
