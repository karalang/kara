# MIR types (M1)

**Status:** DRAFT, 2026-10-06. Owner: the Shared drop schedule thread (MIR types in M1, then drop elaboration and the borrow checker). Part of the redesign approved on 2026-10-06 (`review/REDESIGN_PROPOSAL_2026-10-06.md` in the project folder). The semantics MIR must implement are in `docs/core-semantics.md` (approved 2026-10-06); §7 (Destruction) is the drop elaboration spec. This note fixes the data types and the invariants each MIR phase guarantees, so that the builder (typed HIR to MIR), drop elaboration, the borrow checker, the MIR interpreter and MIR to LLVM can be written against one definition.

## 1. Where MIR sits

```
source → parse → resolve → typecheck → typed HIR (NodeId, DefId, interned Ty)
       → monomorphise → MIR build (per function instance)
       → MIR checks: move check, borrow check
       → drop elaboration
       → MIR interpreter (M1, the reference)   |   MIR → LLVM (M2)
```

- **One MIR body per monomorphic function instance.** MIR is built after monomorphisation, so it contains no type parameters, no associated-type projections and no inference variables. The validator rejects any of them.
- **The switch is whole-program.** C2 (callee-owned by-value parameters) changes the parameter ABI, so a function compiled through MIR cannot call, or be called by, one compiled by the legacy backend. There is no per-function fallback.
- **MIR is the only input to both backends.** Neither backend reads HIR or the AST. Every decision about moves, drops, drop flags and temporaries is made once, in MIR, and both backends execute it without re-deriving it. That is the property the legacy backends lacked.

## 2. Model: rustc's MIR, kept small

The design follows rustc's MIR (a control-flow graph of basic blocks over typed locals and places) with the parts Kāra does not need left out: no unwind edges in use (panic aborts, C8; `Call` and `Drop` carry a slot for one, below), no lifetimes or regions in types, no two-phase borrows in M1, no coroutines, no inline assembly, no `Box` special-casing.

### 2.1 Body

```rust
pub struct Body {
    pub instance: InstanceId,          // the monomorphic function this body implements
    pub locals: IndexVec<Local, LocalDecl>,
    pub arg_count: usize,              // locals 1..=arg_count are the parameters
    pub blocks: IndexVec<BasicBlock, BasicBlockData>,
    pub scopes: IndexVec<SourceScope, SourceScopeData>, // lexical scopes, for names and debug info
    pub phase: MirPhase,
    pub span: Span,
}
```

- `Local(0)` is the return place. Locals `1..=arg_count` are the parameters, in declaration order. A by-value parameter is an ordinary owned local of the callee (core semantics D6): the callee moves out of it or drops it like any other local.
- `BasicBlock(0)` is the entry block.

### 2.2 Locals

```rust
pub struct LocalDecl {
    pub ty: Ty,                 // interned, monomorphic
    pub mutability: Mutability, // Not | Mut
    pub kind: LocalKind,
    pub source_info: SourceInfo,
}

pub enum LocalKind {
    ReturnPlace,
    Arg { name: Symbol, node: NodeId },
    User { name: Symbol, node: NodeId }, // a `let` or pattern binding
    Temp,                                // compiler temporary
    DropFlag,                            // bool introduced by drop elaboration
}
```

A source binding gets one local. A binding shadowed by a later `let` is a different local. A binding inside a loop body is the same local on every iteration, with `StorageLive` / `StorageDead` marking each iteration's lifetime.

### 2.3 Places

```rust
pub struct Place {
    pub local: Local,
    pub projection: ProjList, // interned slice of ProjElem
}

pub enum ProjElem {
    Field(FieldIdx, Ty),      // struct field or tuple element, with the field's type
    Downcast(VariantIdx),     // view an enum place as one variant, before Field
    Deref,                    // through a reference
    Index(Local),             // array or slice element at a runtime index (usize local)
    ConstIndex(u64),          // array element at a constant index
}
```

A place names memory: a local, then a path of projections. `x.a.0` is `Place { local: x, projection: [Field(a), Field(0)] }`, and the payload of `E.A(r)` is `[Downcast(A), Field(0)]`.

**Index projections are read-and-borrow only.** Core semantics C3 forbids moving out of an index, so the validator rejects `Operand::Move` of any place whose projection contains `Index` or `ConstIndex`. Moving out through `Deref` is rejected the same way: a place behind a reference is never owned by this function. `Vec`, `Map` and the other library collections are not indexed by projection at all; their element access is a `Call` to a library function returning a reference.

### 2.4 Operands and constants

```rust
pub enum Operand {
    Copy(Place), // the type is Copy; the source stays initialized
    Move(Place), // the source becomes uninitialized
    Const(Const),
}

pub struct Const { pub ty: Ty, pub kind: ConstKind }
pub enum ConstKind {
    Scalar(ScalarInt),       // ints, uints, bool, char, with the width from `ty`
    Float(FloatBits),
    Str(Symbol),             // a static string literal
    Unit,
    FnDef(InstanceId),       // a function item, the callee of a direct Call
    ZeroSized,               // any other zero-sized value
}
```

`Copy` is only legal on a type that is `Copy`. Every other use of a non-`Copy` value is a `Move`, and the builder decides which, never a backend. A use of a `shared` value (a reference-counted handle) is not a copy: sharing it is the explicit `Rvalue::Retain` below.

### 2.5 Rvalues

```rust
pub enum Rvalue {
    Use(Operand),
    Ref(BorrowKind, Place),               // BorrowKind = Shared | Mut
    Retain(Place),                        // a new handle to a `shared` / Rc / Arc value; count + 1
    BinaryOp(BinOp, Operand, Operand),
    CheckedBinaryOp(BinOp, Operand, Operand), // yields (result, overflowed: bool)
    UnaryOp(UnOp, Operand),
    Cast(CastKind, Operand, Ty),
    Discriminant(Place),                  // an enum place's variant index, as an integer
    Len(Place),                           // length of an array or slice place
    Aggregate(AggregateKind, Vec<Operand>),
}

pub enum AggregateKind {
    Tuple,
    Array(Ty),                            // element type
    Adt { def: DefId, variant: VariantIdx, args: TyList }, // struct, or one enum variant
    Shared { def: DefId, variant: VariantIdx },            // allocates a counted box, count = 1
    Closure { def: DefId, args: TyList },                  // the captures, in capture order
}
```

- Arithmetic overflow and division by zero abort (C8). The builder emits `CheckedBinaryOp` and a `SwitchInt` on the overflow flag whose failing edge is an `Abort` terminator. There is no separate `Assert` terminator.
- Operators on user types never reach MIR as `BinaryOp`: the existing lowering pass already turns them into trait-method calls, which become `Call` terminators.
- `Aggregate` builds a whole value at once. A partially built value is never observable.
- **Erased function values.** A closure or function item stored where its exact type cannot be kept (a field, a `Vec` of handlers) is cast with `CastKind::Erase` to the type `Fn(A) -> R`, `MutFn(A) -> R` or `OnceFn(A) -> R`, written `move _3 as Fn(i64) -> i64 (Erase)`. An erased value is move-only and is not counted. It owns its closure's captures, so it needs drop and is dropped as a whole, like a `Vec`. It may hold the borrows its closure captured. An `Fn` value may also be erased into a wider kind (`Fn` into `MutFn` or `OnceFn`, `MutFn` into `OnceFn`), never a narrower one. A call through it is a `Call` whose `func` is `copy r` with `r: ref Fn(..)`, or `r: mut ref MutFn(..)`, or `move f` with `f: OnceFn(..)`, which consumes it. The validator checks the kind, the arguments and the result. A closure's signature is not in its MIR type, so for a closure the interpreter checks the signature at the call.
- **Weak handles.** `weak T`, for a `shared` type `T` (core semantics §6.5), is the type `weak Node`. A weak value keeps the object's memory but not the object: it is move-only, needs drop (dropping it gives back a weak count), is dropped as a whole, and holds no borrow. It is made with `CastKind::Downgrade` from `ref shared T`, or from `ref weak T` to copy one, so no strong count is taken and given back. It is read with `CastKind::Upgrade` from `ref weak T` to `Option[shared T]`, which is `Some` with a new counted handle while the object is alive and `None` after its last handle is dropped. A weak field is read as a value, like a handle, so it takes no borrow flag (§6.2).

### 2.6 Statements

```rust
pub struct Statement { pub kind: StatementKind, pub source_info: SourceInfo }

pub enum StatementKind {
    Assign(Place, Rvalue),
    StorageLive(Local),
    StorageDead(Local),
    SetDiscriminant(Place, VariantIdx),
    BorrowFlag(FlagOp),   // Acquire { place, kind, loan } | Release { loan } | Check { place, kind }
    Nop,
}
```

- `Assign` does not drop the destination's old value. The builder emits an explicit `Drop(place)` terminator before an assignment to a place that may hold a value (core semantics D4: evaluate the right side, drop the old value, then store), and drop elaboration makes that drop conditional where the place may be uninitialized.
- `StorageLive` / `StorageDead` mark where a local's memory exists. They neither initialize nor drop. A local is dead before its `StorageLive` and after its `StorageDead`, and reading it there is undefined, which the MIR interpreter reports.
- `BorrowFlag` is core semantics §6.2's run-time check on a field of a `shared` value whose type is neither `Copy` nor a handle aggregate. The builder never emits it. A pass after drop elaboration (`src/mir/flags.rs`) inserts an `Acquire` after each `Ref` of such a field, a `Release` of that loan site where the borrow checker's liveness says no local can hold it, and a `Check` before each write, drop or read of such a field. A conflicting `Acquire` or `Check` panics; a frame releases what it still holds when it returns. Text form: `flag_acquire(&mut _1.0, L3)`, `flag_release(L3)`, `flag_check(&_1.0)`.

### 2.7 Terminators

```rust
pub struct Terminator { pub kind: TerminatorKind, pub source_info: SourceInfo }

pub enum TerminatorKind {
    Goto { target: BasicBlock },
    SwitchInt { discr: Operand, targets: SwitchTargets }, // (value, block) pairs + otherwise
    Call {
        func: Operand,                 // ConstKind::FnDef for a direct call, else a fn or closure value
        args: Vec<Operand>,            // by-value args are Move, so the callee owns them (D6)
        destination: Place,
        target: Option<BasicBlock>,    // None when the callee never returns
    },
    Drop { place: Place, target: BasicBlock },
    Return,                            // returns Local(0)
    Abort { reason: AbortReason },     // panic, overflow, failed bounds check, unreachable arm
    Unreachable,                       // proven impossible; the interpreter reports it if reached
}
```

A call that panics aborts the process inside the callee, so a `Call` has at most one successor. `Call` and `Drop` still carry an `unwind: UnwindAction` field whose only value in v1 is `Abort` (plan recheck 2026-10-07 §2.3, adopted by Gowtham). It is the place a cleanup edge goes if task-level failure isolation arrives later: adding a `Cleanup(BasicBlock)` variant then is a compile error at every pass that must decide what it means (successors, the dataflows, drop elaboration, the validator, the interpreter and the LLVM lowering) instead of a change to the shape of every terminator. The text form prints nothing for `Abort`.

## 3. Phases and what each guarantees

```rust
pub enum MirPhase { Built, Checked, DropsElaborated, Optimized }
```

| Phase | Produced by | Guarantees |
|---|---|---|
| `Built` | the builder, from typed HIR | Every scope end, early exit, assignment and temporary already has its `Drop` terminators, in the order core semantics D1 to D5 require. A `Drop` may name a place that is moved on some paths. `defer` / `errdefer` bodies are already inlined on the exits they cover. |
| `Checked` | move check and borrow check (read-only passes) | No use after move, no move out of a forbidden place (C3), no partial move from a type with a `Drop` body (C4), borrows valid. The body is unchanged. The move check is `src/mir/movecheck.rs`: drop elaboration's move paths and dataflow over every non-`Copy` place, with any read, borrow or move of a place that may be uninitialized an error; a `Drop` is not a use. The borrow check is `src/mir/borrowck.rs`: a loan per `Ref`, origins per local by forward dataflow, non-lexical liveness, and the §5.6 conflicts plus a result that borrows an owned local; the caller supplies which callees take `ref self` (§5.4), since MIR does not record receivers, and two-phase borrows are left to the builder's order. |
| `DropsElaborated` | drop elaboration | Every `Drop` runs on a place that is fully initialized on every path reaching it. Conditional drops are explicit `SwitchInt`s on `DropFlag` locals. A drop of a partially moved aggregate is expanded into drops of its remaining fields. Both backends accept only this phase or later. |
| `Optimized` | optional passes | Same as `DropsElaborated`. The passes may only do what core semantics D9 allows. |

A validator runs after every phase and checks that phase's invariants plus the structural ones: every block has a terminator, every place's type is consistent with its projections, `Copy` is only used on `Copy` types, no `Move` goes through `Index` or `Deref`, and no type parameters remain.

## 4. How the builder lowers the core semantics

- **Scopes.** The builder keeps a stack of lexical scopes. Each scope records the locals it declared, in order, and its `defer` / `errdefer` blocks. At a normal scope exit it emits, in reverse order of introduction, one `Drop` per local that needs dropping and the body of each `defer`, then `StorageDead` for each local (D1, D2).
- **Early exits.** `break`, `continue`, `return` and `?` emit the exit sequence of every scope they leave, innermost first, then jump (D5). An error-return edge also emits the `errdefer` bodies of each scope it leaves, in the same LIFO. Each `defer` body is lowered once per exit edge it covers. That duplicates code, but each copy is an ordinary block the later passes treat like any other. Sharing the copies is an optimization for later.
- **Temporaries.** A temporary is a `Temp` local whose scope is its statement (D3). A `match`, `if let` or `while let` scrutinee temporary belongs to a scope spanning the whole construct.
- **Parameters.** A by-value parameter is a local of the outermost scope, declared before the body's locals, so its `Drop` is emitted after theirs (D6).
- **Assignment.** `x = e` lowers to the evaluation of `e` into a temporary, `Drop(x)`, then `Assign(x, Use(Move(tmp)))` (D4).
- **Patterns.** A `match` lowers to `Discriminant` and `SwitchInt`, then a `Downcast` and `Field` projection for each bound payload. A binding that takes a payload by value is `Assign(binding, Use(Move(scrutinee.as_variant.field)))`, which is a partial move of the scrutinee.

## 5. Drop elaboration

Drop elaboration is the step the legacy backends never had: one pass that turns "drop this place at scope end" into exactly the drops that are due on each path. Its specification is the place/state model in `docs/spikes/ownership-drop-judgment.md` §1 to §3. `src/ownership_oracle.rs` and `src/param_fate.rs` are reference material for the cases it must handle, not code to port.

1. **Move paths.** Build the tree of every place that is moved, or that is a prefix of a moved place (rustc's move paths). Each `Move` operand, each `Assign` to a place and each `Drop` is attributed to a move path.
2. **Initialization dataflow.** Compute *maybe initialized* and *maybe uninitialized* for every move path at every program point, forward over the CFG.
3. **Classify each `Drop`.** If the place is initialized on every path, the drop stays as it is. If it is uninitialized on every path, the drop is deleted. If it is initialized on some paths, a `DropFlag` local is added, set at each initialization and cleared at each move, and the `Drop` is guarded by `SwitchInt` on it. If some child paths were moved out, the drop is *opened*: replaced by drops of the remaining fields in reverse declaration order, each one classified again. Opening never reaches a type with a user `Drop` body, because a partial move from such a type is rejected (C4). An enum is opened by a `SwitchInt` on its discriminant, with one arm per variant dropping that variant's remaining fields. If the opened place may itself have been moved as a whole on some path, the opened drops sit behind that place's own flag. Implemented in `src/mir/elaborate.rs`; its tests run every body both unelaborated and elaborated in the MIR interpreter and require the same output.
4. **Flags start false.** Every drop flag is assigned `false` in the entry block, so a path that never initialized the place never drops it.

## 6. The MIR interpreter's view (M1)

The MIR interpreter is the reference implementation and the oracle the LLVM backend is checked against. It needs nothing beyond the types above:

- each local is a typed allocation with per-field initialization state;
- a `Move` marks its source uninitialized, and reading an uninitialized place is reported as an error rather than silently allowed;
- each heap allocation is tracked (Miri-style), so a double free, a use after free and a leak are all reported;
- `Drop` of a type with a user `Drop` body calls that body, then drops the fields; `String`, `Vec`, `Map` and the other library collections are intrinsic types whose drop the interpreter implements directly;
- `Retain` and the drop of a `shared` handle change a reference count, and the box is freed when the count reaches zero.

**Events the interpreter records.** Each run can emit a trace of ownership events, one line per event, in program order, for the corpus runner and the backend-differential tests to diff:

| Event | When |
|---|---|
| `enter <fn>` / `exit <fn>` | a MIR body starts / returns normally (functions the interpreter implements natively, such as `println`, record nothing) |
| `alloc <id> <ty>` | a heap allocation (an `Aggregate` of a `shared` type, a library collection growing, a closure environment); the byte size joins once layouts exist |
| `free <id>` | that allocation is freed |
| `move <place>` | a `Move` operand marks its source uninitialized |
| `init <place>` | an `Assign` initializes a place |
| `drop <place> <ty>` | a `Drop` terminator runs, before any user body |
| `drop_body <ty>` | a user `Drop` body is entered |
| `retain <id> <count>` / `release <id> <count>` | a reference count changes, with the count after the change |
| `flag <local> <bool>` | a drop flag is set |
| `abort <reason>` | an `Abort` terminator runs |

The trace is the reference for drop behaviour: a program's printed output shows only the user `Drop` bodies, while the trace also shows every free and every count change, so a difference in plain memory handling between the two backends is visible too. Allocation ids are assigned in allocation order, so traces are deterministic for a single-threaded program. At exit the interpreter reports every allocation still live as a leak. `interp::run` records the trace; `interp::run_untraced`, which `karac __mir-run` and the corpus use, does not, since one event per statement is most of the time and memory a long program spends. Leaks are reported either way.

Because the interpreter checks initialization on every read, any drop elaboration bug that drops a moved place, or never drops a live one, shows up as a reported error on the first program that exercises it, rather than as a wrong line of output.

## 7. Textual form

Every body prints in a stable textual form, used for golden tests, for `karac dump-mir` and for the corpus runner to diff. The form is part of the contract: locals are numbered `_0`, `_1`, ... in declaration order, blocks `bb0`, `bb1`, ... in creation order, a user local carries its source name as a trailing comment, and nothing in the output depends on hash-map iteration order or on memory addresses. Two builds of the same program print byte-identical MIR.

```
fn eat(_1: R) -> () {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = println(const "mid") -> bb1;
    }
    bb1: {
        drop(_1) -> bb2;       // by-value parameter, owned by the callee (D6)
    }
    bb2: {
        return;
    }
}
```

## 8. Optimizations and reference counts

Optimization passes on MIR follow `docs/core-semantics.md` §7.10: a user `Drop` body call is never moved, added or removed; a plain memory free may run earlier, after the value's last use; and a matched `Retain` / release pair on a `shared` handle may be removed only if no count reaches zero at a different point as a result. That keeps the existing RC-elision win for read-only tree walks (`src/rc_elide.rs`) legal on MIR.

## 9. Open questions

1. **Typed HIR interface.** This note assumes `NodeId`, `DefId`, `Symbol` and an interned, monomorphic `Ty` from the typed-HIR work (Threads B and C). The names here are placeholders until that interface lands.
2. **Library collections.** Are `String` / `Vec` / `Map` / `Set` opaque intrinsic types in MIR (the default here), or ordinary ADTs over raw pointers implemented in Kāra? Intrinsic is simpler for M1. Kāra-level implementations would need raw-pointer places, which this note does not define.
3. **Closures and captures.** Whether a capture is by value or by reference is decided before MIR, and the closure is an `Aggregate` of its captures. A by-value capture moves the captured local, exactly like any other move.
4. **`par {}` and `spawn`.** Both lower to `Call`s of runtime intrinsics that take closures. Nothing about them is special in MIR.
5. **Pattern guards.** A guard that reads a binding must not move it. The builder binds by reference for the guard and moves only once the arm is chosen, as rustc does.
