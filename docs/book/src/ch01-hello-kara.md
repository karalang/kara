# Hello, Kāra

Every journey starts with a first program. Here's yours:

```kara,run
fn main() {
    println("Hello, world!");
}
```

Let's break it down:

- `fn main()` declares the program's entry point. Every Kāra executable starts here.
- `println(...)` prints a line to standard output. It's available everywhere — no imports needed.
- Semicolons end statements. Braces delimit blocks. If you've used C, Rust, Go, or Java, this feels familiar.

## A slightly bigger program

```kara,run
fn greet(name: String) {
    println(f"Hello, {name}!");
}

fn main() {
    greet("Kāra");
    greet("world");
}
```

A few things to notice:

- **Functions are declared with `fn`**, parameters are `name: Type`.
- **String interpolation** uses `f"..."` with `{expr}` inside. No format macros, no concatenation — just prefix the string with `f` and write expressions in braces.
- **No `return` needed.** The last expression in a block is its value. You *can* use `return` for early exits, but for the common case you just write the value.

## Comments

```kara
// This is a line comment.

/* This is a block comment.
   Block comments /* can nest */. */
```

## What you get for free

Even in this tiny program, the Kāra compiler is doing work behind the scenes:

- **Effect inference**: `greet` writes to stdout via `println`. The compiler knows this — it infers a `writes(Stdout)` effect. You didn't have to declare it because `greet` isn't a public API function. Save the program as `main.kara` and `karac explain main.kara.greet` prints the effect and where it comes from.
- **Borrowing by default**: `name` is declared `String`, and a bare parameter *borrows*. `greet` reads the caller's string without taking it, so a caller can go on using its own string after the call. A function that keeps what it is given says so in its signature, `name: own String`; one that changes it writes `name: mut ref String`. Chapter 12 covers the three modes.

You don't need to understand effects or ownership yet. The point is that the compiler is your partner from the very first line of code — quietly making good decisions so you can focus on what your program does.

We'll explore both systems in depth in later chapters.
