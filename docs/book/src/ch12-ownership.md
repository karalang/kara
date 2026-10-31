# Ownership Without the Fight

If you've used Rust, you know the ownership system: powerful but demanding. Lifetimes, borrowing rules, the borrow checker rejecting code you know is safe.

Kāra keeps the semantics and drops the lifetimes. A parameter borrows unless its signature says otherwise: `own` takes ownership and `mut ref` borrows to write. The compiler checks that the body matches, and RC fills in when a single owner can't be proven.

## The three tiers

Every value in Kāra lives in one of three ownership tiers:

1. **Owned** — the default. The value has one owner. When the owner goes out of scope, the value is dropped.
2. **Borrowed** — a temporary view of someone else's value. A bare parameter is a shared, read-only borrow; a `mut ref` parameter is exclusive and mutable. Like Rust's `&T` / `&mut T`, without lifetime variables.
3. **RC (reference-counted)** — shared ownership. Multiple owners, reference-counted. The compiler adds this automatically when needed.

You write the tier at the signature. The compiler infers *nothing* about the public contract — what the source says is what callers see.

## Parameter modes

Every parameter has one mode, written in the signature. A bare parameter borrows (a `Copy` value such as an `i64` is simply copied); `own` takes ownership; `mut ref` borrows exclusively to write:

```kara
fn greet(name: String) {
    println(f"Hello, {name}!");    // borrowed: read, not consumed
}

fn take_name(name: own String) -> String {
    name    // consumed — owned parameter, moves out
}

fn add_suffix(name: mut ref String) {
    name.push_str("!");    // exclusive borrow — mutates in place
}
```

Three rules, one per mode. The body must match: consuming an `own` parameter is fine; consuming a bare (borrowed) parameter is a compile error; writing through a `mut ref` is fine, consuming it is not.

### Receivers

Methods follow the same rule. Bare `self` is a shared borrow, `own self` is the consuming receiver, `mut ref self` is an exclusive borrow:

```kara,ignore
impl Builder {
    fn build(own self) -> Widget { ... }          // consumes self
    fn peek(self) -> i64 { self.count }           // reads self
    fn bump(mut ref self) { self.count = self.count + 1 }   // mutates self
}
```

A `ref self` or `x: ref T` still parses as a redundant spelling of the bare form, and `karac fix` removes the `ref`.

### `karac query ownership`

`karac query ownership <file>.<function>` reports each parameter's mode as JSON, so a tool or an agent can read the contract without parsing the source. It reports what the signature says: callers always see what you wrote.

## Call sites

At call sites, mutation gets a marker when the argument is a fresh binding passed to a `mut ref T` or `mut Slice[T]` parameter:

```kara,ignore
let mut v = [3, 1, 4, 1, 5];
sort_in_place(mut v);          // fresh binding → marker required
```

Inside a function that already holds the binding as a `mut ref`, you don't repeat the marker — the mutation was announced at the callee's signature:

```kara,ignore
fn helper(s: mut ref State) {
    update(s.cache);           // field through a mut-ref root → no marker
    reset(s.counter);          // same — forwarded
}
```

Method calls, field assignment, and index assignment never carry the marker:

```kara,ignore
v.push(x);                     // method call — silent
s.field = 5;                   // field assignment — silent
v[i] = x;                      // index assignment — silent
```

`ref` is never written at call sites — the signature carries the mode. `f(ref v)` is a parse error.

## Move semantics

When a value is moved, the original binding is gone:

```kara
let a = Vec.new();
let b = a;           // `a` is moved into `b`
// println(a);       // compile error: `a` has been moved
println(b);          // ok
```

This prevents use-after-move bugs at compile time. No dangling pointers, no double frees.

## RC fallback

Sometimes the compiler can't prove a single-owner model works — the value is shared across data structures, or its lifetime can't be statically determined. In these cases, the compiler falls back to reference counting:

```kara,ignore
let node = Node { value: 42, children: Vec.new() };
// If `node` ends up shared across a graph structure,
// the compiler automatically wraps it in RC.
```

You don't write `Rc[Node]` or `Arc[Node]`. The source code always says `Node`. The compiler picks the representation, and `karac query cost-summary` counts the reference-counting operations it emitted.

## Slices

A slice is a borrowed view into contiguous memory — a pointer and a length, nothing more. Kāra has two:

- `StringSlice` — a view into a `String`.
- `Slice[T]` — a view into any sequence of `T` (usually a `Vec[T]` or `Array[T, N]`).

Slices let one function work over many container types:

```kara,ignore
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}

let v: Vec[i64] = [1, 2, 3, 4];
let a: Array[i64, 3] = [10, 20, 30];

sum(v);         // Vec coerces to Slice at the call boundary
sum(a);         // Array coerces too
sum(v[1..3]);   // a sub-range is also a Slice
```

You don't write `sum(v.as_slice())` — the compiler inserts the coercion when a call expects `Slice[T]` and the argument is a compatible owned or borrowed container. When you need a slice as a first-class value (stored in a `let`, captured by a closure), call `.as_slice()` explicitly.

### Mutable slices

For in-place operations, use `mut Slice[T]` — the same `mut` modifier Kāra uses everywhere else:

```kara,ignore
fn sort_in_place[T: Ord](xs: mut Slice[T]) { /* ... */ }

let mut v = [3, 1, 4, 1, 5];
sort_in_place(mut v);          // mutably borrow the whole Vec
sort_in_place(mut v[1..4]);    // or just a sub-range
```

### Why slices matter

Without slices, a function that operates on a sequence has to choose between being too restrictive (`Vec[i64]` — rejects arrays) and too generic (a trait bound — loses O(1) indexed access). Slices give you the middle ground: one signature that works over any contiguous sequence, with full random access.

## shared types

For types that are *designed* for shared ownership, use `shared`:

```kara
shared struct TreeNode {
    value: i64,
    left: Option[TreeNode],
    right: Option[TreeNode],
}
```

`shared struct` means: this type always uses reference counting. It's the right tool for trees, graphs, and any structure where multiple parents point to the same child.

## The philosophy

Kāra's ownership model: **Rust semantics, no lifetimes, one word per mode.**

- Signatures declare the mode: bare borrows, `own` takes ownership, `mut ref` borrows to write.
- Call sites mark mutation for fresh bindings with `mut`; forwarded mut-refs and method calls stay silent.
- The compiler never silently copies expensive data. Moves are explicit in the semantics.
- When you need to see what the compiler chose, `karac query` shows you: `ownership` for modes, `cost-summary` for the RC operations it emitted.
- Lifetimes never appear in source. The compiler infers borrow scoping below the signature surface.

The goal is Rust-level safety with mainstream-language readability — no `<'a>`, no turbofish, one unified rule for borrows across free functions, methods, and traits.
