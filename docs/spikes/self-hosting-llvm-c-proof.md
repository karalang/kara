# Minimal proof: Kāra `extern "C"` → LLVM-C → object file → link → run

**Status:** ✅ **RUNS GREEN — `exit=42`** under the stage-0 Rust `karac` (2026-06-11). This is the
spike's [Definition of Done](self-hosting-llvm-c-ffi.md#definition-of-done-this-spike) minimal
proof, and it is now the **seed of the Kāra codegen module** — a real Kāra program that drives
`libLLVM-18` through the FFI binding to build, verify, and emit a working object file. The `.kara`
body, `kara.toml`, and harness below are the **verified-runnable** versions (the earlier draft was
aspirational and never compiled; every gate it surfaced — `[link]`, `CStr.from_ptr`, `#[link_name]`
on externs, the auto-par capture bug, `*mut T` Copy — has since landed). One remaining gap to the
*full* design (`CStr.to_string` for error-path diagnostics) is **not on the success path**, so this
minimal proof sidesteps it; the only other follow-on is the stage-2 cross-check (re-run under the
self-hosted `karac`), which awaits the self-hosted compiler.

The proof builds a trivial module — `i64 main() { ret i64 42 }` — verifies it, emits it to an
object file via the host target machine, links that object into an executable, and runs it.
Success = the linked binary exits `42`. It exercises ~20 of the ~120 llvm-c functions from the
[surface inventory](self-hosting-llvm-c-surface.md): context/module/builder lifecycle, one int
type + fn type, add-function + basic block + position, const-int + ret, verify (return-status),
default triple + target-from-triple + create-target-machine + emit-to-file.

The proof builds a trivial module — `i64 main() { ret i64 42 }` — verifies it, emits it to an
object file via the host target machine, links that object into an executable, and runs it.
Success = the linked binary exits `42`. It exercises ~20 of the ~120 llvm-c functions from the
[surface inventory](self-hosting-llvm-c-surface.md): context/module/builder lifecycle, one int
type + fn type, add-function + basic block + position, const-int + ret, verify (return-status),
default triple + target-from-triple + create-target-machine + emit-to-file, and the disposers.

## Prerequisites gate (all green — proof runs)

A 2026-06-11 build of this program (project mode, `[link]` resolving `libLLVM-18`) enumerated the
real gate — **longer than the original two-item list** — and each item below has since landed.
With all hard gates closed, the proof now compiles, runs, and emits a working object (`exit=42`).
The one remaining unbuilt item (`CStr.to_string`) is off the success path, so the minimal proof
does not need it. The list below is the corrected, build-verified set.

- [x] **Native-library link directive (`kara.toml [link]`)** ✅ LANDED 2026-06-11 — the real blocker,
  now resolved: `[link] libs = ["LLVM-18"]` + `search-paths` append `-L`/`-l` to the native `cc`
  line. The `kara.toml` below works verbatim. ([spike Prerequisites](self-hosting-llvm-c-ffi.md#prerequisites-phase-8-floor) / phase-12 Cluster 2.)
- [x] **`CStr.from_ptr(*const u8) -> ref CStr`** ✅ LANDED 2026-06-11 — the inbound raw-pointer
  C-string constructor used by `read_and_dispose`. Lowers to libc `strlen` + the `{ptr, len}`
  aggregate (same shape a `c"..."` literal lowers to); `unsafe`-gated via the unsafe-fn registry,
  interpreter-rejected. Tested across typechecker/codegen/unsafe-lint.
- [x] **`#[link_name]` honored on `unsafe extern` fn imports** ✅ LANDED 2026-06-11. Kāra rejects
  PascalCase extern fn names (Value-class rule), and the entire LLVM-C API is PascalCase;
  `#[link_name("LLVMContextCreate")]` on a snake_case Kāra fn now redirects the emitted LLVM symbol,
  so the binding can name the C symbol while keeping a legal Kāra identifier. Codegen registers the
  import under the foreign symbol (dedup-reusing any built-in of that name, e.g. `strlen`) and
  translates the Kāra name → symbol at the call site. Verified: `getpid`/`strlen` bind + run; the
  PascalCase `LLVMContextCreate` / `LLVMGetDefaultTargetTriple` / `LLVMModuleCreateWithNameInContext`
  bindings are now **accepted by the front-end** (no parse/symbol error) — the proof gets past symbol
  binding. E2E test in `tests/codegen.rs`.
- [x] **Codegen "Undefined variable" for a local read inside an `unsafe {}` block in an auto-par
  branch** ✅ FIXED 2026-06-11. The `malloc`/`free` reproducer was an **auto-parallelization** bug, not
  an FFI one: the auto-par capture-set collector `refs_in_expr` had no `ExprKind::Unsafe` arm, so a
  local read inside `unsafe { free(m) }` was left out of the par-branch env struct → `Undefined
  variable 'm'`. Fixed by aligning `refs_in_expr` with the concurrency analyzer's `collect_expr_reads`
  (added `Unsafe`/`Try`/`Par`/`Lock`/`Question`/`Pipe`/`NilCoalesce`/`OptionalChain` arms). Regression
  test in `tests/par_codegen.rs`; full codegen + par_codegen + concurrency + closures suites green. A
  general-purpose correctness fix (any auto-par branch reading a local through those forms).
- [x] **`*mut T` raw pointers are now `Copy`** ✅ FIXED 2026-06-11. The proof failed ownership once the
  auto-par bug was cleared: a `*mut` handle passed to a second FFI call reported *"value moved here,
  used again"* (`*const T` was already `Copy`; `*mut T` was move-only). Fixed with a `Type::Pointer
  { .. } => true` arm in `ownership.rs::is_copy_type` (both raw-pointer kinds Copy, Rust parity; no
  `Drop`, so no double-free). **This made the handle chain run end-to-end: `LLVMContextCreate` →
  `LLVMModuleCreateWithNameInContext(name, ctx)` → `LLVMContextDispose(ctx)` builds, links `libLLVM-18`,
  and runs (exit 0)** — the first time Kāra calls real libLLVM through this binding. Regression:
  `tests/ownership.rs::test_raw_mut_pointer_is_copy` (+ `*const` companion).
- [ ] **`CStr.to_string() -> Result[String, Utf8Error]`** *(NEW — build-surfaced).* `read_and_dispose`
  converts the LLVM-owned `char*` to an owned Kāra `String`. The `Utf8Error` type exists
  (`runtime/stdlib/utf8_error.kara`) but `CStr.to_string()` has no typecheck/codegen lowering, and
  there is no runtime UTF-8 validator to reuse (`String.from_utf8` is interpreter-only). This is the
  remaining half of the phase-8 *CString owning type + conversions* item (phase-8:774). Needed for
  the proof to **compile** (the call is on an error path, never executed on the `exit=42` success
  path, but must still typecheck + codegen). → own tracker entry.
- [ ] **Proof-spec rewrite** *(this file).* The `.kara` body uses semicolon-free statements
  (`let x = ...` newline) which do not parse — Kāra needs statement terminators here — and writes
  PascalCase extern names directly. Rewrite to snake_case + `#[link_name]` (once that gate clears)
  and add terminators. Mechanical, but the proof was never actually run, so it is listed explicitly.
- [x] Phase-8 FFI floor: opaque foreign types ✅, raw-ptr params/deref ✅, `ptr.mut` out-params ✅,
  `String.to_cstring` ✅, `c"..."` `CStr` ✅ (all confirmed present in the build attempt).

## Gotcha surfaced while writing this (real finding)

`LLVMInitializeNativeTarget` / `LLVMInitializeNativeAsmPrinter` — what inkwell's
`Target::initialize_native` calls — are **`static inline` functions in `llvm-c/Target.h`, not
exported symbols in `libLLVM`.** They expand to the host-arch concrete initializers. So the Kāra
binding **cannot `extern "C"` them**; it must declare and call the concrete per-arch symbols that
*are* exported — on Apple Silicon (the dev box) the `LLVMInitializeAArch64*` quartet; on x86-64
the `LLVMInitializeX86*` quartet. (Footnote added to the inventory's target section.)

## `kara.toml`

```toml
[link]
libs = ["LLVM-18"]
# search-paths from `llvm-config --libdir` of the SAME prefix the Rust stage uses
# (LLVM_SYS_181_PREFIX) — single source of truth across bootstrap stages (sub-q 2/6).
search-paths = ["/opt/homebrew/opt/llvm@18/lib"]
```

## `proof.kara`

Verified-runnable (2026-06-11). Opaque LLVM-C handles are `*mut u8`; PascalCase C
symbols bind via `#[link_name]`; error paths use libc `exit` (`process.exit` has no
codegen lowering yet). `ptr.null_mut()` makes the `*mut`-typed nulls; `ptr.mut(x)`
forms the `*mut *_` out-param pointers.

```kara
// Minimal LLVM-C codegen proof — the seed of the self-hosted codegen module.
// Builds `i64 main() { ret i64 42 }` via the LLVM-C API, verifies it, and
// emits it to `answer.o`. The harness links answer.o and runs it: the linked
// binary exits 42. (LLVM-C FFI spike — Definition of Done.)
//
// Opaque LLVM-C handles are modeled as `*mut u8` (every `LLVMXRef` is an
// opaque pointer at the ABI). PascalCase C symbols bind to snake_case Kāra
// fns via `#[link_name]`. The success path never reads an LLVM-owned
// `char*`, so it needs no `CStr.to_string` — errors print a static message.

unsafe extern "C" {
    #[link_name("LLVMContextCreate")]
    fn context_create() -> *mut u8;

    #[link_name("LLVMModuleCreateWithNameInContext")]
    fn module_create(name: *const u8, c: *mut u8) -> *mut u8;

    #[link_name("LLVMCreateBuilderInContext")]
    fn builder_create(c: *mut u8) -> *mut u8;

    #[link_name("LLVMInt64TypeInContext")]
    fn int64_type(c: *mut u8) -> *mut u8;

    #[link_name("LLVMFunctionType")]
    fn function_type(ret: *mut u8, params: *mut *mut u8, count: u32, is_vararg: i32) -> *mut u8;

    #[link_name("LLVMAddFunction")]
    fn add_function(m: *mut u8, name: *const u8, fnty: *mut u8) -> *mut u8;

    #[link_name("LLVMAppendBasicBlockInContext")]
    fn append_bb(c: *mut u8, f: *mut u8, name: *const u8) -> *mut u8;

    #[link_name("LLVMPositionBuilderAtEnd")]
    fn position_at_end(b: *mut u8, bb: *mut u8);

    #[link_name("LLVMConstInt")]
    fn const_int(ty: *mut u8, val: u64, sign_extend: i32) -> *mut u8;

    #[link_name("LLVMBuildRet")]
    fn build_ret(b: *mut u8, v: *mut u8) -> *mut u8;

    // action = 2 = LLVMReturnStatusAction — MUST NOT be 0/AbortProcess,
    // or a verify failure would abort() instead of returning control.
    #[link_name("LLVMVerifyModule")]
    fn verify_module(m: *mut u8, action: i32, out_message: *mut *const u8) -> i32;

    // Host target = AArch64 (Apple Silicon). The `Native` wrappers are
    // header-inline `static` functions, not exported symbols — bind the
    // concrete per-arch quartet instead.
    #[link_name("LLVMInitializeAArch64TargetInfo")]
    fn init_target_info();
    #[link_name("LLVMInitializeAArch64Target")]
    fn init_target();
    #[link_name("LLVMInitializeAArch64TargetMC")]
    fn init_target_mc();
    #[link_name("LLVMInitializeAArch64AsmPrinter")]
    fn init_asm_printer();

    #[link_name("LLVMGetDefaultTargetTriple")]
    fn default_triple() -> *const u8;

    #[link_name("LLVMGetTargetFromTriple")]
    fn target_from_triple(triple: *const u8, out_target: *mut *mut u8, out_error: *mut *const u8) -> i32;

    #[link_name("LLVMCreateTargetMachine")]
    fn create_target_machine(t: *mut u8, triple: *const u8, cpu: *const u8, features: *const u8, opt: i32, reloc: i32, code_model: i32) -> *mut u8;

    // file_type = 1 = LLVMObjectFile
    #[link_name("LLVMTargetMachineEmitToFile")]
    fn emit_to_file(tm: *mut u8, m: *mut u8, filename: *const u8, file_type: i32, out_error: *mut *const u8) -> i32;

    // libc `exit` for the error paths (`process.exit` has no codegen
    // lowering yet — it is interpreter-only).
    #[link_name("exit")]
    fn c_exit(code: i32);
}

fn main() {
    // ── build  i64 main() { ret i64 42 } ──
    let ctx = unsafe { context_create() };
    let module = unsafe { module_create(c"proof".as_ptr(), ctx) };
    let builder = unsafe { builder_create(ctx) };

    let i64t = unsafe { int64_type(ctx) };
    let no_params: *mut *mut u8 = ptr.null_mut();
    let fnty = unsafe { function_type(i64t, no_params, 0, 0) };
    let func = unsafe { add_function(module, c"main".as_ptr(), fnty) };
    let entry = unsafe { append_bb(ctx, func, c"entry".as_ptr()) };
    unsafe { position_at_end(builder, entry) };
    let answer = unsafe { const_int(i64t, 42, 0) };
    unsafe { build_ret(builder, answer) };

    // ── verify (return-status, never abort) ──
    let mut vmsg: *const u8 = ptr.null();
    let broken = unsafe { verify_module(module, 2, ptr.mut(vmsg)) };
    if broken != 0 {
        println("codegen produced invalid IR");
        unsafe { c_exit(1) };
    }

    // ── host target machine ──
    unsafe { init_target_info() };
    unsafe { init_target() };
    unsafe { init_target_mc() };
    unsafe { init_asm_printer() };

    let triple = unsafe { default_triple() };
    let mut target: *mut u8 = ptr.null_mut();
    let mut terr: *const u8 = ptr.null();
    let tfail = unsafe { target_from_triple(triple, ptr.mut(target), ptr.mut(terr)) };
    if tfail != 0 {
        println("could not resolve host target");
        unsafe { c_exit(1) };
    }

    // opt=0 None, reloc=2 PIC (links cleanly into a macOS executable),
    // code_model=0 Default.
    let tm = unsafe { create_target_machine(target, triple, c"".as_ptr(), c"".as_ptr(), 0, 2, 0) };

    // ── emit the object file ──
    let mut eerr: *const u8 = ptr.null();
    let efail = unsafe { emit_to_file(tm, module, c"answer.o".as_ptr(), 1, ptr.mut(eerr)) };
    if efail != 0 {
        println("object emit failed");
        unsafe { c_exit(1) };
    }

    println("emitted answer.o");
}
```

## Harness

Project layout: `kara.toml` (above) at the root, `proof.kara` at `src/main.kara`. Confirmed
output is shown in the comments (2026-06-11, Apple M-series, `libLLVM-18` from Homebrew `llvm@18`).

```sh
# 1. build the generator (project mode; kara.toml [link] resolves libLLVM-18)
karac build                     # → Built: ./llvmproof

# 2. run it — emits answer.o, a Mach-O arm64 object holding `i64 main() { ret 42 }`
./llvmproof                     # prints: emitted answer.o   (exit 0)
file answer.o                   # answer.o: Mach-O 64-bit object arm64   (520 bytes)

# 3. link the EMITTED object into an executable and run it
cc answer.o -o answer
./answer; echo "exit=$?"        # exit=42   ✅
```

**Pass criterion:** `exit=42` — **met**. That single number proves the whole chain — Kāra
`unsafe extern "C"` declarations (`#[link_name]`-bound to the PascalCase LLVM-C API) linked against
`libLLVM-18`, called to build + verify + emit a real object file, which the system linker turns
into a runnable binary that executes the IR Kāra generated. The hard FFI/ownership gates this proof
surfaced are all closed; this is the spike's Definition of Done under the stage-0 Rust `karac`.

## Cross-stage check (sub-q 6)

Stage-0 (Rust `karac`): ✅ **green — `exit=42`** (2026-06-11). The stage-2 leg (self-hosted `karac`)
remains to be run once that compiler exists — identical source, identical `[link]` resolution,
identical emitted object. A passing proof under **both** is the codegen leg's real correctness
signal; the byte-identical fixpoint only adds self-consistency on top. The stage-2 run is gated on
the self-hosted compiler (the Phase-12 port itself), not on any FFI/ownership prerequisite — those
are all closed.
