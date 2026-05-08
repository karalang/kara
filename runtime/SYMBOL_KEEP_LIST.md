# Runtime symbol keep-list

Pre-flight audit of every load-bearing symbol declared in `runtime/src/`,
produced for the Phase 1 binary-size optimization slice (`strip -x` post-link
+ `panic = "abort"`). Re-run this audit before enabling Phase 2 cross-archive
LTO + `-Wl,-dead_strip` / `-Wl,--gc-sections` — DCE without an explicit
keep-list will silently strip symbols the codegen-emitted Kāra programs
indirect into via the `karac_*` C ABI.

The audit covers the attribute set: `#[used]`, `#[link_section(…)]`,
`#[ctor]`, `#[dtor]`, `#[no_mangle]`, and `extern "C"` declarations.

## Audit method

```sh
rg '#\[used\]|#\[link_section|#\[ctor\]|#\[dtor\]|#\[no_mangle\]|extern "C"' runtime/src/
```

Re-run on every runtime change; this file should grow only when a new
`#[no_mangle]` or `extern "C"` declaration lands in `runtime/src/`.

## Findings

### Attributes NOT present in the runtime

These attribute kinds were searched and produced **zero** matches as of
2026-05-07:

- `#[used]`
- `#[link_section(…)]`
- `#[ctor]`
- `#[dtor]`

Implication for Phase 2 LTO/DCE: no static-init / static-fini / forced-keep
machinery exists in the runtime today. A future `#[ctor]` or `#[link_section]`
addition (e.g., a panic-handler section, a static registration table) would
need an explicit keep-list entry here *before* it lands, paired with whatever
linker flag preserves the section across DCE on each target.

### `#[no_mangle] extern "C"` exports — runtime entry points

Every symbol below is called by codegen-emitted LLVM IR through its `cc`-style
ABI signature. Stripping any of these from the static archive (`#[used]` is
unnecessary since `#[no_mangle]` + a referencing call site already pins the
symbol) — or DCE-stripping them from the linked executable — would cause
runtime link failures or undefined behavior.

`strip -x` (Phase 1) keeps **all** of these intact: `-x` strips local
non-global symbols only; `#[no_mangle] extern "C"` exports are emitted as
external-linkage globals (Mach-O `EXT`, ELF `STB_GLOBAL`) and are
preserved by definition.

LTO + `-Wl,-dead_strip` (Phase 2) needs explicit per-symbol awareness. The
keep-list pattern is each symbol below; the Phase 2 PR's checklist must
verify none gets stripped.

#### `runtime/src/lib.rs`

| Symbol | Signature (C ABI) | Purpose |
|---|---|---|
| `karac_par_run` | `unsafe extern "C" fn(branches: *const KaracBranch, count: usize)` | `par {}` block executor — fixed-size thread pool, fail-fast cancellation. |
| `karac_error_trace_push` | `unsafe extern "C" fn(file_ptr: *const u8, file_len: usize, line: u32, col: u32)` | Push a frame onto the global `?` error-return trace at every `?` failure site. |
| `karac_error_trace_clear` | `extern "C" fn()` | Reset the global `?` error-return trace at every `?` success site. |

Plus one `extern "C"` block importing libc:

| Imported symbol | Signature | Purpose |
|---|---|---|
| `atexit` (POSIX libc) | `extern "C" fn(callback: extern "C" fn()) -> i32` | Registers `print_trace_at_exit` to flush the `?` trace on normal program exit. Lazy/idempotent — programs that never push a trace frame skip the registration. |

And one private `extern "C"` callback registered with `atexit`:

| Symbol | Signature | Purpose |
|---|---|---|
| `print_trace_at_exit` | `extern "C" fn()` | Module-private. The `extern "C"` ABI is the one `atexit(3)` requires; the symbol is not a public runtime export. Reachable only after `register_trace_atexit_once` arms it. |

#### `runtime/src/clone.rs`

| Symbol | Signature (C ABI) | Purpose |
|---|---|---|
| `karac_string_clone` | `unsafe extern "C" fn(src: *const c_void, dst: *mut c_void)` | Deep-copy a Kāra `String { data, len, cap }` value, with the static-literal `cap == 0` special case that forces the clone onto the heap. |

#### `runtime/src/map.rs`

The type-erased open-addressing hash map's full C ABI surface. Emitted by
codegen for every `Map[K, V]` operation; concrete `hash_fn` / `eq_fn` are
codegen-monomorphized and passed in at construction.

| Symbol | Purpose |
|---|---|
| `karac_map_new` | Construct a fresh map. |
| `karac_map_free` | Destroy a map and free its storage. |
| `karac_map_insert` | Insert without surfacing the displaced value. |
| `karac_map_insert_old` | Insert + surface the displaced value (`Map.insert → Option[V]`). |
| `karac_map_get` | Read a value by key. |
| `karac_map_remove` | Remove by key, return presence boolean. |
| `karac_map_remove_old` | Remove by key, surface the removed value (`Map.remove → Option[V]`). |
| `karac_map_contains` | Presence check. |
| `karac_map_entry` | Probe-and-insert-on-vacant for `entry(k).or_insert*` chains. |
| `karac_map_lookup_slot` | Read-only slot lookup for `entry(k).and_modify` chains. |
| `karac_map_len` | Live-entry count. |
| `karac_map_clear` | Drop every entry; keep the bucket allocation. |
| `karac_map_iter_new` | Construct a forward iterator. |
| `karac_map_iter_next` | Advance the iterator; copies key+val out. |
| `karac_map_iter_free` | Destroy the iterator. |

(All declared `pub unsafe extern "C"` with `#[no_mangle]`.)

### Other `extern "C" fn` types in the surface

These appear as **type signatures** rather than declared symbols — they are
function-pointer types passed in by the codegen at runtime, so the runtime
itself never declares the underlying symbol; the compiler emits the
monomorphized concrete function and hands a pointer through the C ABI.

- `KaracBranch::func` — `unsafe extern "C" fn(*mut c_void, *const AtomicBool)`. Each `par {}` branch's body, emitted per-call-site by codegen.
- `KaracMap` `hash_fn` — `unsafe extern "C" fn(*const c_void) -> u64`. Per-K-type hash function emitted by codegen.
- `KaracMap` `eq_fn` — `unsafe extern "C" fn(*const c_void, *const c_void) -> bool`. Per-K-type equality function emitted by codegen.

Phase 2 LTO/DCE relevance: these are *compiler*-emitted symbols — the keep-list
guarantee for them lives in `src/codegen.rs`, not in this runtime audit.
Today they are reachable through indirect calls only, so cross-archive LTO
cannot prove them dead via local visibility analysis. If a future Phase 2
configuration starts running whole-program devirtualization, these may need
an explicit keep-list at the codegen end.

## Summary

- **Total `#[no_mangle]` exports:** 19 (`karac_par_run`, `karac_error_trace_push`, `karac_error_trace_clear`, `karac_string_clone`, plus 15 `karac_map_*`).
- **Total libc `extern "C"` imports:** 1 (`atexit`).
- **Total private `extern "C"` callbacks:** 1 (`print_trace_at_exit`, registered with `atexit`).
- **`#[used]` / `#[link_section(…)]` / `#[ctor]` / `#[dtor]`:** none.

Phase 1 (`strip -x` + `panic = "abort"`) is safe against this surface: `strip
-x` preserves all 19 global exports by construction, and `panic = "abort"`
removes the unwind tables that the runtime never depends on at runtime
(the lone `catch_unwind` site in `karac_par_run` becomes effectively
dead-code under abort semantics — fail-fast cancellation still works because
the abort happens before the `cancel.store` would have run, which matches
"some other branch exited the process" anyway).

Phase 2 (LTO + `-Wl,-dead_strip` / `-Wl,--gc-sections`) requires this same
list as a per-symbol keep-list directive, plus a re-run of this audit before
the slice lands.
