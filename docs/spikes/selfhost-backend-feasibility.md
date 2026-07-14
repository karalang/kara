# Backend feasibility for a self-hosted (Kāra-authored) codegen

**Status:** investigation findings, not started. **Question asked:** can a
compiler backend written in Kāra actually emit runnable code, and what closes
the self-hosting loop? **Companion:** `selfhost-typechecker-real-types.md`.

## 0. Bottom line

A Kāra-authored backend is **architecturally feasible via textual LLVM IR** — no
inkwell/LLVM-C FFI required — and the `.ll → run` machinery it needs already
exists. But it is the largest component in the compiler (`src/codegen.rs` is
**8924 LOC**, built on inkwell's builder), its verification is **behavioral, not
byte-identical** (weaker than the front-end oracle), and it **consumes real
types** — which the current coarse-category TypeChecker cannot produce. The
single highest-information next step is a **bounded hello-world IR-emission
spike** (§6), not a commitment to the full port.

## 1. Self-hosting is a real bootstrap goal (not just dogfooding)

`docs/roadmap.md` north-star #5: the Kāra compiler is rewritten in Kāra *before*
the long-tail stdlib, pulled into v1; **Phase 11 is built *on* the self-hosted
compiler**, so "every compiler-internal feature (… the codegen IR pass) is
written once, in Kāra." The LLJIT-productionization spike was sequenced
*before* Phase 12 specifically because "self-hosting's bootstrap loop leans on
`karac run`/`test` as the execution path." So the backend question is on the
critical path, and this feasibility check is load-bearing.

## 2. The emission path: textual LLVM IR, no FFI

The seed builds an inkwell `Module`, but it can already **`module.print_to_string()`
→ textual `.ll`** (`src/codegen.rs` — used in ~8 places). And a **`.ll` → execute**
path already ships: `run_ir_via_jit_subprocess` (`src/cli.rs:4301`) writes IR
text to a temp `.ll` and spawns **`karac_jit_runner <path.ll>`**, which JITs and
runs it (the same machinery that passes 2084/2084 codegen-E2E-via-JIT).

So the viable shape is:

```
Kāra backend  :  AST (+ types)  ──►  LLVM IR text : String     (pure transform)
Rust harness  :  String  ──►  karac_jit_runner / assemble+link  ──►  run
```

The Kāra backend is a **pure data transform**, exactly like the ported front-end
(`source → tokens/AST/errors`). It needs **no inkwell bindings** and **no
process spawning of its own** — it returns the IR string; the harness (or the
existing `karac` driver) drives LLVM. This sidesteps the one relevant runtime
gap:

- **I/O capability audit (verified):** Kāra has real file write/read and argv in
  BOTH the interpreter and native codegen (`karac_runtime_fs_write` /
  `karac_runtime_file_*` / `karac_runtime_env_args_into`). **Process spawning is
  interpreter-only** — there is no `karac_runtime_process_*` codegen lowering, so
  a *natively-compiled* Kāra program cannot shell out to `llc`/`clang`. The
  pure-transform architecture makes this a **non-blocker** (the backend returns
  a string; it never spawns), which is also why the front-end oracle works.

## 3. Verification is BEHAVIORAL, not byte-identical — the key downgrade

Every front-end slice was verified by **byte-identical diff** of a canonical
render (`(kind @off:len)` / AST-render) against the seed — deterministic and
exact, which is what made 21 slices cheap and safe.

That does **not** carry over to codegen. inkwell-emitted IR carries incidental,
construction-order-dependent detail — SSA value numbers (`%1 %2 …`),
auto-generated basic-block labels, instruction ordering — that a from-scratch
Kāra reimplementation will not reproduce character-for-character. So the oracle
must be **behavioral**:

```
compile(Kāra-emitted IR) ─run─►  stdout/exit  ==  compile(seed) ─run─►  stdout/exit
```

This is coarser: it catches *wrong behavior* but not *structurally-different-yet-
equivalent IR*, and it localizes failures far worse than an exact diff (a whole
program is right/wrong, not a one-line span). Mitigation: the seed's **codegen-E2E
corpus (~2084 cases)** is a ready-made behavioral oracle — a Kāra backend can be
driven against the same expected-output fixtures. Per-construct IR-shape unit
checks (normalized/regex-tolerant) can supplement, but the primary gate is
end-to-end output parity.

## 4. Scale + the real-types dependency (the load-bearing constraint)

`src/codegen.rs` is **8924 LOC** — the largest file in the compiler — plus
`src/codegen/*` (driver, closures, file, lljit, …). A text-IR reimplementation
does not have to reproduce inkwell's *API*, but it must reproduce every lowering
**decision**: type→LLVM layout, calling conventions, name mangling,
monomorphization, RC/ownership **drop-glue insertion**, auto-parallelization,
struct/enum layout, the runtime-symbol **ABI**. This is larger than the entire
front-end (~5 kLOC across lexer+parser+resolver+typechecker) combined.

**Critical coupling:** codegen consumes *real types* — concrete int **widths**
(`i64` is 64-bit), **monomorphized** generic instantiations (`Vec[i64]` → a
concrete layout), struct layouts, resolved method targets. The current
**coarse-category TypeChecker cannot supply any of this** (it produces an i64
category, not a width/layout). Therefore:

- The coarse TypeChecker is a **bootstrap dead-end** — invaluable as a dogfooding
  / expressiveness proof and bug-finder, but not part of a final all-Kāra
  pipeline, because the next stage needs types it doesn't produce.
- The **real-types TypeChecker** (`selfhost-typechecker-real-types.md`) is thus
  **on the bootstrap critical path**, not an optional strictness upgrade. That
  reframes it: its value is "feeds codegen," not merely "rejects more wrong
  programs."
- During the *port*, this can be deferred: codegen can be developed against the
  **seed's** real types (one phase ported at a time, others still Rust), so
  codegen does not *block* on the Kāra real-types checker. But a *complete*
  bootstrap needs both.

## 5. Remaining bootstrap surface (for honest expectations)

A true all-Kāra pipeline still needs, beyond what's ported (lexer, parser,
resolver, coarse typechecker): **real-types TypeChecker** (~10 slices + the
unification codegen-bug risk), **EffectChecker**, **OwnershipChecker**, and
**Codegen** (the 8.9 kLOC monster), plus the driver glue. This is a
multi-month program. None of it is blocked by a missing language capability
(the text-IR path and I/O both exist); it is blocked only by volume and by the
codegen behavioral-oracle's weaker localization.

## 6. Recommendation: a bounded hello-world IR spike (go/no-go)

Before committing to either the real-types work or the codegen port, run the
**smallest experiment that answers "can Kāra emit IR that actually runs":**

1. Hand-write (in a `.kara` string builder, or first just in a `.ll` file) the
   LLVM IR for `fn main() { println("hi") }` — declaring and calling the exact
   runtime symbol `println` lowers to, with the correct ABI.
2. Feed it to `karac_jit_runner` and confirm it prints `hi` and exits 0.
3. Then generate that same IR **from a tiny Kāra program** (a 50-line
   `emit_hello() -> String`) and run its output through the runner.

This costs ~1 slice and de-risks the **single biggest unknown of the whole
bootstrap**: whether Kāra can produce runnable IR text against the real runtime
ABI at all. Success → the bootstrap is credible and there's a foundation to
grow (arithmetic, control flow, calls, structs — incrementally, each behavioral-
oracle-verified). A wall (runner rejects hand IR, runtime-ABI mismatch,
string-building the IR is intractable) → we've learned it cheaply, and the
answer to "generics-types vs codegen vs stop" is settled by evidence rather than
guesswork.

**This spike is strictly higher-information than starting Phase A of the
real-types work**, because it tests the assumption the entire bootstrap rests on.

## 7. Spike RESULT (2026-07-12): GO

Ran the §6 experiment end to end. **All three steps passed.**

1. **Learned the ABI from the seed.** `fn main() { println("hi") }` emits ~833
   lines of IR, but the load-bearing core is tiny — `println` lowers to libc
   `fwrite` through a module-internal wrapper:
   ```llvm
   @str = internal constant [3 x i8] c"hi\00"
   @stdout = external global ptr
   declare i64 @fwrite(ptr, i64, i64, ptr)
   define i32 @main() {
     %s = load ptr, ptr @stdout, align 8
     call i64 @fwrite(ptr @str, i64 1, i64 2, ptr %s)   ; "hi"
     %s2 = load ptr, ptr @stdout, align 8
     call i64 @fwrite(ptr @nl, i64 1, i64 1, ptr %s2)   ; "\n"
     ret i32 0
   }
   ```
   (The ~820 other lines are runtime-symbol declarations and helper defs that a
   real program pulls in — panic prefix, allocator, par-run, provider table —
   none needed for hello-world.)

2. **Hand-written 16-line `.ll` runs.** A by-hand minimal module (the core above
   + datalayout/triple) fed to `karac_jit_runner` printed `hi` and exited 0 —
   the runner accepts arbitrary hand-authored IR against the real runtime ABI
   (it links the runtime + libc and JITs `main`), not only inkwell-produced IR.

3. **A Kāra program emitted that IR and it ran.** A ~20-line
   `emit_hello() -> String` (plain `String` concatenation) printed the module;
   piping its stdout to `karac_jit_runner` printed `hi`, exit 0. **Kāra produced
   runnable LLVM IR against the real runtime ABI.**

**Conclusion.** The single biggest bootstrap unknown is resolved: Kāra *can* be
the IR producer, using nothing but string building + the already-shipped
`.ll → run` path, with no inkwell/LLVM-C FFI and no process-spawn. The backend
is a pure `AST → IR-string` transform whose output the existing runner executes.
The remaining work is **volume, not feasibility** — grow the emitter construct
by construct (integers → arithmetic → control flow → calls → structs →
RC/drop-glue → monomorphization), each step behavioral-oracle-verified against
the seed's `karac run` output and, eventually, the ~2084-case E2E corpus.

**Reproduction artifacts** (this session, `/tmp`): `hello_seed.ll` (seed IR),
`hello_hand.ll` (hand-written minimal), `/tmp/emit/src/main.kara` (the Kāra
emitter). All three print `hi` via `target/debug/karac_jit_runner`.
