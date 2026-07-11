# Data Layout

Most languages give you no control over how data is arranged in memory. Kāra lets you separate *what* your data is from *how* it's stored — without changing the logical API.

## Why layout matters

Modern CPUs are memory-bound, not compute-bound. Cache misses dominate performance. How your data is laid out in memory — whether related fields are next to each other, whether you're iterating over dense arrays or chasing pointers — matters more than most algorithmic optimizations.

## Layout blocks

A `layout` block reorganizes the memory of a **collection** — a `Vec[T]` or
`Array[T, N]` — without touching the struct definition or the code that uses it.
You attach it to a *binding*, not to the type: `layout <name>: Vec[T] { ... }`.

```kara
struct Particle { x: f64, y: f64, name: String }

// x and y share one contiguous array (the physics hot path);
// name is cold — a separate allocation the hot loop never touches.
layout swarm: Vec[Particle] {
    group hot { x, y }
    cold { name }
}
```

Now `swarm`'s storage is **Structure-of-Arrays**: all the `x`s and `y`s sit
together in the `hot` group's backing array instead of being interleaved with
each `name`. The logical API is unchanged — you still write `swarm[i].x` — and a
loop that reads only `x` and `y` streams through dense memory:

```kara,ignore
fn drift(swarm: ref Vec[Particle]) -> f64 {
    let mut sum = 0.0;
    let mut i = 0i64;
    while i < swarm.len() {
        sum = sum + swarm[i].x + swarm[i].y;   // touches only the hot group
        i = i + 1;
    }
    sum
}
```

Three directives go inside the block:

- `group <name> { fields }` — the named fields become one contiguous array (the SoA transform). Use several groups to keep fields that are read together on the same cache line.
- `cold { fields }` — moves rarely-accessed fields to a separate allocation, out of the hot path. At most one `cold` section per block.
- `align(N)` — forces a group's backing array onto an `N`-byte boundary (e.g. `align(64)` for a cache line), the standard fix for false sharing between threads.

Every field must be placed in exactly one group or in `cold` — the compiler
rejects a layout that leaves a field unassigned, so the storage is never
ambiguous. There is no `soa` keyword: grouping a collection's fields *is* the SoA
transform, and the default (no `layout` block) is plain array-of-structs.

Because a single element's fields are now scattered across the group arrays, no
one contiguous region *is* a whole `Particle` — so you can't borrow a whole
element out of an SoA collection. Reading a field (`swarm[i].x`) works, and you
can always materialize a plain array-of-structs *copy* of one element:

```kara,ignore
let e = swarm[1];       // an array-of-structs copy of one element
println(e.name);
```

## When to use layout control

Most code doesn't need layout blocks. Use them when:

- You have hot loops iterating over large arrays of structs.
- Profiling shows cache misses dominating.
- You want SoA layout for SIMD-friendly processing.

For everything else, let the compiler pick the layout. It's implementation freedom — the compiler can optimize within the constraints you give it.
