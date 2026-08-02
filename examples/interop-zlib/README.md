# Consume-direction FFI — binding a real C library (zlib)

The sibling [`examples/interop/`](../interop/) is the **produce** direction: a
Kāra kernel built as a library and linked into a C or Rust host. This example
is the other half — the **consume** direction: Kāra as the host program,
calling into the system's zlib through the shipped FFI surface. Together they
are the two legs of the additive-adoption story.

Spec: [`docs/design.md` § FFI](../../docs/design.md#ffi) (the
`unsafe extern "C" { }` block, effect defaults, trust-not-verify) and
§ "Foreign-library linking — the `[link]` table".

## What the binding looks like

[`src/main.kara`](src/main.kara) declares four zlib entry points plus libc
`malloc`/`free` — the whole binding is ~30 lines of declarations:

- **`[link] libs = ["z"]`** in [`kara.toml`](kara.toml) puts `-lz` on the link
  line. The extern block says *what symbols* the program imports
  (source-level, soundness-bearing); the manifest says *which library*
  satisfies them (build-time, environment-resolved).
- **Effect annotations are the Kāra-specific part of a binding.** Foreign
  declarations can't be effect-inferred (no body), so the programmer declares
  the effects and the compiler trusts them at the boundary:
  - `#[noblock]` on the pure-CPU entry points (`crc32`, `compressBound`,
    `compress2`, `uncompress`) removes the safe `{blocks}` default, so calls
    never pessimize scheduler placement.
  - `allocates(Heap)` on `compress2`/`uncompress` records that zlib's default
    allocator mallocs internally — which matters under profiles where heap
    allocation is forbidden, and the `karac` FFI lint suggests it for
    known-allocating symbols.
  - `malloc`/`free` keep the `{blocks}` default — the "when in doubt,
    over-declare" rule: a too-liberal effect set costs a little scheduling
    freedom; a too-narrow one is unsound.
- **zlib's in-out length parameter** (`uLongf *destLen`) maps to
  `ptr.mut(local)` — take a raw pointer to a Kāra local, let C write through
  it, read the local afterward.

## Build and run

```
cd examples/interop-zlib
karac build          # → ./zpipe (links -lz via the manifest)
./zpipe
```

Expected output (byte-stable across zlib versions — CRC32 is specified
byte-for-byte; the *compressed size* is not, which is why the program prints
"smaller than original" rather than a number):

```
original: 1440 bytes
compressed: smaller than original
round-trip length: OK
crc32 in:  1141850752
crc32 out: 1141850752
round-trip: OK
```

Verified surfaces (all byte-identical to a C reference implementation of the
same pipeline): the default (auto-parallelizing) build, `KARAC_AUTO_PAR=0`,
and `karac run` (JIT lane).

## `karac run` and FFI

`karac run`'s JIT lane resolves foreign symbols from the JIT-runner process
itself, not from the manifest's `[link]` libraries. This example happens to
work under `karac run` because the runner embeds LLVM, and LLVM links libz —
an incidental fact about *this* library, not a property of FFI programs in
general (a libpng or sqlite3 binding would fail symbol resolution under the
JIT). The tree-walk interpreter (`karac run --interp`) refuses raw-pointer
FFI outright — it has no pointer representation — and directs you to
`karac build`. Treat FFI programs as an AOT surface: `karac build` is the
lane where the `[link]` table actually governs linking.

## Requirements

- zlib development files (`zlib1g-dev` on Debian/Ubuntu, `zlib-devel` on
  Fedora; preinstalled on macOS). A missing library surfaces as an ordinary
  linker error at build time — the manifest is not validated against the
  filesystem.
