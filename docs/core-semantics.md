# Kāra core semantics (v2)

**Status: normative.** Approved by Gowtham on 2026-10-06 at 18:15Z, as DRAFT 2 from the review thread, which had folded in the drop schedule thread's review.
- It implements the decisions approved at 17:46Z: C1–C11 and P1 in §2 of the redesign proposal (`REDESIGN_PROPOSAL_2026-10-06.md`, in the project's shared `review/` folder).
- It is normative for the topics in §0. It supersedes `docs/spikes/ownership-drop-judgment.md` §0, and any part of `docs/design.md` that disagrees with it.
- **Pinning programs** are named after each rule, for example `drop_scope`. Each has a program, its v2 output and today's legacy output. They move to `corpus/core/<name>/` with the corpus extraction. Until then they are in the shared folder at `review/core-pins/`.
- **Drop reference model:** `review/drop-model/` in the shared folder checks §7 independently of the compiler. All 26 pins it can encode agree with it.
- **Changes** to this file need the owner's approval, like any language decision.

## Choices beyond the proposal (approved with this file)

1. **Evaluation is left to right everywhere, including assignment** (§8).
   - Today `v[i] = e` evaluates `e` before `i`.
   - Today `v[i] += e` evaluates `i` **twice** (pin `ok_compound_assign_once`; legacy prints `p1 p8 p1`).
2. **Order of parts inside a value** (§7.8):
   - Struct fields, tuple elements and enum payloads drop **last part first**.
   - `Vec`/`Array` elements drop **first to last**.
   - Tuples change: today they drop first to last.
   - This also corrects `design.md`'s enum line, which contradicted both the struct rule and the interpreter.
3. **Pattern bindings move by default, as in Rust** (§4.6). *Changed after the drop schedule thread's review; DRAFT 1 kept today's inferred mode.* Over an owned scrutinee, a non-`Copy` binding moves its part out; `ref name` borrows instead.
   - Why: with inference, adding `.push(x)` inside an arm moves the payload's `Drop` from the scrutinee's scope end to the arm's end. That is the non-local drop timing this redesign removes, and the per-arm "only reads the binding" classifier behind it produced a whole legacy bug family.
   - Cost: a read-only arm followed by a later use of the scrutinee becomes E0500, and `karac fix` adds `ref`. The parser must accept `ref name` in any pattern; today it accepts it only as `ref name @ PAT`, although `design.md` already specifies the general form.
   - `_` never binds and never moves.
4. **`errdefer` shares the one LIFO with `defer` and destructors** (§7.7). Today the interpreter runs an `errdefer` before a `defer` declared after it.
5. **Function-typed parameters are non-escaping by default** (§9.3). To store or return a parameter closure, declare it `escaping Fn(...)`. This is **new syntax**: one keyword, Swift's `@escaping`.
6. **Views in generic code are checked on monomorphised instances** (§5.8). There is no new syntax; an error names the generic body's offending line.
7. **Shared handles** (§6.1): every copy increments the count, and every holder releases at its own scope end. This makes `design.md`'s "assignment shares" precise.
8. **Two `par` branches that both print conflict** (§11.2), because `println` has `writes(Stdout)`. Today the runtime buffers each branch's output and replays it in source order.
9. **A panic exits with code 101** (§10), as today.

---

## 0. Scope and conventions

**This file is normative for:**
- values and moves;
- places;
- parameters, calls and patterns;
- references and views;
- sharing;
- destruction;
- evaluation order;
- closures;
- panics;
- concurrency safety;
- the soundness defaults of effects.

**`design.md` stays normative for** syntax, types and inference, traits, generics, modules, numerics and stdlib APIs, wherever it does not contradict this file.

**Words.**
- **Error** means a compile-time error that every command reports: `check`, `build`, `run`, `test`. There is no command that tolerates it.
- **Panic** is defined in §10.
- **Unspecified** means the result may differ between runs or backends, and a program must not depend on it.

**Every rule is pinned.** A rule is normative only if at least one pinning program exercises it. The corpus runner checks every pin on every backend.

**Implementation.** This is the contract for the v2 middle end (`--core=v2`) and the MIR interpreter. The legacy backend diverges in the places Appendix A lists, and it will not be fixed.

---

## 1. Values

**1.1 `Copy` types.**
- The primitives (integers, floats, `bool`, `char`).
- `Array[T, N]`, tuples and `Option[T]` when their parts are `Copy`.
- User types with `#[derive(Copy)]`, which the compiler checks field by field.
- `distinct type` does not inherit `Copy`.
- All of this is unchanged from `design.md` Part 6.

**1.2 Move-only types.** Every other type is move-only (§3). The shared handle types of §6 are the one exception: they are duplicated by counting, not by moving.

**1.3 `Clone`.** Duplication of a move-only value is written by the programmer as `.clone()`. The compiler never inserts a copy, a clone or a reference count to make a program legal.

**1.4 Drop glue.**
- A type **needs drop** if any part of it owns heap memory, holds a shared handle, or has a `Drop` body.
- A drop is **observable** if a `Drop` body or a shared handle is involved anywhere inside the value.
- The observable/unobservable distinction matters only for optimisation (§7.10).

---

## 2. Places

**2.1 What a place is.** A *place* is a root followed by a path of projections.
- **Roots:** a local binding, a parameter, or a temporary.
- **Projections:**
  - `.field`;
  - `.0` (a tuple element);
  - `[i]` (an index into a collection);
  - the implicit dereference of a `ref` or `mut ref`.

**2.2 Using a place as a value.**
- If the type is `Copy`, using the place copies it.
- If it is a shared handle, the use counts (§6.1).
- Otherwise the use is a **move** (§3), unless the context borrows it (a `ref` / `mut ref` parameter or receiver, or a borrowing operation).

**2.3 Initialization state.** For every place path and every program point, the compiler knows whether the place is initialized, moved, maybe-moved (moved on some paths) or partially moved. That state is what §3, §5 and §7 consult.

---

## 3. Moves

**3.1 Where a move happens.** In each of these positions, a move-only value moves:
- a `let` initializer;
- the right-hand side of an assignment;
- an argument to a by-value parameter;
- a `self` (consuming) receiver;
- a `return` or a block's tail value;
- a field or element of a struct, tuple, array or enum literal;
- a capture by an escaping closure (§9);
- a pattern binding in move mode (§4.6);
- `for x in c.into_iter()`.

Whether an argument moves is decided only by the callee's declared parameter mode.

**3.2 Use after move (C1).** Using a place that is moved, or maybe-moved, on any path reaching the use is error **E0500**. "Using" means reading it, borrowing it, moving it, or calling a method on it.

Pins:
- `err_use_after_move`: today a warning and a hidden copy; it prints `hi hi`.
- `err_maybe_moved`: today no diagnostic at all, an RC fallback note, and `hi hi`.

**3.3 Loops.** Moving, inside a loop body, a place declared outside the loop is E0500 unless every path back to the loop head initializes the place again. Pin: `err_moved_in_loop`; today there is no diagnostic.

**3.4 Reinitialization.** Assigning to a moved place makes it initialized again. Pin: `ok_reinit`.

**3.5 Conditional moves.** A place moved on some paths only is maybe-moved after the paths join.
- Using it there is E0500 (§3.2).
- At scope end it is dropped only on the paths where it is still initialized. A backend implements that with a drop flag where the paths cannot be told apart statically.
- Pin: `drop_conditional_move`.

**3.6 Partial moves (C4).** A non-`Copy` field, tuple element or enum payload may be moved out of an **owned local, pattern binding or by-value parameter**, unless the root's type, or any type on the path from the root to the moved part, has a `Drop` body.
- After a partial move, the root cannot be used, borrowed or moved as a whole. Its remaining fields still can.
- Assigning a value back into the moved field restores it, without dropping anything. The root is usable as a whole again once every moved part is restored.
- The fields that were not moved drop at the root's scope end (§7.9).
- Pins: `drop_partial_move`, `err_partial_then_whole`.

**3.7 Moves that are errors (C3).** It is an error to move a non-`Copy` value out of a place rooted at:
- a `ref` or `mut ref` parameter or binding, or a view (§5);
- a `shared`, `par` or `frozen` value (§6);
- an index projection `v[i]` (already `E_INDEX_MOVE_NON_COPY`);
- a field of a type with a `Drop` body.

**The fixes:** `.clone()`, `mem.take(place)` (for `T: Default`), `mem.replace(place, value)`, `mem.swap(a, b)`, `Option.take()`, or `for x in c.into_iter()`.

Pins:
- `err_move_out_of_ref`: today a `borrow_projection_copy` warning plus a deep copy.
- `err_partial_move_drop_type`.

---

## 4. Parameters, calls and patterns

**4.1 Modes are declared and unchanged.**
- Parameter forms: bare `T` (owned), `ref T`, `mut ref T`, `Slice[T]`, `mut Slice[T]`.
- Receivers: `self`, `ref self`, `mut ref self`.
- Call sites carry `mut` markers (`design.md` Part 1½).
- `ref` is never written at a call site.

**4.2 Owned parameters belong to the callee (C2).**
- An owned parameter, including a by-value `self`, is a local of the callee's outermost scope, declared before the body in parameter order.
- The callee drops it at its scope end (§7.3), unless it moves it on.
- The caller has moved the argument (§3.1) and never touches it again. A temporary passed as an argument also belongs to the callee.
- A conditional move inside the callee uses a drop flag inside the callee.

This replaces D3.4 of the drop judgment (the caller dropping the argument when the call returns). In ordinary code the printed order is the same; what changes is who emits the drop.

Pins: `drop_callee_owns`, `drop_callee_returns`.

**4.3 Receivers on shared types.** A `shared` type's methods take `ref self` only. This is unchanged.

**4.4 Return values** move to the caller.

**4.5 Argument evaluation.** Arguments are evaluated left to right, after the callee expression and the receiver (§8).

**4.6 Pattern binding modes.** This covers `match`, `if let`, `while let`, `let` destructuring and `for` patterns. The mode of every binding is decided by the pattern and the scrutinee's type, never by how the arm uses the binding.

- **A `ref` or view scrutinee:** every binding is a `ref` into it, and moving a binding is an error (§3.7).
- **An owned scrutinee** (a place rooted at a local or parameter the function owns, or a temporary):
  - A plain binding `name` of a non-`Copy` part **moves** that part out of the scrutinee on that path. That is a partial move (§3.6), or a whole move if the binding covers the whole value. The binding drops at the end of its arm or body unless moved on.
  - `ref name` **borrows** the part instead and leaves the scrutinee intact. `mut ref name` borrows it mutably and requires a mutable scrutinee.
  - **`_` never binds and never moves.** `let _ = x;` leaves `x` intact, and `match x { _ => … }` does not move `x`. `..` likewise moves nothing.
- **A temporary scrutinee:**
  - In `match`, `if let` and `while let`, the temporary lives through the construct (§7.4). Whatever its bindings did not move drops at the end of the construct.
  - In `let`, the temporary ends at the `;`. The parts not bound (by `_` or `..`) drop there with the rest of it, so `let (a, _) = pair();` drops the second element at the `;`.
- **`Copy` bindings** copy.
- **A pattern over a type with a `Drop` body** may bind the whole value, but may not move a part out (§3.7).
- **`for x in c`** borrows `c` through `Iterable`; `x` is the iterator's item, `ref T` for the standard collections. **`for x in c.into_iter()`** moves `c` and each item.

Pins: `ok_match_binding_modes`, `drop_match_scrutinee`, `drop_underscore`.

---

## 5. References and views (C5)

**5.1 Where `ref T` and `mut ref T` may appear:**
- as a parameter type;
- as a return type, or inside one (`Option[ref T]`, a tuple);
- as the type of a local bound to a call that returns a reference, or to a projection of a named place;
- inside a view (§5.2).

**5.2 Views.** A *view* is any type that contains a `ref` or `mut ref` after generic substitution. Examples:
- `Slice[T]` and `StringSlice`;
- iterators over a borrowed collection;
- borrowed structs (structs with `ref` fields);
- `Option[ref T]`;
- `Vec[ref T]`;
- a closure that captures by reference (§9).

A view borrows exactly like a reference.

**5.3 Origins.** Every reference or view value has a set of **origins**: the places it borrows from. Origins are computed inside one function body:
- projecting a place `p` borrows `p`;
- a call result borrows the origins of the arguments that the signature rule (§5.4) selects;
- a literal that builds a view takes the union of its fields' origins;
- branches take the union of their arms.

**5.4 The signature rule.** For a function that returns a reference or a view:
- if it has a `ref self` or `mut ref self` receiver, the result borrows from `self` only;
- otherwise, the result borrows from every `ref`, `mut ref` and view parameter;
- with no such parameter, returning a reference or view is an error.

The body is checked against the rule. A function that needs a different relation must be restructured or return an owned value.

No body-dependent precision is allowed. `design.md` Part 3's tracing of private function bodies is removed, so a function's borrow contract cannot change when its body changes.

Pins:
- `ok_ref_self_wins`: the temporary argument does not limit the result. Today this is "borrow-return form not yet supported".
- `err_ref_from_temp`.

**5.5 Borrows root at named places.**
- A reference or view whose origin is a temporary must not outlive the statement that created the temporary.
- There is no temporary lifetime extension. Bind the temporary to a variable first.
- Pin: `err_ref_from_temp`; today it compiles.

**5.6 Exclusivity inside a body.**
- While a `mut ref` borrow of place `p` is live, no place overlapping `p` may be used except through that borrow.
- While a `ref` borrow of `p` is live, `p` may not be written, moved, mutably borrowed or dropped.
- **Overlap:** a place overlaps its prefixes and its extensions. Distinct fields do not overlap. Any index projection overlaps the whole collection.
- **Liveness:** a borrow is live from its creation to its last use. Unlike drops, this is non-lexical, because when a borrow ends cannot be observed.
- **Method calls:** in a method call with a `mut ref self` receiver, the receiver's borrow starts after the arguments are evaluated. So `v.push(v.len())` is legal (two-phase borrow).

**5.7 Where a view may not go.** A view may not be stored in, or flow into:
- a field of a `shared`, `par` or `frozen` type;
- a global;
- an unstructured task (`spawn`) or a channel;
- an escaping closure (§9.3);
- the function's result, except as §5.4 allows.

A view **may** enter a branch of `par {}`, or a `TaskGroup` task that joins inside the scope of every origin.

**5.8 Views in generic code.** A generic parameter may be instantiated with a view.
- The checks of §5.6–§5.7 run on the monomorphised instance.
- An instance whose body stores a view-typed value somewhere a view may not go is an error. It is reported at the instantiation site, with a note at the generic body's line.

---

## 6. Sharing (C7)

**6.1 `shared struct` and `shared enum`.** These have reference semantics within one task, and their count is not atomic.
- Using a handle as a value (assigning it, passing it, storing it) **increments** the count. The source stays usable.
- Every binding, field or temporary holding a handle **releases** it when that holder is dropped, under §7.
- The object's `Drop` body and fields run when the count reaches zero.
- A struct that *contains* a handle is an ordinary move-only value. Moving it moves the handle without counting; `.clone()` increments.

**6.2 `mut` fields of a shared value carry a borrow flag each.**
- Projecting a `ref` into such a field holds a read borrow of that field while the reference is live. A `mut ref` holds a write borrow.
- Assigning the field, or taking a write borrow, while another borrow of the same field is live is a **panic**.
- A `match` on a `mut` field holds a read borrow for the arm. That is why `design.md`'s "peek-then-mutate" idiom panics; this is unchanged.

**6.3 Across tasks.** Shared handles never cross a task boundary (`E_CONCURRENT_SHARED_STRUCT`).
- `par struct` uses an atomic count, and its `mut` fields must be `Atomic[T]` or `Mutex[T]`.
- `frozen T` is read-only and may be shared by any number of tasks.

**6.4 No implicit sharing.** The compiler never turns an owned value into a shared or atomically counted one. `design.md` Part 4 (RC fallback) and the automatic promotion of Rc to Arc are removed. Where the fallback would have fired, the program has an E0500 (§3.2) or an escaping capture (§9.3).

**6.5 Cycles.** A cycle of strong handles is never freed. `weak` fields break cycles, and reading one yields `Option[T]`. This is unchanged.

---

## 7. Destruction (C6; supersedes the drop judgment §0)

**7.1 What is dropped.** At the end of its scope (§7.2), every holder that is still initialized is dropped. Holders are:
- locals;
- owned parameters (in the callee);
- move-mode pattern bindings;
- temporaries;
- a closure's captured values (dropped with the closure);
- the fields of a value being dropped.

A moved or maybe-moved holder is dropped only on the paths where it is still initialized (§3.5). Drops are not uses: a drop of a maybe-moved place is never E0500. A returned value is moved out of its binding before the scope's drops run, so the returning function does not drop it.

**7.2 When: at scope end, never at last use.** The scopes are:
- a block's closing brace;
- the end of a `match` arm, `if let` body or `while let` body;
- the end of each loop iteration;
- the end of a closure body or function body.

This holds for every value, observable or not; §7.10 is the only freedom.

**Loops.** Each iteration is its own scope. A place moved during an iteration and initialized again in the next one is tracked per iteration, so its drop at the end of each iteration runs only if it is initialized then. `continue` ends the iteration scope exactly as reaching the end of the body does.

Pins: `drop_scope`, `drop_nested`, `drop_loop`. Today they drop at last use; for `drop_scope` that is `d2 use1 d1 end` instead of `use1 end d2 d1`.

**7.3 Order: one LIFO per scope.**
- Bindings and `defer` / `errdefer` blocks are pushed in the order they are introduced, and popped in reverse at scope end.
- Pattern bindings belong to their arm's scope, so they drop before anything outside it.
- Parameters are introduced before the body, in parameter order, so they drop after the body's locals, last parameter first.
- **Shadowing ends nothing.** `let x = a(); let x = b();` leaves both values alive until the scope ends, where the second `x` drops first.

Pins: `drop_defer`, `drop_callee_owns`, `drop_shadowing`.

**7.4 Temporaries.**

| Where the temporary is created | When it drops |
|---|---|
| expression statement `e;` | at the `;` |
| `let` initializer | at the `;` (after the bindings take what they move) |
| argument passed to a `ref` / `mut ref` parameter, borrowed receiver, operator operand or index operand | at the end of the enclosing statement |
| argument passed **by value** | it is not a caller temporary: it moves into the callee, which drops it (§4.2) |
| `if` / `while` condition | after the condition is evaluated, before the branch |
| `match` guard | at the end of the guard |
| `match` / `if let` scrutinee | at the end of the whole construct |
| `while let` scrutinee | at the end of each iteration's body |
| block tail value | after the tail is computed, before the block's locals |
| `return e` | after `e` is computed, before the function's locals and defers |
| `for` iterator | at loop exit |

Several temporaries that drop at the same point drop in reverse order of creation. Literal fields are not temporaries: they move into the literal.

Pins: `drop_temp_stmt`, `drop_match_scrutinee`.

**7.5 Assignment.** For `p = v`:
1. `p`'s operands are evaluated, then `v` (§8.2).
2. The old value of `p` is dropped, if `p` is initialized.
3. The new value is stored.

The same holds for a field or element place. Pin: `drop_assign`.

**7.6 Early exits.** `break`, `continue`, `return` and `?` run the drops and defers of every scope they leave, innermost scope first, each scope in its own LIFO order. Pin: `drop_early_return`.

**7.7 `defer` and `errdefer`.**
- A `defer` block runs when its scope ends, in its LIFO position.
- An `errdefer` block runs in its LIFO position only when the function is exiting through an error return (`?` or `return Err(...)`). Otherwise it is skipped.
- Neither runs on a panic (§10).

Pin: `errdefer_only_on_error`. Today it prints `rollback` before `defer`.

**7.8 Inside a value.**
1. The type's own `Drop` body runs first, with every field still alive.
2. Then the parts drop:
   - **Struct fields:** reverse declaration order.
   - **Tuple elements:** last to first.
   - **Enum:** the active variant's payload, last to first.
   - **`Vec`, `Array`, and slices that own elements:** first index to last.
   - **`Option` / `Result`:** the payload, if present.
   - **`Map` / `Set`:** element order is unspecified (it follows iteration order); within an entry, the key drops before the value.
   - **`SortedMap` / `SortedSet`:** key order.

The asymmetry is deliberate, and it is Rust's choice too. Fixed-shape parts (fields, tuple elements, payloads) mirror construction order, like locals. Collection elements drop in index order, because a collection has no construction order to mirror once elements are pushed, inserted and removed.

Pins: `drop_aggregates`, `drop_user_body_first`.

**7.9 Partially moved values** drop their remaining parts in the §7.8 order, skipping the parts that were moved. Pin: `drop_partial_move`.

**7.10 Optimisation.** The program behaves as if every drop ran exactly where §7.2–§7.9 put it.
- A drop that is not observable (§1.4) is a plain memory free, and it may run at any point after the value's last use.
- An optimiser may never move, add or remove a `Drop` body call.
- An optimiser may remove a matched increment/decrement pair on a shared handle, but only if no count reaches zero at a different point as a result.

**7.11 `Drop` bodies.**
- Form: `fn drop(mut ref self)`.
- A `Drop` body must not panic; the effect checker enforces that.
- Its effects are charged to the scope that performs the drop (§12).

---

## 8. Evaluation order

**8.1 Left to right, everywhere.**
- The operands of a binary operator.
- A call: the callee expression, then the receiver, then the arguments.
- The fields of a struct literal, in **written** order.
- The elements of tuple and array literals.
- An index expression: the base, then the index.
- The interpolations of an f-string.
- A method chain.

Pin: `eval_order`.

**8.2 Assignment.** In `p = v` and `p op= v`:
1. The operands of the place `p` are evaluated once, left to right.
2. Then `v` is evaluated.
3. Then, in order: the bounds check, the read of the current value (for `op=`), the drop of the old value (§7.5), and the store.

Pins: `eval_order`, `ok_compound_assign_once`. Today `=` evaluates `v` first, and `op=` evaluates the index twice.

**8.3 Short circuits.** `&&`, `||` and `?.` evaluate the right-hand side only when it is needed.

**8.4 No observable reordering.** An implementation may not reorder anything a program can observe:
- output and I/O;
- panics;
- `Drop` bodies;
- writes to shared or `mut` state.

Reordering that cannot be observed is allowed.

---

## 9. Closures

**9.1 Capture modes** are inferred per captured place, with disjoint field capture as in `design.md` Rule 2¼:
- **by `ref`** if the body only reads the place;
- **by `mut ref`** if the body mutates it;
- **by move** if the body moves it, or if the closure escapes (§9.3).

`Copy` places are copied. Shared handles are counted.

**9.2 Closures as views.** A closure with any `ref` or `mut ref` capture is a view (§5). It can be called and passed down, but not stored where §5.7 forbids it.

**9.3 Escaping closures.** A closure **escapes** when it, or a value containing it, is:
- returned;
- stored in a struct field, a collection or a global that is not a local view;
- passed to an `escaping` function parameter;
- sent to a task.

An escaping closure captures every place by move. A captured place is then moved (§3), and using it afterwards is E0500. The fix is to clone before creating the closure.

**Function-typed parameters** are non-escaping by default:
- the callee may call such a parameter or pass it to another non-escaping parameter, and nothing else;
- storing or returning it requires the declaration `f: escaping Fn(A) -> R`;
- `spawn` and `TaskGroup.spawn` take escaping closures.

Pins: `err_escaping_capture_reused` (today it compiles and prints both lines), `err_store_nonescaping_param`.

**9.4 When captures drop.**
- A by-move capture moves the captured value into the closure when the closure is created.
- A closure's captures drop when the closure value drops, in reverse capture order, which is the order their names first appear in the body.
- A closure whose body moves a capture out can be called only once (`design.md` § First-Class Functions, "once-callable"). The call consumes the closure: the moved capture goes where the body sends it, and the other captures drop at the end of that call, as the callee's locals would.

---

## 10. Panics and errors (C8)

**10.1 What panics:**
- `panic(...)`;
- a failed bounds check;
- integer overflow in checked arithmetic, which is the default;
- division by zero;
- `unwrap` / `expect` on `None` or `Err`;
- a shared borrow-flag conflict (§6.2);
- a failed assertion;
- allocation failure, unless the profile configures otherwise.

**10.2 What a panic does:**
1. It writes the message, the source location and the error-return trace to stderr.
2. It runs the profile's custom panic handler, if there is one.
3. It ends the process with exit code **101**.

**No** `Drop` body, `defer` or `errdefer` runs, and every other task ends with the process. `catch_panic`, `panic = "unwind"` and `extern "C-unwind"` do not exist.

Pin: `panic_runs_no_drops`. Today the interpreter runs the `defer` before the error message.

**10.3 Errors are values.** `Result` with `?` returns through the normal path, so drops, `defer`s and `errdefer`s run (§7.6–§7.7).

**10.4 Tests.** `karac test` runs each test in its own process, so a panicking test fails alone.

---

## 11. Concurrency (C9)

**11.1 Structured constructs only.** `par { b1 b2 … }`, `TaskGroup` and `spawn` with a scope-local handle. There is no automatic parallelisation of statements.

**11.2 `par {}` branches must not conflict.**
- Two branches conflict when one writes, moves, mutably borrows or drops a place or resource that another reads or writes.
- Resources come from effects (§12).
- `println` has `writes(Stdout)`, so two printing branches conflict.
- A conflict is an error that names both accesses. Disjoint index ranges proven by the existing disjointness analysis do not conflict.

Pin: `err_par_conflict` (already an error today, E0408).

**11.3 What may enter a task:**
- owned values (moved in);
- `frozen` and `par struct` handles;
- views, into structured tasks only (§5.7).

Shared handles may not.

**11.4 Panics** in a task end the process (§10).

**11.5 Automatic loop parallelisation** is an optimisation, and it may change nothing observable:
- the same output;
- the same values, bit for bit, including floating point;
- the same panics, both whether and where.

It applies only where the compiler proves all of that. In particular, it does not reassociate floating-point reductions. It does not parallelise a checked integer reduction whose prefix sums could overflow where the parallel partial sums would not.

**11.6 Data races** cannot happen in a program that compiles: §11.2–§11.3 plus `par struct`'s field rules guarantee it.

---

## 12. Effects: soundness defaults (C10)

The effect system is as in `design.md`, with four defaults made sound:

1. A call whose callee cannot be resolved statically has every effect.
2. An `extern` function must declare its effects; otherwise it has every effect.
3. A drop's effects are those of the dropped type's drop glue, transitively. They are charged to the scope where §7 places the drop.
4. Recursive functions get their effects from a fixpoint over the call-graph strongly connected component, starting empty and iterating to a fixed point. Callees are identified by definition id, never by name.

**Effect polymorphism** keeps only the `with _` pass-through. The effects of a generic call are computed on the monomorphised instance.

---

## 13. Not in the core (C11)

These leave the core and the v2 gate until after parity:
- GPU, tensors, autograd;
- dataframe, columns, Arrow;
- `comptime` beyond `#[derive]`;
- effect polymorphism beyond `with _`;
- layout blocks;
- `dyn`;
- self-hosting;
- the WASM component model;
- SSO.

Plain `wasm32` targets stay.

---

## Appendix A — Where legacy differs (measured on the pins, `karac run --interp`)

| Pin | v2 (this file) | Legacy today |
|---|---|---|
| `drop_scope` | `use1 end d2 d1` | `d2 use1 d1 end` |
| `drop_defer` | `body d2 defer d1` | `d1 d2 body defer` |
| `drop_nested` | `inner d2 outer d1` | `d1 d2 inner outer` |
| `drop_assign` | `a d1 b d2` | `a d1 d2 b` |
| `drop_loop` | `it0 d0 it1 d1 done` | `d0 it0 d1 it1 done` |
| `drop_early_return` | `d2 d1 after` | `d1 d2 after` |
| `drop_callee_owns` | `in1 d9 d1 after` | `d9 in1 d1 after` |
| `drop_callee_returns` | `after d1` | `d1 after` |
| `drop_conditional_move` | `took1 d1 end end d1` | `took1 d1 end d1 end` |
| `drop_aggregates` | `end d11 d10 d9 d8 d7 d6 d5 d4 d1 d2 d3` | `d1 d2 d3 d4 d5 d6 d9 d8 d7 d11 d10 end` |
| `drop_user_body_first` | `end dW d2 d1` | `dW d2 d1 end` |
| `drop_partial_move` | `mid d8 d9 d7` | `d8 d9 d7 mid` |
| `errdefer_only_on_error` | `defer -- defer rollback` | `defer -- rollback defer` |
| `panic_runs_no_drops` | nothing on stdout, exit 101 | `d1 defer`, then the error, exit 101 |
| `eval_order` | `… p7 p1 p8 …` | `… p7 p8 p1 …` |
| `ok_compound_assign_once` | `p1 p8 8` | `p1 p8 p1 8` |
| `ok_match_binding_modes` | `show1 d1 after move-mode match show2 after ref-mode match eat3 d3 end d2` | parse error: `ref` in a pattern is accepted only as `ref name @ PAT` |
| `drop_shadowing` | `end2 d2 d1` | `d1 end2 d2` |
| `drop_underscore` | `d2 mid1 x9 d1 d9` | `d9 d2 mid1 d1 x9`, with an E0500 warning: `let _ = x` is treated as a move |
| `ok_ref_self_wins` | compiles, prints `x` | error E0509, "borrow-return form not yet supported" |
| `err_use_after_move` | E0500 | warning, prints `hi hi` |
| `err_maybe_moved` | E0500 | no diagnostic (an RC note), prints `hi hi` |
| `err_moved_in_loop` | E0500 | no diagnostic, prints `hi hi` |
| `err_move_out_of_ref` | error | warning, deep copy |
| `err_partial_then_whole` | E0500 | warning |
| `err_ref_from_temp` | error | compiles, prints `abc` |
| `err_escaping_capture_reused` | E0500 | compiles, prints `abc 3` |
| `err_store_nonescaping_param` | error | compiles |
| `err_partial_move_drop_type`, `err_par_conflict` | error | error (E0200, E0408) |
| `drop_temp_stmt`, `drop_match_scrutinee`, `ok_reinit` | as legacy | — |

## Appendix B — Open items

- ~~The drop schedule thread's review of §7.~~ Folded in on 2026-10-06 from its rewritten §0 (D1–D9) and its review of DRAFT 1 (binding modes by pattern, `_` and `..`, shadowing, by-value argument temporaries, closure captures, loops, the collection-order note): partial moves are rejected when any type on the path has a `Drop` body; by-value `self` and temporary arguments are callee-owned; drops are not uses; restoring a moved field drops nothing. Its D9 forbade removing any reference-count change; this file allows removing a matched pair when no count reaches zero at a different point, to keep RC elision possible.
- Whether a tail `Err(...)` (a function body ending in an `Err` value without `return`) is an error exit for `errdefer` (§7.7). The reference model assumes it is, as in Zig; §7.7 names only `?` and `return Err(...)`.
- Diagnostic codes for the new errors in §3.7, §5.4–§5.7 and §9.3. Thread A assigns them while implementing C1/C3/C7.
- More pins: one per remaining rule (§5.6 overlap and two-phase borrows, §6.2 borrow-flag panic, §7.4's guard, condition and `while let` rows, §11.5). These come with the corpus extraction.
