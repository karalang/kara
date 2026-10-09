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

## The MIR transform (M4 plan, 2026-10-09)

Written by the MIR owner after the decision, as the plan the implementation follows. **(checked)** and **(inferred)** as above. It follows rustc's generator transform, without the storage overlap for now.

**Which bodies.** A body is a coroutine when its effect set (C10, `src/mir/effects.rs`) contains `suspends`. A suspension point is a `Call` whose callee is a coroutine, or a native that suspends (socket I/O, `sleep`, a channel receive, a join). No new terminator appears in the bodies the builder makes, so every pass before the transform (move check, borrow check, drop elaboration) sees a suspending call as an ordinary call.

**Where.** After drop elaboration and the borrow check, so drop flags and explicit drops already exist and only the validator runs after it. A transformed body is marked as one. For such a body only, the validator allows a move out of a frame field through the frame reference.

**What it makes, for a coroutine `F(args) -> T`:**
- **The frame**, `F.Frame`: a struct with a `state: u32`, one field per local that is live across any suspension point (arguments included), one field per drop flag of such a local, and one field `sub_k: G.Frame` per suspension point `k` calling `G`. A field is uninitialized whenever its local is. Each local keeps a field of its own across the whole body, so a `ref` into a local held across a yield keeps pointing at the same field. Overlapping fields whose live ranges are disjoint is an M2 layout optimization, not part of the transform.
- **The resume function**, `F.resume(frame: mut ref F.Frame) -> Poll[T]`, where `Poll[T]` is `Ready(T) | Pending`. Its body is `F`'s body, with each frame local replaced by its field. It enters through a switch on `state`: 0 goes to the original entry, `k` goes to suspension point `k`'s poll block, and the returned state aborts. A suspension point `dest = G(a…) -> next` becomes:
  - `(*frame).sub_k = G.Frame { state: 0, a… }`, then the poll block;
  - poll: `r = G.resume(&mut (*frame).sub_k)`. On `Ready(v)`, `dest = v` and continue to `next`. On `Pending`, `(*frame).state = k` and return `Pending`.
  - A return becomes `_0 = Ready(value)`, then `state = RETURNED`, then return.
- **The cleanup function**, `F.drop_frame(frame: mut ref F.Frame)`. It switches on `state`, drops the fields that are live and initialized at that state (gated by the saved drop flags), and calls `G.drop_frame` on `sub_k`. It runs when a frame is dropped before it returns. v1 cancels nothing (§11.7), so today it runs only on frames that already returned, where it does nothing. It exists so that cancellation and the unwind slot's `Cleanup` edge have their drops ready.

**The borrow check needs no change.** It runs before the transform, where a suspending call is a call. A `ref` held across a yield points into a frame field, and the frame does not move after its first resume: it lives in the task, and no Kāra value owns a frame **(checked: §9.5, `TaskGroup` owns only results)**. Effect conflicts between tasks are C10's.

**Task roots.** `suspends` is transitive, so only a task root calls a coroutine from outside one: `main`, a `par {}` branch, a `par for` body, or a `TaskGroup.spawn` closure. The runtime (or, in tests, the interpreter's executor) holds the root's frame and calls its `resume` until `Ready`, parking the task on `Pending`.

**Not in the first cut:**
- **A recursive cycle of coroutines** has no finite frame. It is refused with a diagnostic until the first measurement says how often services need it, as the decision asked. Then the callee frame at the back edge is boxed.
- **A `suspends` call through a function value** whose callee is unknown is refused for the same reason.

**The interpreter** already runs suspending programs correctly without the transform, because its natives complete synchronously. So the transform is validated by an executor behind `KARAC_MIR_COROUTINES=1`, with a test native `__yield_now()` that is `Pending` on its first resume, until the corpus agrees under it. Only then does the executor become the default.

**Steps:**
1. Suspension points, from C10's effects.
2. Liveness across them, and the frame layout.
3. The resume body.
4. `drop_frame`.
5. The executor and tests.
6. The recursion and function-value refusals.

**Status (2026-10-09).** Steps 1 to 6 are in `src/mir/coroutine.rs`. A body is a coroutine when it declares `suspends` (`Body.suspends`, set by the builder) or directly calls a coroutine or `__yield_now`. C10's effect sets have no `suspends` verb yet, so the declared flag stands in for them. The frame holds every argument, every local live across a point, and every borrowed local. The last is a conservative stand-in for loan liveness. `interp::run_coroutines`, or `KARAC_MIR_COROUTINES=1` on the ordinary entry points, runs a coroutine `main` under the executor. The tests check that it prints what the synchronous run prints and that it resumes once per yield. `F.drop_frame` drops what a suspended frame owns: the callee's frame first, then the locals in reverse order. It reads which places are initialized at each point from elaboration's dataflow, and it guards a maybe-initialized place with the flag elaboration made for it (`Body.drop_flags`). `interp::run_coroutines_cancelled` exercises it. The validator lets those two bodies move out of their frame's fields. The generated names are `F#resume`, `F#drop_frame` and `F#Frame`. `#` cannot appear in a Kāra name, so they never collide with a user method or type. The interpreter parks a suspended frame's borrow flags until its next resume, or until `drop_frame` releases them. Under the executor, `sleep_ms`, a channel `recv` and a `TaskHandle.join` are suspension points, alongside `__yield_now`. Each yields once before its call and makes the call when resumed, since the interpreter's natives complete synchronously. That puts every corpus program using them through the frame machinery. Of the 52 corpus programs that use them or declare `suspends`, 51 print the same output, with the same exit code, with and without `KARAC_MIR_COROUTINES=1` (measured 2026-10-09; 28 of the 51 fail on `__mir-run` either way). The remaining one, `test_http2_sibling_streams_run_concurrently`, is refused because its handler closure suspends. The transform refuses a coroutine that is used as a value, meaning a function item taken as a value or a closure that is made, because its callers cannot be found. Still open:
- C10's `suspends` verb. Once effects infer it, `coroutines()` should read it rather than the declared flag.
- Narrowing the frame from every borrowed local to the locals whose loans are live across a point.
- The MIR to LLVM lowering of these bodies (M2).
