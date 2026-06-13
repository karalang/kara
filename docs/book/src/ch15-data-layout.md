# Data Layout

Most languages give you no control over how data is arranged in memory. Kāra lets you separate *what* your data is from *how* it's stored — without changing the logical API.

## Why layout matters

Modern CPUs are memory-bound, not compute-bound. Cache misses dominate performance. How your data is laid out in memory — whether related fields are next to each other, whether you're iterating over dense arrays or chasing pointers — matters more than most algorithmic optimizations.

## Layout blocks

A `layout` block defines physical memory organization separately from the struct definition:

```kara
struct Particle {
    position: Vec3,
    velocity: Vec3,
    mass: f64,
    color: Color,
    active: bool,
}

layout Particle {
    group hot { position, velocity }    // fields accessed together in physics loop
    group cold { color, active }        // rarely accessed during simulation
}
```

The struct's logical API doesn't change — you still write `p.position` and `p.color`. But the compiler lays out `hot` fields contiguously for cache-friendly iteration and keeps `cold` fields separate.

## SoA transforms

For arrays of structs, layout blocks can request Structure-of-Arrays (SoA) layout:

```kara
layout Particle {
    soa    // each field becomes its own contiguous array
}
```

An `Array[Particle, 1000]` with `soa` layout stores all positions together, all velocities together, etc. — ideal for SIMD and cache performance. The logical interface (`particles[i].position`) stays the same.

## When to use layout control

Most code doesn't need layout blocks. Use them when:

- You have hot loops iterating over large arrays of structs.
- Profiling shows cache misses dominating.
- You want SoA layout for SIMD-friendly processing.

For everything else, let the compiler pick the layout. It's implementation freedom — the compiler can optimize within the constraints you give it.
