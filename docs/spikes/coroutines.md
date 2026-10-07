# Spike: stackful or stackless tasks for `suspends`

Status: **decided 2026-10-07: stackless** (Gowtham, on the decision card; plan recheck 2026-10-07 §2.3). Written by the MIR owner. Claims are marked **(checked)** when read off the spec or the tree, and **(inferred)** otherwise.

## What is being decided

A call to a `suspends` function may park the task and free the thread (design.md § Execution Effects). The compiler inserts the yield points, so there is no `await` **(checked)**. M4 brings `suspends` back. The question is what a parked task *is*:

- **Stackless.** Each suspending function compiles to a state machine. Its locals that live across a yield move into a heap frame, and the task is a chain of those frames, as in Rust's async.
- **Stackful.** Each task owns a machine stack. Parking saves the registers and switches stacks, as in Go.

## Side by side

| | Stackless | Stackful |
|---|---|---|
| **What MIR needs** | A transform after drop elaboration (rustc's generator transform is the model). Locals live across a yield become frame fields, the body becomes a resume function that switches on a state number, and every drop of a frame field moves into it. Liveness and drop flags already exist from elaboration and borrowck, so the transform reuses them **(inferred)**. | Nothing. A suspending call is an ordinary `Call` into the runtime, which switches stacks. |
| **A `ref` held across a suspension** | The frame holds pointers into itself, so it must never move after its first resume. Kāra exposes no future or task value that owns a frame: `spawn` returns a `TaskHandle` that owns only the result (core semantics §9.5) **(checked)**. So the frame can live in one heap allocation for its whole life, and the rule holds by construction. It needs no user-visible `Pin`. The rule must be written down, because it becomes a design constraint the day anyone proposes a first-class future. | Works with no rule: the stack never moves. |
| **Function colouring** | Every function between the task root and a yield must be transformed. `suspends` is already inferred transitively and must be declared on public functions **(checked)**, so the compiler already knows the colour. A generic used both ways (e.g. an `Iterator` whose `next()` suspends) is monomorphized per effect set. | None needed for code generation. `suspends` still drives scheduler placement. |
| **Recursion and dynamic calls** | A recursive cycle through `suspends` has no static frame size. The compiler boxes the callee's frame at the back edge, one allocation per recursive call, or rejects the cycle. The same holds for a `suspends` call through a function value whose callee is unknown. | No issue. |
| **Memory per task at 1M connections** | The frame is the largest set of state live across one yield, along the deepest suspending path. For socket handlers that is typically hundreds of bytes to a few KB (Rust/tokio experience, **inferred**), so about 1 GB at 1M tasks. Memory grows only with what is actually held. | Kāra cannot copy and grow a stack: `ref`s are raw interior pointers and there is no GC to fix them up **(inferred: MIR `ref`s are plain addresses and nothing tracks them at run time)**. The options are: (a) a fixed small stack per task, where overflow is a crash and guard pages are required; (b) a large virtual reservation per task (for example 256 KB), with at least one 4 KB page plus a guard committed, so ≥ 8 GB resident at 1M tasks; or (c) segmented stacks, which Go and Rust both abandoned for hot-split cliffs. Option (b) also needs two mappings per task (stack plus guard), against Linux's default `vm.max_map_count` of 65 530, so a 1M-task server needs that limit raised, or one pooled mapping with guard pages carved out **(inferred)**. Once touched, stack memory is not returned without `madvise`. |
| **FFI** | C runs on the thread's own stack, so there is nothing to do. | C code must not run on a small task stack. Either every `extern` call switches to the thread's system stack (Go's cgo cost) or task stacks stay large. `extern` never suspends, which helps but doesn't remove the stack-size hazard **(checked: design.md § Leaves for `suspends`)**. |
| **wasm** (`wasm_wasi`, `wasm_browser` are shipping targets) | It is ordinary code: functions plus a switch. Nothing target-specific is needed, and it runs on today's sequential scheduler. | Core wasm cannot switch stacks. The stack-switching proposals (JSPI for JS hosts, typed continuations for WASI hosts) are not something every host Kāra targets can be assumed to have **(inferred)**. The fallback is Binaryen's Asyncify, which is a stackless transform applied to the wasm afterwards, with large code-size overhead. Stackful would therefore still need a stackless path on wasm. |
| **Debugging** | Stack traces across a yield show the resume chain, not a call stack. That needs runtime support to print well. | Native stack traces work. |
| **Build cost** | The transform is the largest single piece: interacting with drop elaboration, borrowck (allowing `ref`s across a yield because the frame does not move) and the M2 frame layout **(inferred)**. | Small: a context switch per architecture (x86-64, AArch64), plus the stack allocator. A wasm path is still needed (see the wasm row). |

## How each choice affects other work

- **M2 frame layout.** Under stackless, a suspending function has two layouts: the LLVM stack frame for state that never crosses a yield, and the heap frame for state that does. M2's local layout has to leave room for that split. This is why the decision comes before M2.
- **Unwind slot** (adopted the same day). If task recovery arrives, dropping a task parked mid-suspension must run the drops of whatever is live at that yield. Under stackless, those drops are a per-state cleanup path generated from the same liveness the transform already computes. Under stackful, it means unwinding a foreign stack, which needs the cleanup edges in every frame on it.
- **§11.7: no `par` branch is cancelled.** Neither model needs cancellation now. If cancellation is ever wanted, stackless can drop a frame at any yield point. Stackful cannot without unwinding.

## Lean, for the decision

**Stackless (inferred).** Its cost is concentrated in one MIR transform, which the drop-elaboration and liveness machinery already half builds. Its two classic pains are already covered: function colouring is free, because `suspends` is inferred, and pinning holds by construction, because no frame is a value. Stackful's main advantage, no transform, is cancelled on wasm, where it needs Asyncify, which is a stackless transform anyway. On native it costs either a hard memory floor per task or crash-on-overflow small stacks, both at the 1M-connection scale the services plan targets.

The open cost of stackless is a recursive or dynamically dispatched `suspends` call, where the frame must be boxed per call. Measuring how often real service code does that (the corpus has few `suspends` programs today) is the first M4 task under this choice.
