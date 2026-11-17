# Data Layout

Most languages give you no control over how data is arranged in memory. Kāra lets you separate *what* your data is from *how* it's stored — without changing the logical API.

## Why layout matters

Modern CPUs are memory-bound, not compute-bound. Cache misses dominate performance. How your data is laid out in memory — whether related fields are next to each other, whether you're iterating over dense arrays or chasing pointers — matters more than most algorithmic optimizations.

## Layout blocks (a later track)

A `layout` block will reorganize the memory of a collection, a `Vec[T]` or
`Array[T, N]`, without touching the struct definition or the code that uses it:

```kara,ignore
struct Particle { x: f64, y: f64, name: String }

// x and y share one contiguous array (the physics hot path);
// name is cold, a separate allocation the hot loop never touches.
layout swarm: Vec[Particle] {
    group hot { x, y }
    cold { name }
}
```

Layout blocks are planned but not in 0.8 (deferred.md § Layout): the compiler
refuses the block (`E_DEFERRED_LAYOUT_BLOCK`). Until they land, a hot loop that
reads only some fields gets the same dense memory from a struct of arrays you
write yourself:

```kara
struct Swarm {
    xs: Vec[f64],
    ys: Vec[f64],
    names: Vec[String],
}

fn drift(swarm: Swarm) -> f64 {
    let mut sum = 0.0;
    let mut i = 0;
    while i < swarm.xs.len() {
        sum = sum + swarm.xs[i] + swarm.ys[i];   // never touches `names`
        i = i + 1;
    }
    sum
}
```

Most code doesn't need this. Reach for it when a hot loop iterates over a large
collection of structs, reads only a few of their fields, and profiling shows
cache misses dominating.
