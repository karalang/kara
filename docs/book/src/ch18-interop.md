# Kāra as a Library: Additive Interop

A new language has a bootstrapping problem: no ecosystem yet, and no team wants to rewrite a working system to get one feature. Kāra's answer is to be **additive, not a replacement.** Write the hot loop, the parallel kernel, the one function that has to be fast, in Kāra — build it as a linkable library with a plain C ABI — and drop it into a program that keeps everything else. This is the Rust-in-Firefox, Zig-alongside-C playbook, and Kāra is built to be the guest.

This chapter walks one kernel from `.kara` source into both a C and a Rust host, side by side. Every command and every line of output below is real — the two hosts print the same numbers, which is the whole point.

## The kernel

A library has no `main`. It has an **exported surface**: the functions a caller is allowed to reach. In Kāra that surface is every `pub extern "C" fn`. Save this as `kernel.kara`:

```kara
#[repr(C)]
pub struct Stats { sum: f64, count: i64 }

// A simple scalar export.
pub extern "C" fn add(a: i32, b: i32) -> i32 { a + b }

// A little real work — iterative Fibonacci.
pub extern "C" fn fib(n: i64) -> i64 {
    if n < 2 { return n; }
    let mut a = 0;
    let mut b = 1;
    let mut i = 2;
    while i <= n {
        let c = a + b;
        a = b;
        b = c;
        i = i + 1;
    }
    b
}

// A `#[repr(C)]` struct crosses the boundary by value.
pub extern "C" fn stats_mean(s: Stats) -> f64 {
    if s.count == 0 { return 0.0; }
    s.sum / (s.count as f64)
}
```

Two things worth pointing at:

- **`pub extern "C" fn`** is the whole export declaration. `pub` makes it visible; `extern "C"` gives it the C calling convention and an unmangled symbol name, so a C caller reaches `add` as `add`, not some mangled string.
- **`#[repr(C)]`** on `Stats` is a promise about memory layout. A default Kāra struct has no stable physical layout (the compiler is free to reorder fields — see [Data Layout](./ch15-data-layout.md)); `#[repr(C)]` pins it to the C order so it can cross the boundary by value.

## Building the artifact

Two crate types, two artifacts:

```text
$ karac build kernel.kara --crate-type staticlib
Built: libkernel.a
Built: libkernel.h

$ karac build kernel.kara --crate-type cdylib
Built: libkernel.so
Built: libkernel.h
```

`staticlib` produces a `.a` (a `.lib` on Windows); `cdylib` produces a `.so` (`.dylib` on macOS, `.dll` on Windows). Both come with `libkernel.h` — the C header, generated for you, so a caller `#include`s it instead of hand-transcribing signatures. The default artifact name is `lib<stem>.<ext>`, distinct from any executable, so a library build never clobbers a stray binary; `-o` overrides it.

The `.a` is **thick**: it bundles the Kāra runtime. A C program links it and runs with **no karac toolchain present** — that is the deliverable, a `.a` + a `.h` you can hand to a team that has never heard of Kāra.

### The emitted header

Here's what `karac` wrote to `libkernel.h` (trimmed to the body):

```c
#include <stdint.h>
#include <stddef.h>

/* Runtime lifecycle. Call karac_runtime_init() once before the first
 * exported call, and karac_runtime_shutdown() at host teardown. */
void karac_runtime_init(void);
void karac_runtime_shutdown(void);

struct Stats {
    double sum;
    int64_t count;
};

int32_t add(int32_t a, int32_t b);
int64_t fib(int64_t n);
double stats_mean(struct Stats s);
```

The `#[repr(C)]` struct came across as a real C `struct`, the scalars mapped to fixed-width `<stdint.h>` types, and two lifecycle functions appeared that you didn't write — more on those next.

## The C host

The host includes the header and calls in. Nothing from karac is on the compile line — just `cc`, the `.a`, and the `.h`:

```c
#include <stdio.h>
#include "libkernel.h"

int main(void) {
    karac_runtime_init();

    struct Stats s = { .sum = 30.0, .count = 4 };
    printf("add=%d fib=%lld mean=%.2f\n",
           add(20, 22),
           (long long)fib(20),
           stats_mean(s));

    karac_runtime_shutdown();
    return 0;
}
```

```text
$ cc host.c libkernel.a -lpthread -lm -ldl -o host_c
$ ./host_c
add=42 fib=6765 mean=7.50
```

`karac_runtime_init()` before the first exported call, `karac_runtime_shutdown()` at teardown — the runtime lifecycle bracket. At v1 they are no-ops, but calling them is the contract: it lets the runtime acquire and release whatever it needs without you rewriting the host later.

## The Rust host — with one caveat

A Rust program consumes Kāra the same way it consumes any C library: an `extern "C"` block declaring the surface. This is the pyo3 / cxx / uniffi pattern, inverted — Rust reaching *into* Kāra across the stable C boundary. (There is no stable *Rust* ABI, so C is the durable bridge in both directions.)

```rust
#[repr(C)]
struct Stats { sum: f64, count: i64 }

#[link(name = "kernel", kind = "dylib")]
extern "C" {
    fn karac_runtime_init();
    fn karac_runtime_shutdown();
    fn add(a: i32, b: i32) -> i32;
    fn fib(n: i64) -> i64;
    fn stats_mean(s: Stats) -> f64;
}

fn main() {
    unsafe {
        karac_runtime_init();
        let s = Stats { sum: 30.0, count: 4 };
        println!("add={} fib={} mean={:.2}", add(20, 22), fib(20), stats_mean(s));
        karac_runtime_shutdown();
    }
}
```

```text
$ karac build kernel.kara --crate-type cdylib -o libkernel.so
$ rustc host.rs -L . -C link-arg=-Wl,-rpath,. -o host_rs
$ ./host_rs
add=42 fib=6765 mean=7.50
```

Same three numbers as the C host. That is the A/B result: one kernel, two languages, identical output.

The caveat is in the build command: a Rust host must link the **cdylib**, not the staticlib. The Kāra runtime is itself a Rust crate that bundles `std`, so a `.a` carries std symbols — `rust_eh_personality`, the allocator shims — that collide with the Rust host's own `std` at static-link time, and you get a cryptic `duplicate symbol` error. A shared library encapsulates those internal symbols; the dynamic linker resolves only the exported entry points. `karac` prints a note steering you to the cdylib whenever you build a staticlib, and the caveat rides along in the header comment too. C and C++ hosts have no `std` to clash with and can link either artifact.

## What crosses the boundary

The C ABI is honest about what it can carry. The type mapping is a deliberate v1 set:

- **Primitives** (`i32`, `i64`, `f64`, `bool`, …) and **raw pointers** cross transparently — they *are* their C equivalents.
- **`#[repr(C)]` structs** cross by value, as you saw with `Stats`.
- **Owned collections returned by value** — `String`, `Vec[i32]`, and one level of nesting like `Vec[String]` — are **auto-boxed**. Kāra returns them as a small `{data, len, cap}` record, which doesn't match the C struct-return ABI, so the compiler heap-boxes the value and hands C an opaque pointer instead. The header gains a matching struct and a `karac_free_<name>` destructor; the C side reads the fields and calls the destructor when done. Zero boilerplate on your side.
- **Everything else** — an `enum`, an `Option`, a plain (non-`repr(C)`) struct by value — is **rejected at build time** with a clear error rather than silently miscompiled. If the offender is a user struct, the diagnostic points at the one-step fix: add `#[repr(C)]`.

That last rule is the important one. The compiler will not emit a header that promises a shape the ABI can't actually deliver, so the `.a` / `.so` / `.h` you ship is never quietly wrong.

## Ownership across the boundary

The simplest way to hand C a buffer it will own is to allocate it the way C expects — through `malloc`, imported as an `unsafe extern "C"` block — fill it with raw-pointer writes, and return the pointer. The caller frees it with `free` (or a Kāra export that calls `free`), and no Kāra destructor is ever involved:

```kara,ignore
unsafe extern "C" { fn malloc(n: usize) -> *mut i64; }

pub extern "C" fn make_squares(n: i64) -> *mut i64 with blocks {
    let p: *mut i64 = unsafe { malloc((n as usize) * 8) };
    let mut i: i64 = 0;
    while i < n {
        unsafe { p.offset(i).write(i * i); }
        i = i + 1;
    }
    p
}
```

The raw-pointer methods — `.offset(i)`, `.write(v)`, `.read()` (and `_unaligned` / `_volatile` variants) — are the low-level toolkit for this, always inside `unsafe`. The `with blocks` on the signature is the [effect system](./ch11-effects.md) at work: calling a foreign function is treated as blocking, and a *public* function must declare the effects it carries — the compiler tells you exactly which to add if you forget.

When instead you're holding an *owned Kāra value* and want to release it to the caller without running its destructor, the [`forget`](./ch12-ownership.md) primitive is the move-out. It consumes its argument and suppresses the drop Kāra would otherwise run:

```kara
forget(value);   // Kāra will NOT drop `value`; ownership has left the language
```

Because `forget` takes its argument by value, the ownership checker and the drop machinery both agree the value left — there is no double-free to reason about. For the auto-boxed return types above, this whole handshake is generated for you; the manual tools here are the escape hatch when you're managing the memory yourself.

## Effects at the boundary

An exported function's [effects](./ch11-effects.md) are part of its contract, and the header states them. The boundary is synchronous: a `suspends` function — one that would yield to the async scheduler — is **rejected as an export** (`E0414`), because there is no scheduler on a bare foreign thread to yield to. `blocks` is fine; `panics` is contained (a panic can't unwind across the C frame, so it aborts rather than corrupt the caller). The exported surface tells the truth about what it does, up front.

## What's next

You've now seen both directions of the C boundary: this chapter produced a library for C and Rust to consume, and the [effect](./ch11-effects.md) and [ownership](./ch12-ownership.md) chapters cover calling *out* to C from Kāra with `unsafe extern` blocks. The full worked example — kernel, C host, Rust host, and a README — lives in `examples/interop/` in the source tree; the specification is `design.md § Exported C ABI`.

The pitch is simple: you don't have to adopt Kāra all at once. Start with one kernel, link it in, and let it earn the next one.
