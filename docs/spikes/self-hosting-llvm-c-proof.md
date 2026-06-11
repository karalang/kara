# Minimal proof: Kāra `extern "C"` → LLVM-C → object file → link → run

**Status:** SPEC — **NOT yet lift-and-run** (the original "lift-and-run" claim was aspirational;
a 2026-06-11 build attempt of the program below found it does not parse/compile verbatim — see
the gate). This is the spike's [Definition of Done](self-hosting-llvm-c-ffi.md#definition-of-done-this-spike)
minimal proof, written against the resolved design (sub-q 1–6). When the gate below clears it
becomes the **seed of the Kāra codegen module**; until then it is a design artifact, not a
runnable test. The `.kara` body below still needs the edits noted in the gate (statement
terminators, PascalCase extern binding) before it will parse.

The proof builds a trivial module — `i64 main() { ret i64 42 }` — verifies it, emits it to an
object file via the host target machine, links that object into an executable, and runs it.
Success = the linked binary exits `42`. It exercises ~20 of the ~120 llvm-c functions from the
[surface inventory](self-hosting-llvm-c-surface.md): context/module/builder lifecycle, one int
type + fn type, add-function + basic block + position, const-int + ret, verify (return-status),
default triple + target-from-triple + create-target-machine + emit-to-file, and the disposers.

## Prerequisites gate (must be green first)

A 2026-06-11 build of this program (project mode, `[link]` resolving `libLLVM-18`) enumerated the
real gate — it is **longer than the original two-item list**. The list below is the corrected,
build-verified set.

- [x] **Native-library link directive (`kara.toml [link]`)** ✅ LANDED 2026-06-11 — the real blocker,
  now resolved: `[link] libs = ["LLVM-18"]` + `search-paths` append `-L`/`-l` to the native `cc`
  line. The `kara.toml` below works verbatim. ([spike Prerequisites](self-hosting-llvm-c-ffi.md#prerequisites-phase-8-floor) / phase-12 Cluster 2.)
- [x] **`CStr.from_ptr(*const u8) -> ref CStr`** ✅ LANDED 2026-06-11 — the inbound raw-pointer
  C-string constructor used by `read_and_dispose`. Lowers to libc `strlen` + the `{ptr, len}`
  aggregate (same shape a `c"..."` literal lowers to); `unsafe`-gated via the unsafe-fn registry,
  interpreter-rejected. Tested across typechecker/codegen/unsafe-lint.
- [ ] **`#[link_name]` honored on `unsafe extern` fn imports** *(NEW — build-surfaced, the biggest
  remaining gate).* Kāra rejects PascalCase extern fn names (Value-class rule: `fn LLVMContextCreate`
  → *"must be Value-class (snake_case)"*), and the entire LLVM-C API is PascalCase. `#[link_name]` is
  a registered attribute but is **NOT** applied to extern imports — codegen emits the Kāra name as
  the symbol (`#[link_name("strlen")] fn c_strlen` still links `_c_strlen`, undefined). Without
  either honoring `#[link_name]` on externs *or* relaxing the name-class rule inside `unsafe extern`,
  **no** LLVM-C symbol can be bound. → own tracker entry, [phase-12 Cluster 2](self-hosting-llvm-c-ffi.md#prerequisites-phase-8-floor).
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

```kara
// Minimal LLVM-C codegen proof — seed of the self-hosted codegen module.
// Owned handles (Context/Module/Builder/TargetMachine) are Drop newtypes (sub-q 3),
// so the `?` early-returns below stay leak-free on every error path — which is the
// whole reason the handle model uses Drop rather than manual dispose.

// ---- opaque foreign pointee types (sub-q 3 representation) ----
unsafe extern "C" {
    type LLVMOpaqueContext;
    type LLVMOpaqueModule;
    type LLVMOpaqueBuilder;
    type LLVMOpaqueType;
    type LLVMOpaqueValue;
    type LLVMOpaqueBasicBlock;
    type LLVMTarget;
    type LLVMOpaqueTargetMachine;
}

// ---- the ~20-function llvm-c surface this proof needs ----
// extern "C" fns default to `blocks` (FFI effect default) — no per-fn annotation needed.
unsafe extern "C" {
    fn LLVMContextCreate() -> *mut LLVMOpaqueContext;
    fn LLVMContextDispose(c: *mut LLVMOpaqueContext);

    fn LLVMModuleCreateWithNameInContext(name: *const u8, c: *mut LLVMOpaqueContext) -> *mut LLVMOpaqueModule;
    fn LLVMDisposeModule(m: *mut LLVMOpaqueModule);

    fn LLVMCreateBuilderInContext(c: *mut LLVMOpaqueContext) -> *mut LLVMOpaqueBuilder;
    fn LLVMDisposeBuilder(b: *mut LLVMOpaqueBuilder);

    fn LLVMInt64TypeInContext(c: *mut LLVMOpaqueContext) -> *mut LLVMOpaqueType;
    fn LLVMFunctionType(ret: *mut LLVMOpaqueType, params: *mut *mut LLVMOpaqueType,
                        count: u32, is_vararg: i32) -> *mut LLVMOpaqueType;

    fn LLVMAddFunction(m: *mut LLVMOpaqueModule, name: *const u8, fnty: *mut LLVMOpaqueType) -> *mut LLVMOpaqueValue;
    fn LLVMAppendBasicBlockInContext(c: *mut LLVMOpaqueContext, f: *mut LLVMOpaqueValue,
                                     name: *const u8) -> *mut LLVMOpaqueBasicBlock;
    fn LLVMPositionBuilderAtEnd(b: *mut LLVMOpaqueBuilder, bb: *mut LLVMOpaqueBasicBlock);

    fn LLVMConstInt(ty: *mut LLVMOpaqueType, val: u64, sign_extend: i32) -> *mut LLVMOpaqueValue;
    fn LLVMBuildRet(b: *mut LLVMOpaqueBuilder, v: *mut LLVMOpaqueValue) -> *mut LLVMOpaqueValue;

    fn LLVMVerifyModule(m: *mut LLVMOpaqueModule, action: i32, out_message: *mut *const u8) -> i32;
    fn LLVMDisposeMessage(msg: *const u8);

    // host = AArch64 on Apple Silicon (see "Gotcha" above — the `Native` wrappers are header-inline).
    fn LLVMInitializeAArch64TargetInfo();
    fn LLVMInitializeAArch64Target();
    fn LLVMInitializeAArch64TargetMC();
    fn LLVMInitializeAArch64AsmPrinter();

    fn LLVMGetDefaultTargetTriple() -> *const u8;                 // owned -> LLVMDisposeMessage
    fn LLVMGetTargetFromTriple(triple: *const u8, out_target: *mut *mut LLVMTarget,
                               out_error: *mut *const u8) -> i32;
    fn LLVMCreateTargetMachine(t: *mut LLVMTarget, triple: *const u8, cpu: *const u8,
                               features: *const u8, opt: i32, reloc: i32, code_model: i32) -> *mut LLVMOpaqueTargetMachine;
    fn LLVMDisposeTargetMachine(tm: *mut LLVMOpaqueTargetMachine);
    fn LLVMTargetMachineEmitToFile(tm: *mut LLVMOpaqueTargetMachine, m: *mut LLVMOpaqueModule,
                                   filename: *const u8, file_type: i32, out_error: *mut *const u8) -> i32;
}

// ---- Category-A owned handles: non-Copy newtypes with Drop (sub-q 3) ----
// Declared ctx -> module -> builder -> tm so reverse-order drop disposes the Context LAST.
struct Context { raw: *mut LLVMOpaqueContext }
impl Drop for Context { fn drop(mut ref self) { unsafe { LLVMContextDispose(self.raw) } } }
impl Context { fn new() -> Context { Context { raw: unsafe { LLVMContextCreate() } } } }

struct Module { raw: *mut LLVMOpaqueModule }
impl Drop for Module { fn drop(mut ref self) { unsafe { LLVMDisposeModule(self.raw) } } }
impl Module {
    fn new(c: ref Context, name: ref CStr) -> Module {
        Module { raw: unsafe { LLVMModuleCreateWithNameInContext(name.as_ptr(), c.raw) } }
    }
}

struct Builder { raw: *mut LLVMOpaqueBuilder }
impl Drop for Builder { fn drop(mut ref self) { unsafe { LLVMDisposeBuilder(self.raw) } } }
impl Builder { fn new(c: ref Context) -> Builder { Builder { raw: unsafe { LLVMCreateBuilderInContext(c.raw) } } } }

struct TargetMachine { raw: *mut LLVMOpaqueTargetMachine }
impl Drop for TargetMachine { fn drop(mut ref self) { unsafe { LLVMDisposeTargetMachine(self.raw) } } }

// ---- error type: ICE/environment class, never a fake source span (sub-q 5) ----
enum CodegenError { InvalidIR(String), TargetInit(String), EmitFailed(String) }
impl CodegenError {
    fn message(ref self) -> String {
        match self {
            CodegenError.InvalidIR(d)  => "codegen produced invalid IR: " + d,
            CodegenError.TargetInit(d) => "target init failed: " + d,
            CodegenError.EmitFailed(d) => "object emit failed: " + d,
        }
    }
}

// Read an LLVM-owned char* into an owned String, then dispose it (sub-q 4 outbound path).
fn read_and_dispose(msg: *const u8) -> String with blocks {
    if unsafe { ptr.addr(msg) } == 0 { return String.new() }
    let s = unsafe { CStr.from_ptr(msg) }.to_string().unwrap_or(String.new());
    unsafe { LLVMDisposeMessage(msg) }
    s
}

fn verify(m: ref Module) -> Result[(), CodegenError] with blocks {
    let mut msg: *const u8 = ptr.null()
    // action = 2 = LLVMReturnStatusAction — MUST NOT be 0 (AbortProcess) or it kills the process (sub-q 5).
    let broken = unsafe { LLVMVerifyModule(m.raw, 2, ptr.mut(msg)) }
    if broken != 0 { return Err(CodegenError.InvalidIR(read_and_dispose(msg))) }
    Ok(())
}

fn init_host_target() {
    unsafe {
        LLVMInitializeAArch64TargetInfo()
        LLVMInitializeAArch64Target()
        LLVMInitializeAArch64TargetMC()
        LLVMInitializeAArch64AsmPrinter()
    }
}

fn host_target_machine() -> Result[TargetMachine, CodegenError] with blocks {
    let triple = unsafe { LLVMGetDefaultTargetTriple() }      // owned
    let mut target: *mut LLVMTarget = ptr.null()
    let mut err: *const u8 = ptr.null()
    let failed = unsafe { LLVMGetTargetFromTriple(triple, ptr.mut(target), ptr.mut(err)) }
    if failed != 0 {
        let detail = read_and_dispose(err)
        unsafe { LLVMDisposeMessage(triple) }
        return Err(CodegenError.TargetInit(detail))
    }
    // opt=0(None), reloc=2(PIC — links cleanly into a macOS executable), code_model=0(Default)
    let tm = unsafe { LLVMCreateTargetMachine(target, triple, c"".as_ptr(), c"".as_ptr(), 0, 2, 0) }
    unsafe { LLVMDisposeMessage(triple) }
    Ok(TargetMachine { raw: tm })
}

fn emit_object(tm: ref TargetMachine, m: ref Module, path: ref CStr) -> Result[(), CodegenError] with blocks {
    let mut err: *const u8 = ptr.null()
    // file_type = 1 = LLVMObjectFile
    let failed = unsafe { LLVMTargetMachineEmitToFile(tm.raw, m.raw, path.as_ptr(), 1, ptr.mut(err)) }
    if failed != 0 { return Err(CodegenError.EmitFailed(read_and_dispose(err))) }
    Ok(())
}

fn build_and_emit(obj_path: ref CStr) -> Result[(), CodegenError] with blocks {
    let ctx = Context.new()
    let module = Module.new(ctx, c"proof")
    let builder = Builder.new(ctx)

    let i64t = unsafe { LLVMInt64TypeInContext(ctx.raw) }
    // i64 main()  — no params; pass a null param array with count 0.
    let fnty = unsafe { LLVMFunctionType(i64t, ptr.null[*mut LLVMOpaqueType](), 0, 0) }
    let func = unsafe { LLVMAddFunction(module.raw, c"main".as_ptr(), fnty) }
    let entry = unsafe { LLVMAppendBasicBlockInContext(ctx.raw, func, c"entry".as_ptr()) }
    unsafe { LLVMPositionBuilderAtEnd(builder.raw, entry) }
    let answer = unsafe { LLVMConstInt(i64t, 42, 0) }
    unsafe { LLVMBuildRet(builder.raw, answer) }

    verify(module)?                       // leak-free on Err: ctx/module/builder Drop run on return
    init_host_target()
    let tm = host_target_machine()?
    emit_object(tm, module, obj_path)?
    Ok(())
    // ctx, module, builder, tm all Drop here in reverse order — Context disposed last.
}

fn main() with blocks {
    match build_and_emit(c"answer.o") {
        Ok(()) => println("emitted answer.o"),
        Err(e) => println(e.message()),
    }
}
```

## Harness

```sh
# 1. build the generator (needs kara.toml [link] → libLLVM-18)
karac build proof.kara

# 2. run it — emits answer.o, an object containing `i64 main() { ret 42 }`
./proof                         # prints: emitted answer.o

# 3. link the EMITTED object into an executable and run it
cc answer.o -o answer
./answer; echo "exit=$?"        # expected: exit=42
```

**Pass criterion:** `exit=42`. That single number proves the whole chain — Kāra `extern "C"`
declarations linked against `libLLVM-18`, called to build + verify + emit a real object file,
which the system linker turns into a runnable binary that executes the IR Kāra generated.

## Cross-stage check (sub-q 6)

Before trusting the bootstrap fixpoint, run this proof green under **both** the stage-0 Rust
`karac` *and* the stage-2 self-hosted `karac` — identical source, identical `[link]` resolution,
identical emitted object. A passing proof under both is the codegen leg's real correctness signal;
the byte-identical fixpoint only adds self-consistency on top.
