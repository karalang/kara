# FFI and Interop

A new language has a bootstrapping problem: no ecosystem yet, and no team wants to rewrite a working system to get one feature. Kāra's answer is to be **additive, not a replacement**. It can call the C libraries a program already depends on, and a later release lets it be the guest: a kernel written in Kāra, built as a linkable library with a plain C ABI, dropped into a C or Rust program that keeps everything else.

This chapter covers the direction that ships in 0.9, calling C from Kāra, and then sketches the export direction that follows.

## Calling C from Kāra

To import C functions into Kāra, declare them in an `unsafe extern "C"` block and call them inside `unsafe`:

```kara
/// # Safety
/// `abs` is pure integer math from libc; `strlen` reads only the
/// NUL-terminated bytes behind the pointer, which `c"..."` literals guarantee.
unsafe extern "C" {
    fn abs(x: i32) -> i32;
    fn strlen(s: *const u8) -> usize;
}

fn main() {
    // Safety: abs has no preconditions for any value but i32::MIN.
    let n = unsafe { abs(-42) };
    println(n);                           // 42
    let msg = c"hello from C land";
    // Safety: msg is a static NUL-terminated literal.
    let len = unsafe { strlen(msg.as_ptr()) };
    println(len as i64);                  // 17
}
```

This works identically under `karac run` and `karac build`: the JIT resolves the symbols in-process, and the AOT build links them from libc. In 0.9 an imported function's parameters and result must be integers, `bool` or raw pointers; floats and `#[repr(C)]` structs do not cross an import yet.

Three rules carry most of the weight:

- **The declaration block is `unsafe extern`, and every call site is `unsafe`.** The compiler can't check what's on the other side of the boundary, so both the block and each call carry a `# Safety` / `// Safety:` comment stating the trust contract — the `undocumented_unsafe` lint warns when they're missing.
- **C strings cross as `*const u8`.** A `c"..."` literal is a `ref CStr` (NUL-terminated, in rodata); hand it to C with `.as_ptr()`. Declaring an extern parameter as `ref CStr` directly is not the FFI form — `CStr` is a Kāra type, not a C one.
- **Effects still apply.** An extern fn that writes somewhere declares it: `fn puts(s: *const u8) -> i32 with writes(Console);` — foreign code doesn't get to dodge the effect system (see [Effects](./ch11-effects.md)).

## Kāra as a library (after 0.9)

The export direction answers "can I use this next to my existing code?". Its design is settled ([deferred.md § Exported C ABI](https://github.com/karalang/kara/blob/main/docs/deferred.md#exported-c-abi)), and it is not in 0.9: `karac check` refuses a `pub extern "C" fn`.

In outline:

- **The exported surface is every `pub extern "C" fn`.** `extern "C"` gives it the C calling convention and an unmangled name. A library has no `main`.
- **Two artifacts.** `karac build kernel.kara --crate-type staticlib` gives a thick `.a` that bundles the runtime, and `--crate-type cdylib` gives a `.so` / `.dylib`. Both come with a generated C header, so a caller includes it instead of transcribing signatures. A Rust host links the cdylib, because the runtime's own `std` symbols clash with the host's in a static link.
- **What crosses.** Primitives, raw pointers and `#[repr(C)]` structs by value. Owned `String` and `Vec` results are boxed behind an opaque pointer with a generated `karac_free_<name>` destructor. Anything else (an `enum`, an `Option`, a plain struct) is refused at build time, with the `#[repr(C)]` fix when that is the fix.
- **Effects are part of the contract.** A `suspends` function cannot be exported, since a foreign thread has no scheduler to yield to. `blocks` is fine, and a panic aborts rather than unwind across the C frame.

The worked example (kernel, C host, Rust host) is in `examples/interop/` in the source tree, and runs once the exported C ABI lands.

The pitch stays simple: you don't have to adopt Kāra all at once. Start by calling the C you already have.
