# Additive interop — a Kāra kernel in a C and a Rust program

The adoption pitch for a language with no ecosystem yet: **be additive, not
a replacement.** Write the hot / parallel kernel in Kāra, build it as a
linkable library with a C ABI, and drop it into an existing C or Rust
program that keeps everything else — the Rust-in-Firefox / Zig-alongside-C
playbook. This directory is the worked proof-point.

Spec: [`docs/design.md` § Exported C ABI](../../docs/design.md#exported-c-abi).
Spike: [`docs/spikes/additive-interop-adoption.md`](../../docs/spikes/additive-interop-adoption.md).

## The kernel

[`kernel.kara`](kernel.kara) exports three `pub extern "C" fn`s — the
public C surface — plus a `#[repr(C)]` struct that crosses the boundary by
value. There is no `main`: a library artifact has no entry point.

```
karac build kernel.kara --crate-type staticlib   # → libkernel.a  + libkernel.h
karac build kernel.kara --crate-type cdylib       # → libkernel.so + libkernel.h
```

`karac` emits the companion header ([the cbindgen analogue](kernel.kara))
so the caller `#include`s it instead of hand-transcribing signatures.

## C host — links the static archive

```
karac build kernel.kara --crate-type staticlib
cc host.c libkernel.a -lpthread -lm -ldl -o host_c
./host_c        # => add=42 fib=6765 mean=7.50
```

The static archive is **thick** — it bundles the Kāra runtime — so the C
program links and runs with **no karac toolchain present**. That is the
whole point: hand a C team a `.a` + a `.h`.

## Rust host — links the shared library

```
karac build kernel.kara --crate-type cdylib -o libkernel.so
rustc host.rs -L . -C link-arg=-Wl,-rpath,. -o host_rs
./host_rs       # => add=42 fib=6765 mean=7.50
```

**Rust hosts must link the `cdylib`, not the `staticlib`.** The Kāra runtime
is a Rust crate that bundles `std`, so a `.a` carries std symbols
(`rust_eh_personality`, allocator shims, …) that collide with the Rust
host's own `std` at static-link time. A `.so` encapsulates those internal
symbols and the dynamic linker resolves only the exported entry points. C
hosts have no `std` to clash with and can use either artifact.

## "Call Rust crates cleanly" — the honest version

Rust has no stable ABI, so *nothing* links arbitrary Rust crates directly.
The durable bridge is the C ABI: a Rust crate exposes
`#[no_mangle] pub extern "C" fn` + `#[repr(C)]` types (the pyo3 / cxx /
uniffi pattern), and *then* Kāra calls it exactly like C. The reverse — a
Rust program calling *into* Kāra — is what [`host.rs`](host.rs) shows, and
it too goes through the C boundary.

## What crosses, and what doesn't (v1)

| Kāra type | Crosses as |
|---|---|
| primitives (`i32`, `f64`, `bool`, `usize`, …) | the matching C fixed-width type |
| `*const T` / `*mut T` | `const T*` / `T*` |
| `#[repr(C)] struct` | the same C struct, emitted into the header |
| `Vec` / `String` / `enum` / default-layout struct | an opaque `KaraHandle` (`void*`) — pointer only, never `free()`d by the caller |

The ownership handoff behind an opaque handle (who frees a Kāra-boxed
`Vec` the C side received) is the `forget` primitive — the next spike
slice, co-designed with the ownership-mechanization spike. This example
stays within the transparent set.
