# Spike: Additive interop — Kāra as a component you add, not a rewrite you commit to

**Status:** ✅ **ALL SLICES COMPLETE (2026-07-08).** Shipped: framing + export-ABI spec + native library build mode (`.a`/`.so`/`.dylib`) + C-header emitter + producer-side effect policing + a C-and-Rust proof-point demo + the full Slice-4 ownership handoff (`forget` move-out primitive, Path-A raw-pointer methods, Path-B compiler auto-boxing + auto-destructors). Both consume and produce directions are real and verified end-to-end under ASAN/LeakSanitizer. The producer direction is v1 (owner decision 2026-07-08). Remaining polish (Windows library artifacts, 5-target CI matrix, project-mode `[lib]` table, Rust-host `std`-collision smoothing, book chapter, non-scalar/enum boxed returns) is tracked in roadmap Tier 5 as v1 hardening / follow-ons — no core capability is missing.
**Decision date:** 2026-07-06. **Owner call:** worth doing, but scope honestly — the mechanism mostly exists, one claim in the pitch is physically un-cashable, and the genuine gap is the producer direction + a proof-point.

**Progress (2026-07-08):**
- **Slice 0 (framing) — DONE.** The two corrections (consume=done/produce=gap; Rust-via-C-shim) are recorded below and are the do-not-rescope reference the Phase-8.5 entries cite.
- **Slice 1 (export-ABI spec) — DONE.** [`design.md § Exported C ABI`](../design.md#exported-c-abi): export surface = `pub extern "C" fn` (language-driven discovery, mirroring WASM; `#[unsafe(no_mangle)]` is the forward-compat idiom, not required today since Kāra doesn't yet mangle); type mapping = primitives + `#[repr(C)]` structs transparent, everything else a Kāra-owned opaque handle; effect contract = producer-side effects are KNOWN; runtime-init contract + not-self-contained caveat.
- **Slice 2 (native library build mode) — DONE.** `karac build --crate-type staticlib` (→ thick `.a`, runtime bundled via `ar -M`/`libtool`) and `--cdylib` (→ `.so`/`.dylib`, `cc -shared`/`-dynamiclib`, runtime lifecycle forced in via `-u`, macOS `@rpath` install-name). `-o`/`--out` output path; default `lib<stem>.<ext>` so a lib build never clobbers a stray exe. `src/codegen/driver.rs::link_native_library`; CLI in `src/cli/args.rs` + `cmd_build`. Wasm × crate-type rejected. Runtime lifecycle no-ops added (`runtime/src/lifecycle.rs`).
- **Slice 3 (C-header emitter) — DONE.** `src/cheader.rs` (plain data, non-llvm): include guard + `<stdint.h>`/`<stddef.h>` + `extern "C"` guard + `karac_runtime_init/shutdown` + `#[repr(C)]` struct defs (dependency-ordered) + one `@effects`-annotated prototype per export + `KaraHandle` opaque typedef when needed. 6 unit tests.
- **Slice 3½ (producer effect contract) — DONE.** `suspends` exports rejected (E0414, `verify_extern_export_no_suspends` — sibling of the existing C-unwind rule; fatal for library builds); `panics` auto-abort + `extern "C-unwind"` rejection were already in codegen. 2 effect tests.
- **Slice 5 (proof-point) — DONE.** [`examples/interop/`](../../examples/interop/): the same kernel linked into a C host (staticlib) *and* a Rust host (cdylib), with a working recipe. E2E test `test_build_crate_type_staticlib_links_from_c_e2e` in `tests/cli.rs` (C links the `.a` with no karac toolchain present). **Finding:** a Rust host must link the *cdylib*, not the staticlib — the runtime bundles `std`, so a `.a` collides on `rust_eh_personality` etc.; the `.so` encapsulates them. Documented in the spec + example.
- **Slice 4 (`forget` / ownership handoff) — PRIMITIVE SHIPPED; round-trip blocked on two named foundations.** `forget[T](value)` (the move-out primitive) landed and is verified sound (owned param → ownership checker + drop oracle both consume → codegen suppression matches; 0 `drop_differential` divergences; observable drop-count tests in interpreter + codegen; use-after-forget is a move error). Co-designed with the ownership-mechanization spike's (drafted) slice-2 drop model. The full allocate→use→free **round-trip** is blocked on two foundations, found while building it: **(a)** the manual raw-pointer path needs `.offset`/`.read`/`.write` pointer methods that are spec'd-but-unimplemented in codegen (`B-2026-07-08-4`); **(b)** the auto-boxing/auto-destructor sugar needs invasive return-ABI surgery + a per-type drop-glue synthesizer whose soundness gate is the CI-scale ASAN/LSan differential fuzzer corpus, not a single pass. The primitive + convention are done; the round-trip ergonomics wait on (a) or (b)'s gate.
- **Graduating criterion — MET + advanced.** Slices 2/3/3½/5 are `[x]` in [`roadmap.md` Phase 8.5 Track 2 → Tier 5](../roadmap.md#phase-85-v1-ship-readiness); Slice 4 stays `[ ]`.

---

## Decision & rationale

The pitch: *"Be additive, not a replacement. No one leaves Rust for an empty ecosystem. If Kāra can call C and Rust crates cleanly, it becomes a language you add to a system (write the parallel data kernel in Kāra, keep everything else) rather than a bet-the-codebase rewrite. That's the only realistic adoption path."*

The strategic claim is right — it is the Rust-in-Firefox / Zig-alongside-C playbook, the correct instinct for a language with no ecosystem yet. But this item is **not a greenfield epic** like the LLJIT and ownership spikes. Investigating the FFI surface turned up a surprise that reshapes the work.

**The consume side — Kāra calling foreign code — already ships and is load-bearing.**

- **Language surface:** `extern "C" fn`, `unsafe extern "ABI" { ... }` blocks, opaque handle types (`ExternItem::OpaqueType`), FFI `union` (`#[repr(C)]` + `Copy` fields + unsafe read, `design.md § FFI Unions`), and a second foreign form `host fn` for wasm/host imports. Roadmap marks both the surface (L149 `[x]`) and codegen (L379 `[x]`) done.
- **Calling conventions:** `extern "C"` / `"C-unwind"` / `"interrupt"` implemented; `stdcall`/`fastcall`/`win64`/`sysv64` reserved (roadmap L966).
- **Effect integration — the part no one else has:** `src/effectchecker/extern_ffi.rs` seeds ABI-keyed default effects (`"C"` → `{blocks}`, `"C-unwind"` → `{blocks, panics}`), honors `@noblock`, lints extern names that suggest an omitted `allocates(Heap)`, enforces the `no_alloc` profile across the boundary, and gates `extern "C-unwind"` *exports* on a `panics` body (`ExternCUnwindRequiresPanics`).
- **Foreign-library linking already works:** `[link] libs = [...]` / `search-paths = [...]` in the manifest (`src/manifest.rs:432`) lowers to `-l`/`-L` on the link line — "general-purpose foreign-library linking," born to link `libLLVM-18`.

And it is not theoretical — three v1-critical paths depend on it *today*: self-hosting calls the LLVM-C API through this FFI (roadmap L1045, the `inkwell`-analogue), `std.tls` vendors rustls via an FFI binding layer, `std.crypto` delegates to a vetted C library. **Interop is on the v1 critical path internally, whether or not adoption ever needs it.**

So the proposal's premise ("*if* Kāra can call C") is largely already true. That collapses this item's real content down to two things: an honest scoping correction, and one genuinely-missing capability.

### Correction 1 — "call Rust crates cleanly" is physically un-cashable as written

Rust has **no stable ABI**. *Nothing* calls arbitrary Rust crates cleanly — not even Rust across a dylib / compiler-version boundary. The only durable bridge is the C ABI: a crate exposes `#[no_mangle] pub extern "C" fn` + `#[repr(C)]` types, and *then* Kāra calls it exactly like C (already done). So the achievable promise is **"call C, and call Rust crates that are wrapped to expose a C ABI"** — the pyo3 / cxx / uniffi pattern. Real and valuable, but it is "add a C-ABI shim crate," not "add it to `Kara.toml` and call it." Stating this plainly keeps the README from writing a check the ABI can't cash.

### Correction 2 — the adoption thesis points at the *producer* direction, which is the gap

"Kāra can call C and Rust crates" = Kāra as **consumer** (done). But *"write the parallel data kernel in Kāra, keep everything else"* = an existing C/Rust/Python system calling **into** Kāra = Kāra as **producer / embeddable library**. That direction is the real hole:

- `--crate-type staticlib/cdylib` in the tree only builds the *runtime* Rust crate (`karac-runtime`), **not** a user Kāra program as a linkable `.a`/`.dylib` with a stable C surface.
- `--export=` is **WASM-only** (`src/codegen/driver.rs:33`); native has no export-surface concept beyond `pub` giving external linkage.
- There is **no C-header emitter** (no cbindgen analogue) for a `pub extern "C" fn` surface.

The capability the adoption story actually needs — *hand a C/Rust/Python team a `.a` + a `.h` and let them link your Kāra kernel* — does not exist. That is this spike's target.

### Relationship to the other two hardening spikes

Different category. LLJIT and ownership each **eliminate a bug class** (run-vs-build tax; drop-soundness). This is an **adoption-track** item — no bug class, a go-to-market capability. Mechanically it is *further along* than either; strategically it is *unproven* (no demo exists). Independent of both — it touches the build driver, the manifest, and a new emitter, not the interpreter or drop-insertion.

| Spike | Category | State |
|---|---|---|
| LLJIT productionization | eliminate the run-vs-build tax | active epic |
| Ownership mechanization | eliminate drop-soundness | proposed epic |
| **Additive interop (this)** | adoption capability (producer direction) | consume-side ships; producer-side is the gap |

---

## Current state — what already exists to build on

- **Consume side, in full (do NOT rescope as greenfield):** `extern "C" fn` + `unsafe extern` blocks + opaque types + FFI unions + calling conventions + effect integration + `[link]` manifest linking. Roadmap L149 / L379 `[x]`. This is the baseline every producer-side tracker entry must cite so nobody files "build C/Rust interop" as new work.
- **Export-boundary groundwork:** `verify_extern_export_panics` / `ExternCUnwindRequiresPanics` already police one aspect of *exported* `extern` fns — the effect contract at the boundary is partly specified, not blank.
- **`forget` (unsafe) is reserved but unbuilt** (roadmap L516) — "suppress destructor; reserved for FFI handoff." This is exactly the primitive the producer direction needs for *ownership handoff across the boundary* (Kāra allocates, C frees), and it collides with the ownership-mechanization axis — see Gotchas.
- **WASM already does producer-side export discovery** (`crate::wasm_exports::collect_wasm_exports` → `--export=`). The native producer path is the missing peer; the WASM one is a working design reference for "which symbols are the public surface."
- **The manifest is the natural home for an export list** — it already carries `[link]`; a `[lib]` / `[export]` table is the symmetric addition.

There is **no** native library-artifact build mode, **no** C-header emitter, and **no** embed-into-a-foreign-system example anywhere in `examples/` or `kara-katas`.

---

## Ordered slices (design forks first — the shape is unsettled, so this is a spike, not a checklist)

**Slice 0 — write the framing down (this doc's core; settle the two corrections). ✅ DONE (2026-07-08).**
Land the "consume = done, produce = gap, Rust-via-C-shim" framing as the shared understanding so no tracker entry gets filed as "build C/Rust interop" greenfield. Cite the L149/L379 `[x]` baseline. *Output: the corrections above become the reference the Phase-8.5 entries point at.* Zero code. *Landed: the corrections below are cited verbatim by the roadmap Tier-5 entries.*

**Slice 1 — decide the export surface + type-mapping (design fork, no code). ✅ DONE (2026-07-08) — [`design.md § Exported C ABI`](../design.md#exported-c-abi).**
Answer the open questions *before* building:
- **What is the public surface?** Every `pub extern "C" fn`? A manifest `[lib] exports = [...]`? A `#[export]` attribute? (WASM's `collect_wasm_exports` is the reference for how a surface is discovered.)
- **How do Kāra types cross a C header?** The honest v1 answer is likely primitives + `#[repr(C)]` structs + opaque handles only — `Vec`/`String`/`enum`/`Option` map to opaque pointers with accessor functions, *not* a transparent layout. Decide the allowed set and the boxing convention.
- **The effect contract for an effect-blind caller.** A C caller has no effect system; `verify_extern_export_panics` already handles the `panics` case — extend the rule to the full exported surface.

*Output: a written export-ABI spec (`design.md § Exported C ABI`).*

**Slice 2 — native library-artifact build mode (the core capability). ✅ DONE (2026-07-08).**
`karac build --crate-type staticlib` (→ `.a`) and `--cdylib` (→ `.so`/`.dylib`), routing the Slice-1 exported surface through the native link path with external linkage. Reuses `driver.rs` runtime-archive location logic. *Landed: `link_native_library` (thick archive via `ar -M`/`libtool`; shared lib via `cc -shared`/`-dynamiclib` with `@rpath` install-name + forced-in lifecycle symbols). CLI `--crate-type` / `-o`. Wasm × crate-type rejected.*

**Slice 3 — C-header emitter (the "clean" in "cleanly"). ✅ DONE (2026-07-08) — `src/cheader.rs`.**
Emits a `.h` for the exported surface (the cbindgen analogue) so a foreign caller `#include`s it. Scoped to the Slice-1 type-mapping. *Landed: plain-data emitter (non-llvm), guard + includes + `extern "C"` wrapper + lifecycle protos + dependency-ordered `#[repr(C)]` structs + `@effects`-annotated prototypes + `KaraHandle` opaque typedef.*

**Slice 4 — ownership handoff across the boundary (the soundness fork). ✅ COMPLETE (2026-07-08) — `forget` + Path A + Path B all shipped.**
*The full slice is done. **`forget`** (the move-out primitive) + **Path A** (raw-pointer instance methods `.offset`/`.read`/`.write`, `B-2026-07-08-4` closed) give the sound manual allocate→use→free round-trip. **Path B** (compiler auto-boxing + auto-destructors) makes it zero-boilerplate: a `pub extern "C" fn` returning `Vec[scalar]`/`String` — and, via the follow-on, `Vec[String]`/`Vec[Vec[scalar]]` (one level of nesting, nested transparent structs + a recursive destructor) — auto-boxes into an opaque pointer and auto-emits `karac_free_<name>`; the C side reads the `{data,len,cap}` fields transparently and frees via the destructor. A `validate_exports` ABI-honesty gate rejects any non-transparent, non-boxable export return/param (`enum`, `Option`, `Vec` by value, deeper nesting) rather than shipping a dishonest `KaraHandle`. All verified end-to-end under ASAN/LeakSanitizer (no leak, no use-after-free); drop_differential 0 divergences; memory_sanitizer 558 pass. Deeper nesting / `enum` / user-struct returns cross via a raw pointer to a Kāra-owned box (the manual Path-A pattern) — a further follow-on.*

Build `forget` (roadmap L516) and specify who frees what: Kāra allocates a `Vec`/`String` and hands it to C — either C calls a Kāra-provided `karac_free_*`, or the value crosses as an opaque handle carrying a destructor export. **This is where this spike touches the ownership-mechanization spike** — the export boundary is a *move out of the Kāra ownership universe*, and that spike's model (its slice 2, now drafted) states the transition. *Landed: `forget[T](value)`, the move-out primitive — sound by construction (owned param → ownership checker + drop oracle consume → codegen suppression; 0 drop-oracle divergences), verified with observable drop-count tests + a use-after-forget move error.* The full **round-trip** (allocate→use→free) remains blocked on two foundations found while building it: **(a)** raw-pointer instance methods `.offset`/`.read`/`.write` are spec'd-but-unimplemented in codegen (`B-2026-07-08-4`, the lower-risk manual path); **(b)** the auto-boxing/auto-destructor sugar needs invasive return-ABI surgery + a per-type drop-glue synthesizer gated on the CI-scale ASAN/LSan differential corpus. The primitive + convention are done; the ergonomic round-trip waits on (a) or (b).

**Slice 5 — the proof-point (the actual adoption story). ✅ DONE (2026-07-08) — [`examples/interop/`](../../examples/interop/).**
A hot kernel written in Kāra, built as a `.a` + `.h`, linked into an existing C *and* Rust program that keeps everything else. *Landed: the C host links the staticlib with no karac toolchain present (E2E test `test_build_crate_type_staticlib_links_from_c_e2e`); the Rust host links the cdylib. Finding: a Rust host must use the cdylib (the staticlib bundles `std` and collides on `rust_eh_personality`).* Book-snippet A/B verification ([[book-snippets-ab-verify-like-katas]]) is a follow-up.

---

## Gotchas — do not rediscover these

- **"Call Rust crates" has no clean form — always route through C.** Any slice that promises native Rust-crate consumption is promising something the Rust ABI cannot deliver; the deliverable is the C-shim pattern + docs, not a `Kara.toml` Rust dependency. (Correction 1.)
- **`forget` / handoff collides with the ownership-mechanization spike.** The export boundary is a move *out* of Kāra's ownership universe; if handoff is specified independently of that spike's slice-2 model, the two specs diverge — the exact unspecified-invariant failure the ownership spike exists to kill. Co-design Slice 4 with it. ([[ownership-model-mechanization-spike]])
- **A produced library is NOT self-contained by default.** It still depends on `libkarac_runtime.a` symbols (alloc/free, RC, channels); the artifact links only if the runtime is bundled or its symbols are re-exported. Reuse `driver.rs`'s runtime-location logic and verify the consumer links with **no karac toolchain present** ([[runtime-archive-rebuild-dance]]).
- **`karac build` writes its binary to CWD** — a library-artifact build must not clobber a stray executable in the working dir; pick an explicit output path ([[generic-struct-element-monomorphization]]).
- **Producer-side effects are KNOWN, not trust-not-verify.** An *exported* fn's effects were checked against its body, so the header/contract can state them precisely — do **not** copy the extern-*import* default (`{blocks}`) onto exports.
- **A produced `.dylib` on macOS carries install-name / rpath baggage.** The WASM export path sidesteps this; the native path must set `-install_name`/rpath so the consumer loads it. Untriaged — verify on a real link before claiming Slice 2 done.
- **Stale installed `karac` can mask a producer-mode change** — black-box `karac build --crate-type ...` may hit a stale `~/.local/bin/karac`; reinstall from `target/release` + md5-compare first ([[stale-installed-karac-cli-repro-trap]]).

## Acceptance criteria

Slice 0–1: the framing + a written export-ABI spec (`design.md § Exported C ABI`) with the type-mapping and effect-contract decided. Slice 2–3: `karac build --crate-type staticlib/cdylib` produces a linkable artifact + an emitted `.h`; a C program links and calls it with no karac toolchain present. Slice 4: `forget` + a stated ownership-handoff rule, co-designed with the ownership spike. Slice 5: one A/B-verified demo — a Kāra kernel embedded in both a C and a Rust host. **Graduating criterion (this spike's defining feature):** Slices 2–5 land as `[ ]` entries in the **Phase 8.5** tracker (packaging / build-tooling), with a couple possibly in **Phase 10** (targets); the consume side stays `[x]` and is cited as the do-not-rescope baseline.

## Open question (owner sign-off)

**RESOLVED 2026-07-08 (owner).** (1) **Producer direction is v1** — it is the flagship adoption pitch ("add Kāra to your system"), the capability already exists and is proven, and the remaining polish is bounded. **v1 scope line, to keep the release-blocking set achievable and sound:**
- **v1 (release-blocking):** the shipped core (native `.a`/`.so`/`.dylib` build mode, C-header emitter, producer-side effect policing, `forget`, the C-and-Rust proof-point) + the polish that makes it a supported guarantee — Windows library artifacts + 5-target CI matrix, project-mode `[lib]` table, the Rust-host `std`-collision smoothing, and the **low-risk Path-A round-trip** (raw-pointer instance methods `.offset`/`.read`/`.write`, `B-2026-07-08-4`), which completes allocate→use→free soundly with no per-type drop synthesis.
- **v1.x (additive, non-blocking):** the **auto-boxing / auto-destructor sugar** (Path B). Its soundness gate is the CI-scale ASAN/LSan `drop_differential` corpus, so it must NOT gate the v1 release; it lands additively once that gate is stood up. Book chapter + book-snippet A/B verification also v1.x.

(2) **Sequencing vs ownership mechanization** — Slice 4's `forget` shipped co-designed with that spike's (now-drafted) slice-2 drop model; the remaining round-trip work inherits the same co-design. Resolved.
