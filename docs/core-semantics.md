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

## Amendments (approved 2026-10-07)

Gowtham approved these at 16:53Z as decisions D1, D2, D4 and D6 of the design review (`review/DESIGN_REVIEW_2026-10-07.md`, revised by its §0).

1. **D1, peek-then-mutate** (§6.2). A `mut` field of a shared value whose type is `Copy` or a handle aggregate is read as a value, so `match cur.next { Some(nxt) => cur.next = nxt.next … }` works. For other `mut` fields, a conflict through the same handle is a compile error, and only a conflict through another handle is a runtime check.
2. **D2, closure kinds** (§9.4, §9.6). Function types are `Fn`, `MutFn` or `OnceFn`, so whether a function value may be called twice is known from its type. The capture prefixes `own |x|`, `ref |x|` and `mut ref |x|` are removed.
3. **D4, failure in `par {}`** (§11.7). No branch is cancelled. Every branch runs to completion, and the source-earliest error is returned. `Cancellable`, `#[derive(Cancellable)]` and `collect_all*` are removed. Cooperative cancellation at I/O calls is planned for M4.
4. **D6, sends and receives keep their order** (§11.2, §12). Two sends to the same resource conflict, and so do two receives. A resource rooted at a value (a channel, a connection, a file) is keyed by that value.
5. **Clarification: only bindings move** (§4.6). A pattern that binds nothing by value moves nothing.
6. **Follows from the review's batch and D6, adopted at the same time** (§4, §5.7, §6.3, §9.3, §9.5, §11.1–§11.3, §11.6, §11.7, §12, §13).
   - There is no free `spawn`. Tasks start only in `par {}`, `par for` and `TaskGroup.spawn`.
   - `par { e1, e2 }` is a comma-separated list of branch expressions whose value is the tuple of their values, and `par for x in it { body }` is one branch per element, whose value is a `Vec`.
   - `TaskGroup.spawn` borrows its receiver (`self`), so a group can be passed borrowed to the functions that spawn into it.
   - The `with _` annotation is removed: a non-escaping function parameter's effects come from the argument at each call.
   - `par struct` and `par enum` are renamed `sync struct` and `sync enum`, since `par` now names the fork-join constructs.
7. **Consistency with the design.md rewrite** (§1.1, §4.6, §4.7, §5.2, §5.3, §5.10, §6.1, §6.3, §7.4, §7.7, §7.11, §8.3, §9.1, §10), from the review thread's answers in `review/design-rewrite/DECISIONS.md`.
   - A `Fn` closure whose captures are all `Copy` is `Copy`.
   - `for x in e` consumes an `Iterator` and borrows anything else through `Iterable`.
   - §4.7's example uses D8's `Add[Rhs = Self]` with `Output`.
   - `StringSlice` is `Str`. A view of static data, such as a string literal typed `Str`, has no origins.
   - A `let … else` initializer's temporaries drop before the `else` block.
   - A `Drop` body may panic, since a panic aborts and `panics` is default-permitted.
   - The short-circuit operators are `and`, `or` and `??`.
   - `sync` fields are never `mut`; they must be `CrossTask`, and mutation goes through `Atomic[T]` and `Mutex[T]`. One error code, `E_NOT_CROSS_TASK`, covers any value that may not enter a task.
   - A `frozen` handle is not counted, so a closure that captures one is a view (§5.7).
   - A handle passed to a borrowed parameter is borrowed and not counted (§6.1). A deadlocked `recv` on a single-threaded target panics (§10.1).
   - Over a borrowed scrutinee a `Copy` part binds by copy (§4.6), and a `ref` to a `Copy` value is read wherever the value is expected (§5.10). Both matter more under D5, where most parameters are borrowed.
   - `errdefer` runs on every failure return, a tail `Err(...)` and `return None` included (§7.7). The panic handler is enabled by `#[panic_handler]` alone (§10.2).
8. **D5, borrow-by-default parameters** (Gowtham, 2026-10-07 18:26Z) (§3.1, §3.6, §4.1–§4.3, §4.7, §5.4, §5.9, §7.4, §9.5, §9.6). A bare parameter `x: T` borrows (for a `Copy` type it is a copy); `x: own T` is owned and the callee drops it; `x: mut ref T` is unchanged. Receivers are `self` (borrowed), `own self` and `mut ref self`. Only the spelling changes: every parameter still has one declared mode, and §4.2's rule for owned parameters is unchanged. The pins keep the earlier spelling until the `karac fix` migration rewrites them.

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
- `Array[T, N]`, tuples, `Option[T]` and `Result[T, E]` when their parts are `Copy`. (`Result` added by Gowtham, 2026-10-09.)
- User types with `#[derive(Copy)]`, which the compiler checks field by field.
- `ref T`, but not `mut ref T`. Copying a shared reference gives a second reference to the same place, with the same origins (§5.3). So `let u = x;` with `x` bound by `ref` makes `u` a reference too, and moves nothing.
- A closure of kind `Fn` (§9.6) whose captures are all `Copy`, `ref` captures included. A `MutFn` or `OnceFn` closure never is.
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
- an argument to an `own` parameter;
- an `own self` (consuming) receiver;
- a `return` or a block's tail value;
- a field or element of a struct, tuple, array or enum literal;
- a capture by an escaping closure (§9);
- a pattern binding in move mode (§4.6);
- `for x in c.into_iter()`.

Whether an argument moves is decided only by the callee's declared parameter mode.
An operator is a call to the method its operand type's impl provides, so the same rule applies: the operands' modes are those declared by the impl method the operator resolves to (§4.7). `String`'s `+` is `fn add(self, other: String) -> String`, which borrows both, so `a + b` on `String` moves neither operand. Comparison operators borrow both. On `Copy` types the question does not arise.

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

**3.6 Partial moves (C4).** A non-`Copy` field, tuple element or enum payload may be moved out of an **owned local, pattern binding or `own` parameter**, unless the root's type, or any type on the path from the root to the moved part, has a `Drop` body.
- After a partial move, the root cannot be used, borrowed or moved as a whole. Its remaining fields still can.
- Assigning a value back into the moved field restores it, without dropping anything. The root is usable as a whole again once every moved part is restored.
- The fields that were not moved drop at the root's scope end (§7.9).
- Pins: `drop_partial_move`, `err_partial_then_whole`.

**3.7 Moves that are errors (C3).** It is an error to move a non-`Copy` value out of a place rooted at:
- a `ref` or `mut ref` parameter or binding, or a view (§5);
- a `shared`, `sync` or `frozen` value (§6);
- an index projection `v[i]` (already `E_INDEX_MOVE_NON_COPY`);
- a field of a type with a `Drop` body.

**The fixes:** `.clone()`, `mem.take(place)` (for `T: Default`), `mem.replace(place, value)`, `mem.swap(a, b)`, `Option.take()`, or `for x in c.into_iter()`.

Pins:
- `err_move_out_of_ref`: today a `borrow_projection_copy` warning plus a deep copy.
- `err_partial_move_drop_type`.

---

## 4. Parameters, calls and patterns

**4.1 Modes are declared and unchanged.**
- Parameter forms: bare `T` (borrowed; a copy for a `Copy` type), `own T` (owned), `mut ref T`, `Slice[T]`, `mut Slice[T]`. `ref T` in a parameter is a redundant spelling of bare `T`; `karac fix` removes it.
- Receivers: `self` (borrowed), `own self`, `mut ref self`.
- An argument to a borrowed parameter of type `T` may be a value or place of type `T`, or a `ref T` or `mut ref T`. The callee sees a read-only borrow for the call. Nothing moves and no handle is counted (§6.1); a temporary argument drops at the end of the caller's statement (§7.4).
- An `escaping` function-typed parameter (§9.3) is owned.
- Call sites carry `mut` markers (`design.md` Part 1½).
- `ref` is never written at a call site.

**4.2 Owned parameters belong to the callee (C2).**
- An owned parameter (`own T`, including `own self`) is a local of the callee's outermost scope, declared before the body in parameter order.
- The callee drops it at its scope end (§7.3), unless it moves it on.
- The caller has moved the argument (§3.1) and never touches it again. A temporary passed as an argument also belongs to the callee.
- A conditional move inside the callee uses a drop flag inside the callee.

This replaces D3.4 of the drop judgment (the caller dropping the argument when the call returns). In ordinary code the printed order is the same; what changes is who emits the drop.

Pins: `drop_callee_owns`, `drop_callee_returns`.

**4.3 Receivers on shared types.** A `shared` type's methods take a borrowed `self` only. This is unchanged apart from the spelling.

**4.4 Return values** move to the caller.

**4.5 Argument evaluation.** Arguments are evaluated left to right, after the callee expression and the receiver (§8).

**4.6 Pattern binding modes.** This covers `match`, `if let`, `while let`, `let` destructuring and `for` patterns. The mode of every binding is decided by the pattern and the scrutinee's type, never by how the arm uses the binding.

- **A `ref` or view scrutinee:** a binding of a non-`Copy` part is a `ref` into it, and moving it is an error (§3.7). A binding of a `Copy` part copies it, so `match shape { Circle { radius } => radius * radius, … }` on a borrowed `shape` needs no `*`.
- **A `shared` scrutinee** (a handle, or any place reached through one) counts as a `ref` scrutinee. A shared value's contents are only ever reached through a handle (§6.1), so nothing can move out of them. A plain binding of a part is therefore a `ref` automatically, and moving it is an error whose fix is `.clone()`. A binding that covers the whole handle copies the handle (§6.1). This is unlike a type with a `Drop` body (below), where a plain binding of a part still needs an explicit `ref`. A `mut` field of `Copy` or handle-aggregate type is the exception: it is read as a value (§6.2), so the scrutinee is a temporary.
- **An owned scrutinee** (a place rooted at a local or parameter the function owns, or a temporary):
  - A plain binding `name` of a non-`Copy` part **moves** that part out of the scrutinee on that path. That is a partial move (§3.6), or a whole move if the binding covers the whole value. The binding drops at the end of its arm or body unless moved on.
  - `ref name` **borrows** the part instead and leaves the scrutinee intact. `mut ref name` borrows it mutably and requires a mutable scrutinee.
  - **`_` never binds and never moves.** `let _ = x;` leaves `x` intact, and `match x { _ => … }` does not move `x`. `..` likewise moves nothing.
  - **Only bindings move.** A pattern that binds nothing by value, such as a unit variant (`if let Level.Info = lv`), a literal, a range, or a variant whose sub-patterns are only `_`, `..` and `ref` bindings, tests the scrutinee and leaves it intact. So does a whole `match` whose arms bind nothing by value.
- **A temporary scrutinee:**
  - In `match`, `if let` and `while let`, the temporary lives through the construct (§7.4). Whatever its bindings did not move drops at the end of the construct.
  - In `let`, the temporary ends at the `;`. The parts not bound (by `_` or `..`) drop there with the rest of it, so `let (a, _) = pair();` drops the second element at the `;`.
- **`Copy` bindings** copy, whatever the scrutinee.
- **A pattern over a type with a `Drop` body** may bind the whole value, but may not move a part out (§3.7).
- **`for x in e`** consumes `e` when its type is an `Iterator`: `for x in c.into_iter()` moves `c` and each item. Otherwise it borrows `e` through `Iterable`, and `x` is the iterator's item, `ref T` for the standard collections.

Pins: `ok_match_binding_modes`, `drop_match_scrutinee`, `drop_underscore`, `ok_pattern_binds_nothing`.

**4.7 An impl's modes may be weaker than its trait's.** An impl method may declare any parameter, the receiver included, with the same mode as the trait method or a weaker one, in the order `own` → `mut ref` → borrowed (and `mut Slice[T]` → `Slice[T]`). It may never declare a stronger one.
- A call whose impl is known where it is checked (a concrete type, including an operator on one) uses the impl's modes.
- A call through a generic bound is checked against the trait's modes, so an argument the trait takes `own` is moved there. When the instance's impl borrows it instead, the moved value becomes a temporary of the call and drops at the end of the enclosing statement (§7.4).
- So `trait Add[Rhs = Self] { type Output; fn add(own self, rhs: own Rhs) -> Self.Output; }` and `String`'s `fn add(self, other: String) -> String` conform. `a + b` on two `String` locals moves neither (§3.1). Inside `fn sum[T: Add[Output = T]](a: own T, b: own T) -> T { a + b }`, both move.

---

## 5. References and views (C5)

**5.1 Where `ref T` and `mut ref T` may appear:**
- as a parameter type;
- as a return type, or inside one (`Option[ref T]`, a tuple);
- as the type of a local bound to a call that returns a reference, to a projection of a named place, or to another reference;
- inside a view (§5.2).

**5.2 Views.** A *view* is any type that contains a `ref` or `mut ref` after generic substitution. Examples:
- `Slice[T]` and `Str`;
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
- branches take the union of their arms;
- a call with a `mut ref` argument (a `mut ref self` receiver included) whose pointee can hold a reference may store the other reference and view arguments into it, so the pointee's origins grow by theirs. `v.push(r)` on a `Vec[ref T]` is the common case.

**Static data has no origins.** A view of data that lives for the whole program, such as a string literal typed `Str` or a `Slice` of a constant, borrows nothing, so none of §5.5–§5.7 limits it.

Only a value whose type can hold a reference has origins: a reference, a view, a closure, and an aggregate or collection containing one. A type the compiler cannot see into counts as one that can; `TaskGroup` is such a type, because it holds its tasks' closures. So an `i64` copied out through a reference borrows nothing.

**5.4 The signature rule.** For a function that returns a reference or a view:
- if it has a `self` or `mut ref self` receiver, the result borrows from `self` only;
- otherwise, the result borrows from every borrowed parameter of a non-`Copy` type, every `mut ref` parameter and every view parameter;
- with no such parameter, returning a reference or view is an error.

The body is checked against the rule. A function that needs a different relation must be restructured or return an owned value.

**Items borrow what the receiver borrows.** For a method with a `ref self` or `mut ref self` receiver whose result type contains a reference only through a type parameter of the impl or an associated type (`T`, `Self.Item`, `I.Item`), and not through a `ref` written in the signature or a named type that declares a `ref` field, the result borrows what `self`'s value borrows (its origins, §5.3), not the receiver place. So two `it.next()` results can be live together, and both borrow the collection `it` iterates; `last`, `nth` and `find` can return items. The body is checked against this: the result may hold only references reached through `self`'s own `ref` fields, so an iterator whose items point into itself (a lending iterator) is an error at the return. A `ref` written in the signature keeps the rule above: `peek(mut ref self) -> Option[ref Self.Item]` borrows `self`. (Decided by Gowtham, 2026-10-09.)

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
- **Calls (two-phase borrows):** in any call, the borrow for a `mut ref` argument, a `mut ref self` receiver included, starts after every argument is evaluated. So `v.push(v.len())` and `insert(nodes, nodes[root].left, v)` are legal. A later argument may read the place; one whose value still borrows the place when the call starts is an error. (Extended from receivers to every `mut ref` argument by Gowtham, 2026-10-08.)

**5.7 Where a view may not go.** A view may not be stored in, or flow into:
- a field of a `shared`, `sync` or `frozen` type;
- a global;
- a channel;
- an escaping closure (§9.3);
- the function's result, except as §5.4 allows.

A view **may** enter a branch of `par {}` or `par for`, or a `TaskGroup` task that joins inside the scope of every origin (§9.5).

**5.8 Views in generic code.** A generic parameter may be instantiated with a view.
- The checks of §5.6–§5.7 run on the monomorphised instance.
- An instance whose body stores a view-typed value somewhere a view may not go is an error. It is reported at the instantiation site, with a note at the generic body's line.

**5.9 Nothing is written through a `ref`.** A place whose path dereferences a `ref` is read-only. That covers a borrowed parameter or receiver (bare `T`, `self`), a `ref` pattern binding, the item of `for x in c` (§4.6), a `ref` returned by a call, and a local bound to any of these. Such a place may not be:
- assigned or compound-assigned;
- mutably borrowed: a `mut ref self` receiver, an argument marked `mut`, or a `mut ref name` binding;
- moved out of (§3.7).

**The exception** is a `mut` field of a `shared` value. It is writable through any handle, however the handle was reached, under §6.2's flags; that is why a shared type's methods can take a borrowed `self` (§4.3).

**The fixes:** declare the parameter `mut ref T` and mark its call sites `mut`; iterate with `c.iter_mut()`; or bind with `mut ref name` from a mutable owned scrutinee.

Pins: `err_write_through_ref`, `ok_shared_field_through_ref`.

**5.10 Reading a `Copy` value through a reference.** A `ref T` or `mut ref T` whose `T` is `Copy` is read as a `T` wherever a `T` is expected: an operator operand, an argument, a `let` with a `T` annotation, a field or element of a literal, a return value. Nothing is moved and the reference stays usable. `*r` writes the read explicitly. So `for x in v { total += x }` over a `Vec[i64]` needs no `*`, and neither does a bare parameter of a `Copy` type, which is already a copy (§4.1).

---

## 6. Sharing (C7)

**6.1 `shared struct` and `shared enum`.** These have reference semantics within one task, and their count is not atomic.
- Using a handle as a value (assigning it, passing it to an `own` parameter, storing it, returning it) **increments** the count, including when the handle is read from a borrowed place. The source stays usable. Passing a handle to a borrowed parameter borrows it and counts nothing.
- Every binding, field or temporary holding a handle **releases** it when that holder is dropped, under §7.
- The object's `Drop` body and fields run when the count reaches zero.
- An `Option`, a `Result` or a tuple whose parts are only handles, Copy values, and further such `Option`s, `Result`s and tuples (`Option[Node]`, `(Node, i64)`, `Option[(Node, Node)]`, `Result[Node, i64]`) is a **handle aggregate**. It is duplicated the same way: using it as a value increments every handle inside it, the source stays usable, and each copy releases its own handles when dropped. So `cur = node.next` walks a list without `.clone()` (decision 2026-10-06).
- Any other type that contains a handle, a user `struct` or `enum` or a `Vec[Node]`, is an ordinary move-only value. Moving it moves the handle without counting; `.clone()` increments.

**6.2 `mut` fields of a shared value** (amended 2026-10-07, D1).
- **`Copy` and handle-aggregate fields are read as values.** Reading a `mut` field whose type is `Copy` or a handle aggregate (§6.1) copies it, or counts its handles. That includes reading it as a `match`, `if let` or `while let` scrutinee. No borrow is held, so the field may be assigned while the copy is in use. So `match cur.next { Some(nxt) => { cur.next = nxt.next; } None => {} }` is legal: the arm works on a counted copy of the field. This is the `cur = node.next` rule of §6.1, applied to `match`.
- **Other `mut` fields are borrowed.** This covers a `Vec`, a `String`, a user struct, or any type that is neither `Copy` nor a handle aggregate.
  - Projecting a `ref` into such a field holds a read borrow of it while the reference is live.
  - A `mut ref`, including a `mut ref self` method call on the field, holds a write borrow.
  - A `match` or `for` over the field holds a read borrow for the whole construct.
- **A conflict through the same handle is a compile error.** Suppose the conflicting access reaches the field through the same handle place as the live borrow: the same local, parameter or `self`, not reassigned in between. Then writing the field, or taking a write borrow of it, while the borrow is live is an error. The borrow checker finds it as in §5.6. The common case is pushing to `self.items` inside `for x in self.items`.
- **A conflict through another handle is a runtime check.** Two different handles may name the same object, and the compiler cannot always see that. So each `mut` field of a type that is neither `Copy` nor a handle aggregate carries a borrow flag. A conflicting access through another handle is a **panic** (§10.1).

Pins: `ok_shared_peek_then_mutate`, `err_shared_field_same_handle`, `panic_shared_field_alias`.

**6.3 Across tasks.** Shared handles never cross a task boundary (`E_NOT_CROSS_TASK`).
- `sync struct` and `sync enum` (formerly `par struct` and `par enum`) use an atomic count. Their fields are never `mut` and must be `CrossTask`; mutation goes through `Atomic[T]` and `Mutex[T]`, whose methods borrow.
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
| argument passed to a borrowed or `mut ref` parameter or receiver, including an operator operand or index operand passed that way | at the end of the enclosing statement |
| argument passed to an **`own`** parameter | it is not a caller temporary: it moves into the callee, which drops it (§4.2) |
| `if` / `while` condition | after the condition is evaluated, before the branch |
| `let … else` initializer | before the `else` block runs, or at the `;` when the pattern matches |
| `match` guard | at the end of the guard |
| `match` / `if let` scrutinee | at the end of the whole construct |
| `while let` scrutinee | at the end of each iteration's body |
| block tail value | after the tail is computed, before the block's locals |
| `match` arm without braces (`p => e`) | as a block tail: after `e` is computed, at the arm's end |
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
- An `errdefer` block runs in its LIFO position only when the function returns the failure variant of its `Result` or `Option` return type, however that value was produced: `?`, `return Err(...)`, `return None`, or a tail `Err(...)`. Otherwise it is skipped.
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
- A `Drop` body may panic. The panic ends the process like any other (§10.2); `panics` is default-permitted (§12).
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

**8.3 Short circuits.** `and`, `or` and `??` evaluate the right-hand side only when it is needed.

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

A by-move capture of a `Copy` place copies it. A read-only capture of a `Copy` place is still a `ref` capture, so writing the place while the closure is live is an error (§5.6), never a silent read of a stale copy: in `for x in xs.iter().filter(|v| v < lim) { lim = lim - 1; }` the write to `lim` is rejected. To give the closure a snapshot, copy the value into a new binding first. (Decided by Gowtham, 2026-10-09.) `shared` and `sync` handles are counted. A `frozen` handle is never counted, so a closure that captures one is a view (§5.2, §5.7).

**9.2 Closures as views.** A closure with any `ref` or `mut ref` capture is a view (§5). It can be called and passed down, but not stored where §5.7 forbids it.

**9.3 Escaping closures.** A closure **escapes** when it, or a value containing it, is:
- returned;
- stored in a struct field, a collection or a global that is not a local view;
- passed to an `escaping` function parameter;
- sent through a channel.

An escaping closure captures every place by move. A captured place is then moved (§3), and using it afterwards is E0500. The fix is to clone before creating the closure.

**Function-typed parameters** are non-escaping by default:
- the callee may call such a parameter or pass it to another non-escaping parameter, and nothing else;
- storing or returning it requires the declaration `f: escaping Fn(A) -> R`;
- there is no free `spawn`. Tasks start only in `par {}`, `par for` and `TaskGroup.spawn`, and none of them takes an escaping closure (§9.5, §11.1).

Pins: `err_escaping_capture_reused` (today it compiles and prints both lines), `err_store_nonescaping_param`.

**9.4 When captures drop.**
- A by-move capture moves the captured value into the closure when the closure is created.
- A closure's captures drop when the closure value drops, in reverse capture order, which is the order their names first appear in the body.
- A closure whose body moves a capture out has kind `OnceFn` (§9.6). Calling it consumes the closure: the moved capture goes where the body sends it, and the other captures drop at the end of that call, as the callee's locals would.

**9.5 `TaskGroup` tasks borrow.** The closure passed to `TaskGroup.spawn` captures as §9.1 infers: by `ref`, by `mut ref` or by move, place by place. It is not escaping.
- `spawn` borrows its receiver (`self`), so a group can be passed borrowed to the functions and tasks that spawn into it, such as a server's request handlers. Spawns on one group are synchronized.
- The group then borrows the closure's origins (§5.3) until it drops. Dropping a group joins its tasks, so the drop is the borrow's last use (§5.6). The returned `TaskHandle` borrows nothing; it owns its task's result.
- So while the group lives, a captured place may not be written, moved or dropped, and a second task may not capture by `mut ref` a place another task already captured.
- Every origin must outlive the group. A place declared after the group in the same scope drops before it (§7.3), which is an error. Declare the place first, or put the group in an inner block.
- So a task spawned from inside another task (a request handler, say) can capture that task's own locals only by move: they do not outlive the group.
- This is §5.3's rule for a method that stores a reference in its receiver. Nothing about `TaskGroup` is special except that it holds references through a `ref` receiver.

Pins: `ok_taskgroup_borrows`, `err_taskgroup_write_while_borrowed`, `err_taskgroup_origin_declared_after`.

**9.6 Function types have a kind** (2026-10-07, D2).
- **`Fn(A) -> R`** may be called any number of times, through any access, `ref` included. Its call only reads the captures.
- **`MutFn(A) -> R`** may be called any number of times, and a call may mutate its captures. It can be called only through an access that is unique: an owned binding, an `own` parameter or a `mut ref`. A shared borrow of a `MutFn` (a `ref`, or a bare parameter) cannot call it, which rules out a call re-entering the same closure.
- **`OnceFn(A) -> R`** may be called once. The call moves the value (§3), so a second call is E0500.
- **Kinds of closures and functions.**
  - A closure literal gets the most permissive kind its body allows: `Fn` if it only reads its captures, `MutFn` if it mutates one, `OnceFn` if it moves one out.
  - A named function is an `Fn`.
  - An `Fn` may be passed where a `MutFn` or `OnceFn` is expected, and a `MutFn` where an `OnceFn` is expected. Passing a closure where a more permissive kind is expected is an error that names the capture responsible.
- **Orthogonal parts.** The kind is independent of `escaping` (§9.3) and of the effect clause, so `escaping MutFn(Event) with writes(Log)` is one type.
- **Removed.** The capture prefixes `own |x|`, `ref |x|` and `mut ref |x|` are removed; capture modes are §9.1's. Write `.clone()` before the closure to give it its own copy.

Pins: `ok_closure_kinds`, `err_once_called_twice`, `err_fn_kind_mismatch`.

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
- allocation failure;
- a channel `recv` that can never complete because no other task can run (deadlock, on a single-threaded target).

**10.2 What a panic does:**
1. It writes the message, the source location and the error-return trace to stderr.
2. It runs the program's custom panic handler (`#[panic_handler]`), if there is one. A panic inside the handler ends the process at once.
3. It ends the process with exit code **101**.

**No** `Drop` body, `defer` or `errdefer` runs, and every other task ends with the process. `catch_panic`, `panic = "unwind"` and `extern "C-unwind"` do not exist.

Pin: `panic_runs_no_drops`. Today the interpreter runs the `defer` before the error message.

**10.3 Errors are values.** `Result` with `?` returns through the normal path, so drops, `defer`s and `errdefer`s run (§7.6–§7.7).

A `main` that returns `Err(e)` is an ordinary error exit: `main`'s drops, `defer`s and `errdefer`s run first, then `e` is written to stderr and the process exits with code **1**. Code 101 stays reserved for a panic, so a script can tell the two apart.

**10.4 Tests.** `karac test` runs each test in its own process, so a panicking test fails alone.

---

## 11. Concurrency (C9)

**11.1 Structured constructs only.**
- `par { e1, e2, … }` runs its comma-separated branch expressions concurrently; its value is the tuple of their values. A branch may be a block.
- `par for x in it { body }` runs one branch per element; its value is the `Vec` of the bodies' values, in iteration order. It may take a limit on the number of branches in flight (`design.md`).
- `TaskGroup` runs tasks that join when the group drops (§9.5).
- There is no free `spawn`.
- v1 has no automatic parallelisation of statements; it returns in M4 under §11.5's rule (redesign amendment, 2026-10-07).

**11.2 `par {}` branches must not conflict.**
- Two branches conflict when one writes, moves, mutably borrows or drops a place or resource that another reads or writes.
- Resources come from effects (§12).
- `println` has `writes(Stdout)`, so two printing branches conflict.
- Two branches that both `sends` to the same resource conflict, and so do two that both `receives` from it, because messages on one channel or connection have an order (D6, 2026-10-07). A branch that sends and one that receives on the same resource do not conflict.
- The iterations of a `par for` are branches of one block, so two iterations conflict by the same rule.
- A conflict is an error that names both accesses. Disjoint index ranges proven by the existing disjointness analysis do not conflict.

Pin: `err_par_conflict` (already an error today, E0408).

**11.3 What may enter a task:**
- owned values (moved in);
- `frozen` and `sync` handles;
- views, into `par {}` branches and `TaskGroup` tasks only (§5.7, §9.5).

Shared handles may not.

**11.4 Panics** in a task end the process (§10).

**11.5 Automatic parallelisation** of loops, and of statements from M4, is an optimisation, and it may change nothing observable:
- the same output;
- the same values, bit for bit, including floating point;
- the same panics, both whether and where.

It applies only where the compiler proves all of that. In particular, it does not reassociate floating-point reductions. It does not parallelise a checked integer reduction whose prefix sums could overflow where the parallel partial sums would not.

**11.6 Data races** cannot happen in a program that compiles: §11.2–§11.3 plus the field rules of `sync` types guarantee it.

**11.7 Failure in `par {}`** (2026-10-07, D4).
- **`?` in a branch** keeps `design.md`'s block-piercing meaning. It ends that branch, running the branch's drops, `defer`s and `errdefer`s as an error exit (§7.6–§7.7). The error is returned from the enclosing function once the block is done.
- **No branch is cancelled.** Every other branch runs to completion.
- **When the block is done:**
  - If any branch ended with an error, the enclosing function returns the **source-earliest** one (for `par for`, the earliest in iteration order). That holds whichever failed first, so the result is deterministic. The block's other values, errors included, drop in reverse source order as the block is left (§7.6).
  - Otherwise the block has its value, as `design.md` defines it.
- **A panic** in any branch ends the process (§11.4).
- **Removed.** `Cancellable`, `#[derive(Cancellable)]`, `collect_all` and `collect_all_vec` are removed. A block whose branches produce `Result` values without `?` already returns every result, in source order.
- **Planned for M4, not yet core:** cooperative cancellation. Once a branch fails or a deadline passes, a sibling's next I/O or suspending call returns `Err(Cancelled)`, which converts into the branch's error type through `From` and propagates with `?`. Pure computation is never interrupted.

Pin: `par_error_runs_siblings`.

---

## 12. Effects: soundness defaults (C10)

The effect system is as in `design.md`, with four defaults made sound:

1. A call whose callee cannot be resolved statically has every effect.
2. An `extern` function must declare its effects; otherwise it has every effect.
3. A drop's effects are those of the dropped type's drop glue, transitively. They are charged to the scope where §7 places the drop.
4. Recursive functions get their effects from a fixpoint over the call-graph strongly connected component, starting empty and iterating to a fixed point. Callees are identified by definition id, never by name.
5. A resource rooted at a value, such as `sends(tx)` or `writes(self.cache)`, is keyed by that value for conflict checking (D6, 2026-10-07). Two channels, two connections or two files are two resources. When the compiler cannot tell whether two such values are the same, they conflict. A program-wide resource such as `Network` stays a capability ("may use the network"), not one shared key, so two calls on separate connections do not conflict.

**Effect polymorphism needs no annotation** (D6, 2026-10-07).
- A call through a non-escaping function-typed parameter has the effects of the argument passed at each call site, and they are charged to that caller.
- The effects of a generic call are computed on the monomorphised instance.
- A call through an escaping function value has the effects its type declares (`escaping Fn(Request) -> Response with reads(Db)`); storing a value whose effects exceed them is an error. With no effect clause, a call through it has every effect (default 1). Effect variables for stored callbacks arrive with the services track.
- The `with _` annotation is removed.

---

## 13. Not in the core (C11)

These leave the core and the v2 gate until after parity:
- GPU, tensors, autograd;
- dataframe, columns, Arrow;
- `comptime` beyond `#[derive]`;
- named effect variables (`with E`);
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
- ~~Whether a tail `Err(...)` is an error exit for `errdefer` (§7.7).~~ Settled 2026-10-07: it is, as is `return None` (§7.7).
- Diagnostic codes for the new errors in §3.7, §5.4–§5.7 and §9.3. Thread A assigns them while implementing C1/C3/C7.
- More pins: one per remaining rule (§5.6 overlap and two-phase borrows, §7.4's guard, condition and `while let` rows, §11.5, §12 item 5). These come with the corpus extraction. §6.2 has its pins as of 2026-10-07.
