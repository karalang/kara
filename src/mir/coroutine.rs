//! The stackless coroutine transform (`docs/spikes/coroutines.md`, "The MIR
//! transform"): which bodies are coroutines, where they suspend, and which
//! locals the frame must hold across those points.
//!
//! A suspending call is an ordinary `Call` to every pass before this one, so
//! the analysis runs on elaborated, borrow-checked bodies and only reads them.

use std::collections::{btree_map, BTreeMap, BTreeSet};

use crate::ids::{DefId, NodeId};

use super::borrowck::liveness;
use super::syntax::*;
use super::ty::{AdtDef, AdtId, FnKind, IntTy, Ty, TyInterner, TyKind, VariantDef};

/// The executor's test native: `Pending` on its first resume and `Ready(())`
/// on the next.
pub const YIELD_NOW: &str = "__yield_now";

/// Whether the native `name` suspends: `__yield_now`, a sleep, a `select`
/// with no arm ready (`__select_park`), a channel
/// receive, a join, a socket call that can wait, a child's `wait`, or a
/// pool's `acquire`. The interpreter's natives complete synchronously, so
/// under the executor a suspending native other than `__yield_now` yields
/// once and then makes its call when resumed.
pub fn suspends_native(name: &str) -> bool {
    if name == YIELD_NOW || name == "sleep_ms" || name == "__select_park" {
        return true;
    }
    let Some((ty, method)) = name.rsplit_once('.') else {
        return false;
    };
    let base = ty.split('[').next().unwrap_or(ty);
    matches!(
        (base, method),
        ("Receiver" | "Channel", "recv" | "recv_blocking")
            | ("Sender" | "Channel", "send")
            | ("TaskHandle", "join" | "try_join")
            | ("Mutex", "acquire")
            | ("TaskGroup", "spawn_frame" | "end")
            | ("TcpListener" | "TlsListener", "accept")
            | ("TcpStream", "read" | "write" | "connect")
            | ("TlsStream", "read" | "write")
            | ("UdpSocket", "recv" | "recv_from")
            | ("Child", "wait")
            | (
                "ChildStdout" | "ChildStderr",
                "read" | "read_to_end" | "read_to_string"
            )
            | ("Client", "get" | "post")
            | ("RequestBuilder", "send")
            | ("Pool", "acquire")
    )
}

/// The readiness native the transform asks before each suspending native:
/// `__ready(const "<native>", args..)` is `true` when that call can
/// complete now (B-2026-10-14-49).
pub const READY: &str = "__ready";

/// The native a readiness loop asks after a call on a descriptor:
/// `__io_again()` is `true` when the call found nothing after all and did
/// nothing (the reactor contract's `IO_NOT_READY`, `runtime/src/mir_task.rs`),
/// so the loop asks `__ready` again.
pub const IO_AGAIN: &str = "__io_again";

/// Cancellation (core-semantics §11.9) tears a task down at the wait it is
/// suspended in, as an error exit from every frame on its chain. In a
/// program that cancels, the MIR builder follows each suspending call with
/// a cancel exit: `__unwinding()` is `true` when the call returned because
/// its callee was torn down, and the exit then runs every scope's drops and
/// `defer`s, between `__unwind(false)` and `__unwind(true)`, and returns.
/// The readiness loop of a suspending native with such an exit asks
/// `__cancelled()` each time its task resumes from the wait: `true` once,
/// when the task has been cancelled, and the loop then drops the call's
/// arguments and leaves for the exit, the call not made.
pub const CANCELLED: &str = "__cancelled";
/// See [`CANCELLED`].
pub const UNWINDING: &str = "__unwinding";
/// See [`CANCELLED`].
pub const UNWIND: &str = "__unwind";

/// A task of an isolating group ends at a panic (core-semantics §11.12):
/// in a program that makes one, the MIR builder lowers each `Abort` to a
/// call of `__panic(code)` ([`crate::mir::syntax::AbortReason::code`])
/// followed by a cancel exit, and gives every call that may panic one
/// too. Inside such a task, a panic records the task's failure and leaves
/// by those exits, as a teardown does; anywhere else `__panic` aborts, as
/// the `Abort` would have.
pub const PANIC: &str = "__panic";

/// The native that makes an isolating group (§11.12).
pub const ISOLATING: &str = "TaskGroup.isolating";

/// Whether the call that continues to `target` in `body` has the cancel
/// exit the builder puts after a call that may suspend or panic.
pub fn has_cancel_exit(body: &Body, target: BasicBlock) -> bool {
    cancel_exit_of(body, target).is_some()
}

/// A coroutine erased into a function value (M4 item 4) is called through
/// a frame of its own on the heap. `__fn_suspends(ref f)` is `true` when
/// the erased value `f` is such a coroutine. `__fn_start(f, args..)` then
/// starts it: it calls the coroutine's `F#dstart` with `f`'s environment
/// and the arguments, as a call through `f` would call `F`, and answers
/// the started frame as an erased `MutFn() -> Poll[T]` whose every call
/// resumes it and whose drop drops it through `F#drop_frame`. The caller
/// resumes it until it is ready, suspending in between.
pub const FN_SUSPENDS: &str = "__fn_suspends";
/// See [`FN_SUSPENDS`].
pub const FN_START: &str = "__fn_start";
/// `__frame_start(const "F#resume", args..)`, the body of `F#dstart`: a
/// fresh frame of that resume body on the heap, in its first state, with
/// `args` as its arguments, as the erased `MutFn() -> Poll[T]` that
/// resumes it.
pub const FRAME_START: &str = "__frame_start";

/// The body that starts coroutine `name`'s frame for a call through a
/// function value ([`FN_START`]).
pub fn dstart_name(name: &str) -> String {
    format!("{name}#dstart")
}

/// Whether `body_name` is a coroutine's `dstart` body.
pub fn is_dstart(body_name: &str) -> bool {
    body_name.ends_with("#dstart")
}

/// The function a generated body was made from: `F` for `F#resume`,
/// `F#drop_frame` and `F#dstart`, which share `F`'s DefId.
pub fn made_from(body_name: &str) -> &str {
    ["#resume", "#drop_frame", "#dstart"]
        .iter()
        .find_map(|s| body_name.strip_suffix(s))
        .unwrap_or(body_name)
}

/// The erased function type `t` with its kind forgotten, so a value
/// widened from `Fn` to `MutFn` or `OnceFn` keeps its taint.
fn fn_key(tys: &TyInterner, t: Ty) -> Option<Ty> {
    match tys.kind(t) {
        TyKind::Fn { params, ret, .. } => Some(tys.intern(TyKind::Fn {
            params,
            ret,
            kind: FnKind::Fn,
        })),
        _ => None,
    }
}

/// The name a program that cancels gives the calls through values of the
/// type `t` (behind references) that may suspend, among the callees whose
/// calls get a cancel exit (`lower::cancel_points`).
pub fn fn_value_key(tys: &TyInterner, mut t: Ty) -> Option<String> {
    while let TyKind::Ref(i) | TyKind::MutRef(i) = tys.kind(t) {
        t = i;
    }
    fn_key(tys, t).map(|k| format!("fn value {}", tys.display(k)))
}

/// The type of `o` in `body`.
fn operand_ty(body: &Body, tys: &TyInterner, o: &Operand) -> Option<Ty> {
    match o {
        Operand::Const(c) => Some(c.ty),
        Operand::Copy(p) | Operand::Move(p) => {
            super::place_ty::place_ty(body, tys, p).ok().map(|t| t.ty)
        }
    }
}

/// The erased function type a call through `func` calls, behind any
/// references, keyed by [`fn_key`].
fn called_fn_key(body: &Body, tys: &TyInterner, func: &Operand) -> Option<Ty> {
    if direct_callee(func).is_some() {
        return None;
    }
    let mut t = operand_ty(body, tys, func)?;
    while let TyKind::Ref(i) | TyKind::MutRef(i) = tys.kind(t) {
        t = i;
    }
    fn_key(tys, t)
}

/// What calls through function values add to the coroutines (M4 item 4).
pub struct DynCalls {
    /// Every coroutine: `coroutines_from`'s, and the bodies that call
    /// through a function value a coroutine may be erased into, and their
    /// callers.
    pub set: BTreeSet<String>,
    /// The coroutines erased into a function value, which get a `dstart`.
    pub erased: BTreeSet<String>,
    /// The erased types ([`fn_key`]) those values have; a call through one
    /// may suspend.
    pub tainted: BTreeSet<Ty>,
}

/// [`coroutines_from`] with calls through function values: a coroutine
/// erased into a value taints the value's type, a call through a tainted
/// type may suspend, so its body is a coroutine too, and so on until
/// nothing changes. A function type carries no effects in MIR, so every
/// value of a tainted type is asked at the call whether it suspends.
pub fn dyn_calls(bodies: &[Body], tys: &TyInterner, seed: &BTreeSet<String>) -> DynCalls {
    let mut set = coroutines_from(bodies, seed);
    loop {
        let (erased, tainted) = erased_coroutines(bodies, tys, &set);
        let callers: Vec<String> = bodies
            .iter()
            .filter(|b| !set.contains(&b.instance.name))
            .filter(|b| {
                b.blocks.iter().any(|d| match &d.terminator.kind {
                    TerminatorKind::Call { func, .. } => {
                        called_fn_key(b, tys, func).is_some_and(|k| tainted.contains(&k))
                    }
                    _ => false,
                })
            })
            .map(|b| b.instance.name.clone())
            .collect();
        if callers.is_empty() {
            return DynCalls {
                set,
                erased,
                tainted,
            };
        }
        set.extend(callers);
        set = coroutines_from(bodies, &set);
    }
}

/// The erased `MutFn() -> Poll[T]` a started frame of a coroutine
/// returning `T` is called through ([`FN_START`]).
fn poller_ty(tys: &TyInterner, polls: &mut BTreeMap<Ty, Ty>, t: Ty) -> Ty {
    let poll = poll_of(tys, polls, t);
    tys.intern(TyKind::Fn {
        params: vec![],
        ret: poll,
        kind: FnKind::MutFn,
    })
}

/// A native's name as a callee operand.
fn native_fn(tys: &TyInterner, name: &str) -> Operand {
    Operand::Const(Const {
        ty: tys.unit(),
        kind: ConstKind::FnDef(InstanceId {
            def: DefId(0),
            args: vec![],
            name: name.to_string(),
        }),
    })
}

/// `F#dstart(args..) -> MutFn() -> Poll[T]` for the coroutine `F` erased
/// into a value: `F`'s parameters, and a frame of `F` started with them
/// ([`FRAME_START`]). A call through the value starts it as a call would
/// call `F`, with the value's environment first for a closure.
fn dstart(body: &Body, tys: &TyInterner, polls: &mut BTreeMap<Ty, Ty>) -> Body {
    let si = SourceInfo {
        span: body.span,
        scope: SourceScope(0),
    };
    let poller = poller_ty(tys, polls, body.return_ty());
    let mut locals = vec![LocalDecl {
        ty: poller,
        mutability: Mutability::Mut,
        kind: LocalKind::ReturnPlace,
        source_info: si,
    }];
    locals.extend(body.args().map(|l| body.local(l).clone()));
    let mut args = vec![Operand::Const(Const {
        ty: tys.intern(TyKind::Str),
        kind: ConstKind::Str(resume_name(&body.instance.name)),
    })];
    args.extend(body.args().map(|l| Operand::Move(Place::local(l))));
    let term = |kind| Terminator {
        kind,
        source_info: si,
    };
    let blocks = vec![
        BasicBlockData {
            statements: vec![],
            terminator: term(TerminatorKind::Call {
                func: native_fn(tys, FRAME_START),
                args,
                destination: Place::local(Local::RETURN_PLACE),
                target: Some(BasicBlock(1)),
                unwind: UnwindAction::Abort,
            }),
        },
        BasicBlockData {
            statements: vec![],
            terminator: term(TerminatorKind::Return),
        },
    ];
    let mut instance = body.instance.clone();
    instance.name = dstart_name(&body.instance.name);
    Body {
        instance,
        locals,
        arg_count: body.arg_count,
        blocks,
        scopes: body.scopes.clone(),
        phase: body.phase,
        span: body.span,
        provided: Vec::new(),
        auto_par_candidates: Vec::new(),
        par_regions: Vec::new(),
        suspends: false,
        drop_flags: Vec::new(),
    }
}

/// Rewrites each call of `body` through a function value of a `tainted`
/// type ([`dyn_calls`]):
///
/// ```text
/// call:  q = __fn_suspends(ref f) -> sw
/// sw:    switchInt(copy q) -> [0: plain, otherwise: start]
/// plain: dest = f(args..) -> next
/// start: h = __fn_start(f, args..) -> poll
/// poll:  r = &mut h; p = move r() -> chk
/// chk:   d = discriminant(p); switchInt(copy d) -> [0: ready, otherwise: y]
/// y:     _ = __yield_now() -> poll
/// ready: dest = move (p as Ready).0; drop(h) -> next
/// ```
///
/// so a value that suspends runs in its own frame, resumed each time the
/// task is, while any other is called as before.
fn rewrite_dyn_calls(
    body: &mut Body,
    tys: &TyInterner,
    tainted: &BTreeSet<Ty>,
    polls: &mut BTreeMap<Ty, Ty>,
) -> Result<(), String> {
    if tainted.is_empty() {
        return Ok(());
    }
    for i in 0..body.blocks.len() {
        let t = &body.blocks[i].terminator;
        let TerminatorKind::Call {
            func,
            args,
            destination,
            target: Some(next),
            unwind,
        } = &t.kind
        else {
            continue;
        };
        let Some(key) = called_fn_key(body, tys, func).filter(|k| tainted.contains(k)) else {
            continue;
        };
        let TyKind::Fn { ret, .. } = tys.kind(key) else {
            unreachable!("fn_key answers a function type")
        };
        let (func, args, dest, next, unwind, si) = (
            func.clone(),
            args.clone(),
            destination.clone(),
            *next,
            *unwind,
            t.source_info,
        );
        let poller = poller_ty(tys, polls, ret);
        let poll_t = match tys.kind(poller) {
            TyKind::Fn { ret, .. } => ret,
            _ => unreachable!("a poller is a function type"),
        };
        let new_local = |body: &mut Body, ty: Ty| {
            body.locals.push(LocalDecl {
                ty,
                mutability: Mutability::Mut,
                kind: LocalKind::Temp,
                source_info: si,
            });
            Local(body.locals.len() as u32 - 1)
        };
        let fty = operand_ty(body, tys, &func).ok_or("a call through an untyped value")?;
        let mut pre = Vec::new();
        let query = match &func {
            Operand::Copy(p) | Operand::Move(p)
                if matches!(tys.kind(fty), TyKind::Ref(_) | TyKind::MutRef(_)) =>
            {
                Operand::Copy(p.clone())
            }
            Operand::Copy(p) | Operand::Move(p) => {
                let r = new_local(body, tys.intern(TyKind::Ref(fty)));
                pre.push(Statement {
                    kind: StatementKind::Assign(
                        Place::local(r),
                        Rvalue::Ref(BorrowKind::Shared, p.clone()),
                    ),
                    source_info: si,
                });
                Operand::Move(Place::local(r))
            }
            Operand::Const(_) => return Err("a call through a constant function value".into()),
        };
        let q = new_local(body, tys.bool());
        let h = new_local(body, poller);
        let r = new_local(body, tys.intern(TyKind::MutRef(poller)));
        let p = new_local(body, poll_t);
        let d = new_local(body, tys.int(IntTy::I64));
        let u = new_local(body, tys.unit());
        let base = body.blocks.len() as u32;
        let [sw, plain, start, poll, chk, y, ready] =
            std::array::from_fn(|k| BasicBlock(base + k as u32));
        let term = |kind| Terminator {
            kind,
            source_info: si,
        };
        let block = |statements, kind| BasicBlockData {
            statements,
            terminator: term(kind),
        };
        let call = |func, args, destination: Local, target| TerminatorKind::Call {
            func,
            args,
            destination: Place::local(destination),
            target: Some(target),
            unwind,
        };
        let orig = std::mem::replace(
            &mut body.blocks[i].terminator,
            term(call(native_fn(tys, FN_SUSPENDS), vec![query], q, sw)),
        );
        body.blocks[i].statements.extend(pre);
        body.blocks.push(block(
            vec![],
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(q)),
                targets: SwitchTargets {
                    values: vec![(0, plain)],
                    otherwise: start,
                },
            },
        ));
        body.blocks.push(BasicBlockData {
            statements: vec![],
            terminator: orig,
        });
        let mut start_args = vec![func];
        start_args.extend(args);
        body.blocks.push(block(
            vec![],
            call(native_fn(tys, FN_START), start_args, h, poll),
        ));
        body.blocks.push(block(
            vec![Statement {
                kind: StatementKind::Assign(
                    Place::local(r),
                    Rvalue::Ref(BorrowKind::Mut, Place::local(h)),
                ),
                source_info: si,
            }],
            call(Operand::Move(Place::local(r)), vec![], p, chk),
        ));
        body.blocks.push(block(
            vec![Statement {
                kind: StatementKind::Assign(Place::local(d), Rvalue::Discriminant(Place::local(p))),
                source_info: si,
            }],
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(d)),
                targets: SwitchTargets {
                    values: vec![(0, ready)],
                    otherwise: y,
                },
            },
        ));
        body.blocks.push(block(
            vec![],
            call(native_fn(tys, YIELD_NOW), vec![], u, poll),
        ));
        body.blocks.push(block(
            vec![Statement {
                kind: StatementKind::Assign(
                    dest,
                    Rvalue::Use(Operand::Move(Place {
                        local: p,
                        projection: vec![
                            ProjElem::Downcast(VariantIdx(0)),
                            ProjElem::Field(FieldIdx(0), ret),
                        ],
                    })),
                ),
                source_info: si,
            }],
            TerminatorKind::Drop {
                place: Place::local(h),
                target: next,
                unwind: UnwindAction::Abort,
            },
        ));
    }
    Ok(())
}

/// The coroutines among `set` some `Erase` cast makes a value of, and the
/// types ([`fn_key`]) of those values, including the kinds they widen to.
fn erased_coroutines(
    bodies: &[Body],
    tys: &TyInterner,
    set: &BTreeSet<String>,
) -> (BTreeSet<String>, BTreeSet<Ty>) {
    let by_def = |def: DefId| {
        bodies
            .iter()
            .find(|b| b.instance.def == def && set.contains(&b.instance.name))
            .map(|b| b.instance.name.clone())
    };
    let mut erased = BTreeSet::new();
    let mut tainted = BTreeSet::new();
    let mut casts = Vec::new();
    for b in bodies {
        for d in &b.blocks {
            for st in &d.statements {
                if let StatementKind::Assign(_, Rvalue::Cast(CastKind::Erase, o, to)) = &st.kind {
                    let from = operand_ty(b, tys, o);
                    let source = match (direct_callee(o), from.map(|t| tys.kind(t))) {
                        (Some(n), _) => set.contains(n).then(|| n.to_string()),
                        (None, Some(TyKind::FnDef(def) | TyKind::Closure(def, _))) => by_def(def),
                        _ => None,
                    };
                    match source {
                        Some(n) => {
                            erased.insert(n);
                            tainted.extend(fn_key(tys, *to));
                        }
                        None => {
                            if let (Some(f), Some(t)) =
                                (from.and_then(|f| fn_key(tys, f)), fn_key(tys, *to))
                            {
                                casts.push((f, t));
                            }
                        }
                    }
                }
            }
        }
    }
    // Widening keeps the value, so its taint.
    let mut changed = true;
    while changed {
        changed = false;
        for &(f, t) in &casts {
            if tainted.contains(&f) && tainted.insert(t) {
                changed = true;
            }
        }
    }
    (erased, tainted)
}

/// The cancel exit the builder put after a call that continues to
/// `target`: the statements of the edge blocks drop elaboration put
/// before it, and the exit's first block.
fn cancel_exit_of(body: &Body, target: BasicBlock) -> Option<(Vec<Statement>, BasicBlock)> {
    let mut stmts = Vec::new();
    let mut b = target;
    for _ in 0..8 {
        let d = &body.blocks[b.index()];
        match &d.terminator.kind {
            TerminatorKind::Goto { target } => {
                stmts.extend(d.statements.iter().cloned());
                b = *target;
            }
            TerminatorKind::Call {
                func,
                destination,
                target: Some(sw),
                ..
            } if direct_callee(func) == Some(UNWINDING) && d.statements.is_empty() => {
                let TerminatorKind::SwitchInt {
                    discr: Operand::Copy(p),
                    targets,
                } = &body.blocks[sw.index()].terminator.kind
                else {
                    return None;
                };
                return match targets.values.as_slice() {
                    [(0, _)] if p == destination => Some((stmts, targets.otherwise)),
                    _ => None,
                };
            }
            _ => return None,
        }
    }
    None
}

/// Whether the suspending native `name` waits on a descriptor, and may find
/// it not ready after all when called.
fn on_descriptor(name: &str) -> bool {
    let Some((ty, method)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(
        (ty.split('[').next().unwrap_or(ty), method),
        ("TcpListener", "accept")
            | ("TcpStream", "read" | "write" | "connect")
            | ("UdpSocket", "recv" | "recv_from")
            | ("Child", "wait")
    )
}

/// Puts a readiness loop before each suspending native of `body` other than
/// `__yield_now`:
///
/// ```text
/// chk:  r = __ready(const "<native>", args..) -> sw
/// sw:   switchInt(copy r) -> [0: y, otherwise: call]
/// y:    _ = __yield_now() -> chk
/// call: dest = <native>(args..) -> next
/// ```
///
/// and, after a native on a descriptor, the retry of a call that found it
/// not ready after all:
///
/// ```text
/// call: dest = <native>(args..) -> ag
/// ag:   a = __io_again() -> sw2
/// sw2:  switchInt(copy a) -> [0: next, otherwise: chk]
/// ```
///
/// so the task suspends only while the call cannot complete, and the
/// executor can park it on what `__ready` saw. `__ready` takes the native's
/// arguments as copies, or as shared borrows of the owned values the call
/// itself consumes.
fn insert_ready_loops(body: &mut Body, tys: &TyInterner) -> Result<(), String> {
    let unit = tys.unit();
    let bool_t = tys.bool();
    let str_t = tys.intern(TyKind::Str);
    for i in 0..body.blocks.len() {
        let t = &body.blocks[i].terminator;
        let TerminatorKind::Call {
            func,
            args,
            target: Some(_),
            unwind,
            ..
        } = &t.kind
        else {
            continue;
        };
        let Some(callee) = direct_callee(func).map(str::to_string) else {
            continue;
        };
        if callee == YIELD_NOW || !suspends_native(&callee) {
            continue;
        }
        let (args, unwind, si) = (args.clone(), *unwind, t.source_info);
        let new_local = |body: &mut Body, ty: Ty| {
            body.locals.push(LocalDecl {
                ty,
                mutability: Mutability::Mut,
                kind: LocalKind::Temp,
                source_info: si,
            });
            Local(body.locals.len() as u32 - 1)
        };
        let r = new_local(body, bool_t);
        let u = new_local(body, unit);
        let mut stmts = Vec::new();
        let mut ready_args = vec![Operand::Const(Const {
            ty: str_t,
            kind: ConstKind::Str(callee.clone()),
        })];
        for a in &args {
            ready_args.push(match a {
                Operand::Move(p) => {
                    let pt = super::place_ty::place_ty(body, tys, p)?.ty;
                    if tys.is_copy(pt) || matches!(tys.kind(pt), TyKind::Ref(_)) {
                        Operand::Copy(p.clone())
                    } else {
                        // A `mut ref` is not Copy: `__ready` gets a shared
                        // reborrow of its target, and anything else a shared
                        // borrow of the argument, so the call still moves it
                        // (B-2026-10-15-75).
                        let (target, rt) = match tys.kind(pt) {
                            TyKind::MutRef(t) => (p.project(ProjElem::Deref), t),
                            _ => (p.clone(), pt),
                        };
                        let rt = new_local(body, tys.intern(TyKind::Ref(rt)));
                        stmts.push(Statement {
                            kind: StatementKind::Assign(
                                Place::local(rt),
                                Rvalue::Ref(BorrowKind::Shared, target),
                            ),
                            source_info: si,
                        });
                        Operand::Move(Place::local(rt))
                    }
                }
                _ => a.clone(),
            });
        }
        let base = body.blocks.len() as u32;
        let (chk, sw, y, call) = (
            BasicBlock(base),
            BasicBlock(base + 1),
            BasicBlock(base + 2),
            BasicBlock(base + 3),
        );
        let native = |name: &str| {
            Operand::Const(Const {
                ty: unit,
                kind: ConstKind::FnDef(InstanceId {
                    def: DefId(0),
                    args: vec![],
                    name: name.to_string(),
                }),
            })
        };
        let term = |kind| Terminator {
            kind,
            source_info: si,
        };
        let orig = std::mem::replace(
            &mut body.blocks[i].terminator,
            term(TerminatorKind::Goto { target: chk }),
        );
        let block = |statements, kind| BasicBlockData {
            statements,
            terminator: term(kind),
        };
        body.blocks.push(block(
            stmts,
            TerminatorKind::Call {
                func: native(READY),
                args: ready_args,
                destination: Place::local(r),
                target: Some(sw),
                unwind,
            },
        ));
        body.blocks.push(block(
            vec![],
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(r)),
                targets: SwitchTargets {
                    values: vec![(0, y)],
                    otherwise: call,
                },
            },
        ));
        body.blocks.push(block(
            vec![],
            TerminatorKind::Call {
                func: native(YIELD_NOW),
                args: vec![],
                destination: Place::local(u),
                target: Some(chk),
                unwind,
            },
        ));
        // A wait with a cancel exit, found before the call moves.
        let exit = match &orig.kind {
            TerminatorKind::Call {
                target: Some(t), ..
            } => cancel_exit_of(body, *t),
            _ => None,
        };
        let mut orig = orig;
        if on_descriptor(&callee) {
            let TerminatorKind::Call { target, .. } = &mut orig.kind else {
                unreachable!("matched a call above")
            };
            let next = target
                .replace(BasicBlock(base + 4))
                .expect("a call that returns");
            let a = new_local(body, bool_t);
            body.blocks.push(BasicBlockData {
                statements: vec![],
                terminator: orig,
            });
            body.blocks.push(block(
                vec![],
                TerminatorKind::Call {
                    func: native(IO_AGAIN),
                    args: vec![],
                    destination: Place::local(a),
                    target: Some(BasicBlock(base + 5)),
                    unwind,
                },
            ));
            body.blocks.push(block(
                vec![],
                TerminatorKind::SwitchInt {
                    discr: Operand::Copy(Place::local(a)),
                    targets: SwitchTargets {
                        values: vec![(0, next)],
                        otherwise: chk,
                    },
                },
            ));
        } else {
            body.blocks.push(BasicBlockData {
                statements: vec![],
                terminator: orig,
            });
        }
        // Resumed from the yield, the loop asks whether its task was
        // cancelled, and if so drops the arguments the call would have
        // moved, last first, runs the edge's flag updates and leaves for
        // the exit.
        let Some((edge, exit)) = exit else {
            continue;
        };
        let drops: Vec<Place> = args
            .iter()
            .rev()
            .filter_map(|a| match a {
                Operand::Move(p) => Some(p.clone()),
                _ => None,
            })
            .filter(|p| super::place_ty::place_ty(body, tys, p).is_ok_and(|t| tys.needs_drop(t.ty)))
            .collect();
        let c = new_local(body, bool_t);
        let cx = BasicBlock(body.blocks.len() as u32);
        if let TerminatorKind::Call { target, .. } = &mut body.blocks[y.index()].terminator.kind {
            *target = Some(cx);
        }
        body.blocks.push(block(
            vec![],
            TerminatorKind::Call {
                func: native(CANCELLED),
                args: vec![],
                destination: Place::local(c),
                target: Some(BasicBlock(cx.0 + 1)),
                unwind,
            },
        ));
        let leave = BasicBlock(cx.0 + 2);
        body.blocks.push(block(
            vec![],
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(c)),
                targets: SwitchTargets::if_else(leave, chk),
            },
        ));
        for (k, p) in drops.into_iter().enumerate() {
            body.blocks.push(block(
                vec![],
                TerminatorKind::Drop {
                    place: p,
                    target: BasicBlock(leave.0 + k as u32 + 1),
                    unwind,
                },
            ));
        }
        body.blocks
            .push(block(edge, TerminatorKind::Goto { target: exit }));
    }
    Ok(())
}

/// The native a group's drop waits on first: `TaskGroup.end(&g)` completes
/// once every task `g` started has returned (B-2026-10-15-30).
pub const GROUP_END: &str = "TaskGroup.end";

/// Whether `ty` is the library's `TaskGroup`.
fn is_task_group(tys: &TyInterner, ty: Ty) -> bool {
    matches!(tys.kind(ty), TyKind::Adt(a) if tys.adt(a).library_name() == Some("TaskGroup"))
}

/// Puts a call to [`GROUP_END`] before each drop of a `TaskGroup` in
/// `body`, so dropping a group waits for its tasks:
///
/// ```text
/// drop(g) -> next      becomes      r = &g; _ = TaskGroup.end(move r) -> d
///                                   d: drop(g) -> next
/// ```
fn insert_group_ends(body: &mut Body, tys: &TyInterner) -> Result<(), String> {
    for i in 0..body.blocks.len() {
        let t = &body.blocks[i].terminator;
        let TerminatorKind::Drop { place, unwind, .. } = &t.kind else {
            continue;
        };
        let pt = super::place_ty::place_ty(body, tys, place)?.ty;
        if !is_task_group(tys, pt) {
            continue;
        }
        let (place, unwind, si) = (place.clone(), *unwind, t.source_info);
        let mut new_local = |ty: Ty| {
            body.locals.push(LocalDecl {
                ty,
                mutability: Mutability::Mut,
                kind: LocalKind::Temp,
                source_info: si,
            });
            Local(body.locals.len() as u32 - 1)
        };
        let r = new_local(tys.intern(TyKind::Ref(pt)));
        let u = new_local(tys.unit());
        let d = BasicBlock(body.blocks.len() as u32);
        let drop = std::mem::replace(
            &mut body.blocks[i].terminator,
            Terminator {
                kind: TerminatorKind::Call {
                    func: Operand::Const(Const {
                        ty: tys.unit(),
                        kind: ConstKind::FnDef(InstanceId {
                            def: DefId(0),
                            args: vec![],
                            name: GROUP_END.to_string(),
                        }),
                    }),
                    args: vec![Operand::Move(Place::local(r))],
                    destination: Place::local(u),
                    target: Some(d),
                    unwind,
                },
                source_info: si,
            },
        );
        body.blocks[i].statements.push(Statement {
            kind: StatementKind::Assign(Place::local(r), Rvalue::Ref(BorrowKind::Shared, place)),
            source_info: si,
        });
        body.blocks.push(BasicBlockData {
            statements: vec![],
            terminator: drop,
        });
    }
    Ok(())
}

/// The callee a `Call` names directly, if it names one.
pub fn direct_callee(func: &Operand) -> Option<&str> {
    match func {
        Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) => Some(&inst.name),
        _ => None,
    }
}

fn calls(body: &Body) -> impl Iterator<Item = (BasicBlock, &str)> {
    body.blocks.iter().enumerate().filter_map(|(i, b)| {
        if let TerminatorKind::Call { func, .. } = &b.terminator.kind {
            direct_callee(func).map(|c| (BasicBlock(i as u32), c))
        } else {
            None
        }
    })
}

/// The bodies that are coroutines: those that declare `suspends`, and,
/// transitively, those that call a coroutine or a suspending native.
pub fn coroutines<'a>(bodies: impl IntoIterator<Item = &'a Body> + Clone) -> BTreeSet<String> {
    coroutines_from(bodies, &BTreeSet::new())
}

/// Whether any of `bodies` declares `suspends` or names a suspending
/// native, called or as a value: the sources of the effects report's
/// `suspends`, without which no body suspends.
pub fn may_suspend<'a>(bodies: impl IntoIterator<Item = &'a Body>) -> bool {
    let native = |o: &Operand| matches!(direct_callee(o), Some(c) if suspends_native(c));
    bodies.into_iter().any(|b| {
        b.suspends
            || b.blocks.iter().any(|d| match &d.terminator.kind {
                TerminatorKind::Call { func, args, .. } => native(func) || args.iter().any(native),
                _ => false,
            })
            || b.blocks
                .iter()
                .flat_map(|d| &d.statements)
                .any(|s| match &s.kind {
                    StatementKind::Assign(_, Rvalue::Use(o)) => native(o),
                    _ => false,
                })
    })
}

/// As [`coroutines`], starting from `seed` as well: the instances the
/// effects report says suspend (M4 item 3), so a body suspends exactly
/// when its effects say so. The calls are still followed, for the bodies
/// made after the report (task bodies, a span's direct call).
pub fn coroutines_from<'a>(
    bodies: impl IntoIterator<Item = &'a Body> + Clone,
    seed: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = bodies
        .clone()
        .into_iter()
        .filter(|b| b.suspends || seed.contains(&b.instance.name))
        .map(|b| b.instance.name.clone())
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for b in bodies.clone() {
            if set.contains(&b.instance.name) {
                continue;
            }
            if calls(b).any(|(_, c)| set.contains(c) || suspends_native(c)) {
                set.insert(b.instance.name.clone());
                changed = true;
            }
        }
    }
    set
}

/// The native a spawn becomes when its closure runs as a task:
/// `TaskGroup.spawn_frame(g, const "<C>#task#resume", move closure)`. The
/// native starts a fresh frame of that resume's frame type, moves the
/// closure into its field 1 (the wrapper's argument, `task_body`), and
/// queues the task; the handle receives what the task returns.
pub const SPAWN_FRAME: &str = "TaskGroup.spawn_frame";

/// The task body that runs a spawned closure `C`: `C#task(env: own C) -> T`
/// calls `C(&env)` and then drops `env`.
pub fn task_name(closure_body: &str) -> String {
    format!("{closure_body}#task")
}

/// The task body that runs a spawned closure `C` the spawning body was
/// handed by reference (`std.time.timeout`'s `f`):
/// `C#ref#task(env: ref C) -> T` calls `C(env)`, and the closure's owner
/// drops it.
fn ref_task_name(closure_body: &str) -> String {
    format!("{closure_body}#ref#task")
}

/// Whether `name` is a task body (`task_name`), or its resume.
pub fn is_task_body(name: &str) -> bool {
    name.ends_with("#task") || name.ends_with("#task#resume")
}

/// The spawned closures of `bodies`, by their bodies' names: each
/// `TaskGroup.spawn(g, move c)` whose `c` is made in the same body by a
/// closure aggregate, with the block of the call and that closure's body,
/// or whose `c` the body was handed, by value or by reference (the last
/// item says which).
fn spawns(bodies: &[Body], tys: &TyInterner) -> Vec<(usize, BasicBlock, String, bool)> {
    // A closure's body takes its environment first, by reference, or by
    // value when it moves a capture out; the environment's type tells a
    // generic closure's instances apart.
    let body_of = |def: DefId, env: Ty| {
        bodies.iter().find(|b| {
            b.instance.def == def && b.arg_count >= 1 && {
                let first = b.local(Local(1)).ty;
                first == env
                    || matches!(tys.kind(first), TyKind::Ref(t) | TyKind::MutRef(t) if t == env)
            }
        })
    };
    let mut out = Vec::new();
    for (bi, b) in bodies.iter().enumerate() {
        let made: BTreeMap<Local, (DefId, Ty)> =
            b.blocks
                .iter()
                .flat_map(|d| &d.statements)
                .filter_map(|s| match &s.kind {
                    StatementKind::Assign(
                        p,
                        Rvalue::Aggregate(AggregateKind::Closure { ty }, _),
                    ) if p.projection.is_empty() => match tys.kind(*ty) {
                        TyKind::Closure(def, _) => Some((p.local, (def, *ty))),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
        for (bb, callee) in calls(b) {
            if callee != "TaskGroup.spawn" {
                continue;
            }
            let TerminatorKind::Call { args, .. } = &b.block(bb).terminator.kind else {
                continue;
            };
            let Some(Operand::Move(c) | Operand::Copy(c)) = args.get(1) else {
                continue;
            };
            // A closure made here, or one the body was handed (a generic
            // function's closure parameter, as `std.time.timeout` spawns
            // its `f`, which arrives by reference): its type names its body
            // either way.
            let handed = || match super::place_ty::place_ty(b, tys, c).map(|t| t.ty) {
                Ok(t) => match tys.kind(t) {
                    TyKind::Closure(def, _) => Some((def, t, false)),
                    TyKind::Ref(i) | TyKind::MutRef(i) => match tys.kind(i) {
                        TyKind::Closure(def, _) => Some((def, i, true)),
                        _ => None,
                    },
                    _ => None,
                },
                Err(_) => None,
            };
            let closure = c
                .projection
                .is_empty()
                .then(|| made.get(&c.local).map(|&(d, env)| (d, env, false)))
                .flatten()
                .or_else(handed)
                .and_then(|(d, env, by_ref)| Some((body_of(d, env)?, by_ref)));
            if let Some((cb, by_ref)) = closure {
                out.push((bi, bb, cb.instance.name.clone(), by_ref));
            }
        }
    }
    out
}

/// `C#task`, for the closure body `c` (see `task_name`). A closure that
/// takes its environment by value (it moves a capture out) is called with
/// it, and drops it itself: `C#task(env) { _0 = C(move env); return }`.
/// A closure `handed` by reference runs as `C#ref#task(env: ref C)`, which
/// passes the reference on (`ref_task_name`).
fn task_body(c: &Body, tys: &TyInterner, handed: bool) -> Result<Body, String> {
    let si = SourceInfo {
        span: c.span,
        scope: SourceScope(0),
    };
    let env_ref = c.local(Local(1)).ty;
    if handed && !matches!(tys.kind(env_ref), TyKind::Ref(_) | TyKind::MutRef(_)) {
        return Err(format!(
            "{} moves a capture out, so a task cannot run it through the reference it was handed",
            c.instance.name
        ));
    }
    // A closure that mutates a capture takes its environment `mut ref`;
    // a handed one's reference is its environment as it is.
    let (env, by_ref, kind) = match tys.kind(env_ref) {
        _ if handed => (env_ref, false, BorrowKind::Shared),
        TyKind::Ref(t) => (t, true, BorrowKind::Shared),
        TyKind::MutRef(t) => (t, true, BorrowKind::Mut),
        _ => (env_ref, false, BorrowKind::Shared),
    };
    let decl = |ty, kind| LocalDecl {
        ty,
        mutability: Mutability::Mut,
        kind,
        source_info: si,
    };
    let term = |kind| Terminator {
        kind,
        source_info: si,
    };
    let unit = tys.unit();
    let call = |arg: Local, target| {
        term(TerminatorKind::Call {
            func: Operand::Const(Const {
                ty: unit,
                kind: ConstKind::FnDef(c.instance.clone()),
            }),
            args: vec![Operand::Move(Place::local(arg))],
            destination: Place::local(Local::RETURN_PLACE),
            target: Some(target),
            unwind: UnwindAction::Abort,
        })
    };
    let blocks = if by_ref {
        vec![
            BasicBlockData {
                statements: vec![Statement {
                    kind: StatementKind::Assign(
                        Place::local(Local(2)),
                        Rvalue::Ref(kind, Place::local(Local(1))),
                    ),
                    source_info: si,
                }],
                terminator: call(Local(2), BasicBlock(1)),
            },
            BasicBlockData {
                statements: vec![],
                terminator: term(TerminatorKind::Drop {
                    place: Place::local(Local(1)),
                    target: BasicBlock(2),
                    unwind: UnwindAction::Abort,
                }),
            },
            BasicBlockData {
                statements: vec![],
                terminator: term(TerminatorKind::Return),
            },
        ]
    } else {
        vec![
            BasicBlockData {
                statements: vec![],
                terminator: call(Local(1), BasicBlock(1)),
            },
            BasicBlockData {
                statements: vec![],
                terminator: term(TerminatorKind::Return),
            },
        ]
    };
    let mut locals = vec![
        decl(c.return_ty(), LocalKind::ReturnPlace),
        decl(
            env,
            LocalKind::Arg {
                name: "env".into(),
                node: NodeId(0),
            },
        ),
    ];
    if by_ref {
        locals.push(decl(env_ref, LocalKind::Temp));
    }
    Ok(Body {
        instance: InstanceId {
            def: DefId(0),
            args: vec![],
            name: if handed {
                ref_task_name(&c.instance.name)
            } else {
                task_name(&c.instance.name)
            },
        },
        locals,
        arg_count: 1,
        blocks,
        scopes: c.scopes.clone(),
        phase: c.phase,
        span: c.span,
        auto_par_candidates: Vec::new(),
        par_regions: vec![],
        provided: Vec::new(),
        suspends: false,
        drop_flags: vec![],
    })
}

/// Each spawn `spawns` found becomes `TaskGroup.spawn_frame` of its
/// closure's task body; returns those task bodies, one per spawned closure.
fn rewrite_spawns(bodies: &mut [Body], tys: &TyInterner) -> Result<Vec<Body>, String> {
    let found = spawns(bodies, tys);
    let task = |c: &str, handed| {
        if handed {
            ref_task_name(c)
        } else {
            task_name(c)
        }
    };
    let mut tasks: BTreeMap<String, Body> = BTreeMap::new();
    for (_, _, c, handed) in &found {
        if let btree_map::Entry::Vacant(e) = tasks.entry(task(c, *handed)) {
            let cb = bodies
                .iter()
                .find(|b| &b.instance.name == c)
                .ok_or_else(|| format!("no body for the spawned closure {c}"))?;
            e.insert(task_body(cb, tys, *handed)?);
        }
    }
    let str_t = tys.intern(TyKind::Str);
    for (bi, bb, c, handed) in found {
        let t = &mut bodies[bi].blocks[bb.index()].terminator.kind;
        let TerminatorKind::Call { func, args, .. } = t else {
            unreachable!("spawns finds calls");
        };
        let Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) = func
        else {
            unreachable!("spawns finds direct calls");
        };
        inst.name = SPAWN_FRAME.to_string();
        args.insert(
            1,
            Operand::Const(Const {
                ty: str_t,
                kind: ConstKind::Str(resume_name(&task(&c, handed))),
            }),
        );
    }
    Ok(tasks.into_values().collect())
}

/// A call that may suspend.
#[derive(Debug, Clone, PartialEq)]
pub struct SuspensionPoint {
    /// The block whose terminator is the call.
    pub block: BasicBlock,
    pub callee: String,
    /// Where the call returns to.
    pub resume: BasicBlock,
    /// The locals live across the call, the destination excepted: the
    /// frame must hold them while the callee is pending.
    pub across: Vec<Local>,
}

/// Where a coroutine suspends, and the locals its frame holds.
#[derive(Debug, Clone, PartialEq)]
pub struct CoroutineLayout {
    /// In block order; point `k` resumes in state `k + 1`.
    pub points: Vec<SuspensionPoint>,
    /// Every local some point holds across, plus every borrowed local (a
    /// `ref` held across a point may be its only use), in local order.
    pub frame: Vec<Local>,
}

/// The suspension points of `body`, a member of `coroutines`, and its frame,
/// which holds every local that is ever borrowed: a reference to it may be
/// live across a point when the local itself is not used again.
pub fn layout(body: &Body, coroutines: &BTreeSet<String>) -> CoroutineLayout {
    layout_with(body, coroutines, None)
}

/// What the result of a call of each body borrows.
pub type CallBorrows<'a> = dyn Fn(&InstanceId) -> super::borrowck::ResultBorrows + 'a;

/// [`layout`], with the frame narrowed to the borrowed locals some loan of
/// which is live at a point, when `loans` gives what each call's result
/// borrows (the borrow check's loan liveness): live just before the call,
/// as the callee may hold an argument's borrow while it is pending, or
/// when it returns.
pub fn layout_with(
    body: &Body,
    coroutines: &BTreeSet<String>,
    loans: Option<(&TyInterner, &CallBorrows)>,
) -> CoroutineLayout {
    let live_out = liveness(body);
    let mut points = Vec::new();
    let mut frame: BTreeSet<Local> = BTreeSet::new();
    for (bb, callee) in calls(body) {
        // Every other suspending native sits behind a readiness loop
        // (`insert_ready_loops`), whose `__yield_now` is where it suspends.
        if !coroutines.contains(callee) && callee != YIELD_NOW {
            continue;
        }
        let TerminatorKind::Call {
            destination,
            target,
            ..
        } = &body.block(bb).terminator.kind
        else {
            unreachable!("calls yields Call blocks")
        };
        let Some(resume) = *target else {
            // A call that never returns never resumes either.
            continue;
        };
        let mut across: Vec<Local> = live_out[bb.index()]
            .iter()
            .copied()
            .filter(|&l| !(l == destination.local && destination.projection.is_empty()))
            .collect();
        across.sort();
        frame.extend(across.iter().copied());
        points.push(SuspensionPoint {
            block: bb,
            callee: callee.to_string(),
            resume,
            across,
        });
    }
    match loans {
        _ if points.is_empty() => {}
        Some((tys, has_receiver)) => {
            let (sites, live) =
                super::borrowck::loan_liveness(body, tys, has_receiver, &|_, _| true);
            for p in &points {
                // No loan at all leaves the table empty.
                let before = live.get(p.block.index()).and_then(|b| b.last());
                let after = live.get(p.resume.index()).and_then(|b| b.first());
                for point in before.into_iter().chain(after) {
                    for (li, _) in point.iter().enumerate().filter(|(_, &on)| on) {
                        frame.insert(sites[li].1.local);
                    }
                }
            }
        }
        None => {
            for b in &body.blocks {
                for s in &b.statements {
                    if let StatementKind::Assign(_, Rvalue::Ref(_, p)) = &s.kind {
                        frame.insert(p.local);
                    }
                }
            }
        }
    }
    CoroutineLayout {
        points,
        frame: frame.into_iter().collect(),
    }
}

/// Coroutines that reach themselves through suspending calls, whose frame
/// therefore contains itself (the compiled backends box that callee frame).
/// Each cycle is named once, by its members in order.
pub fn recursive_cycles<'a>(
    bodies: impl IntoIterator<Item = &'a Body>,
    coroutines: &BTreeSet<String>,
) -> Vec<Vec<String>> {
    let edges: BTreeMap<&str, BTreeSet<&str>> = bodies
        .into_iter()
        .filter(|b| coroutines.contains(&b.instance.name))
        .map(|b| {
            let out = calls(b)
                .map(|(_, c)| c)
                .filter(|c| coroutines.contains(*c))
                .collect();
            (b.instance.name.as_str(), out)
        })
        .collect();
    // Strongly connected components, by mutual reachability: the graphs
    // are a handful of nodes.
    let reach = |from: &str| -> BTreeSet<&str> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<&str> = edges.get(from).into_iter().flatten().copied().collect();
        while let Some(n) = stack.pop() {
            if seen.insert(n) {
                stack.extend(edges.get(n).into_iter().flatten().copied());
            }
        }
        seen
    };
    let reaches: BTreeMap<&str, BTreeSet<&str>> = edges.keys().map(|&n| (n, reach(n))).collect();
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut cycles = Vec::new();
    for (&n, r) in &reaches {
        if done.contains(n) || !r.contains(n) {
            continue;
        }
        let scc: Vec<String> = reaches
            .iter()
            .filter(|(m, mr)| r.contains(*m) && mr.contains(n))
            .map(|(m, _)| m.to_string())
            .collect();
        done.extend(scc.iter().cloned());
        cycles.push(scc);
    }
    cycles
}

// ---- the transform ----

/// The local a resume body reaches its frame through.
const FRAME: Local = Local(1);

/// A coroutine's frame type and where each of its locals lives in it.
#[derive(Debug, Clone)]
pub struct FrameInfo {
    /// `F#Frame`, a struct whose field 0 is the `u32` state.
    pub ty: Ty,
    /// The field holding each frame local of the original body.
    pub fields: BTreeMap<Local, u32>,
    /// The field holding the callee's frame at each suspension point that
    /// calls a body (a native keeps no frame).
    pub subs: BTreeMap<usize, u32>,
    /// `Poll[T]` for the body's return type `T`: `Ready(T) | Pending`.
    pub poll: Ty,
    /// The state of a frame whose body has returned.
    pub returned: u32,
}

/// The `resume` body of coroutine `name`.
pub fn resume_name(name: &str) -> String {
    format!("{name}#resume")
}

/// The frame type of coroutine `name`. `#` cannot appear in a Kāra name,
/// so the generated names (this, `resume_name`, `drop_frame_name`) never
/// collide with a user type or method.
pub fn frame_name(name: &str) -> String {
    format!("{name}#Frame")
}

/// Whether `adt_name` is a coroutine frame type.
pub fn is_frame(adt_name: &str) -> bool {
    frame_owner(adt_name).is_some()
}

/// The coroutine whose frame type is `adt_name`, if it is a frame.
pub fn frame_owner(adt_name: &str) -> Option<&str> {
    adt_name.strip_suffix("#Frame")
}

/// Whether `body_name` is a coroutine's `resume` body.
pub fn is_resume(body_name: &str) -> bool {
    body_name.ends_with("#resume")
}

/// Whether `body_name` is a coroutine's `drop_frame` body.
pub fn is_drop_frame(body_name: &str) -> bool {
    body_name.ends_with("#drop_frame")
}

/// The `drop_frame` body of coroutine `name`.
pub fn drop_frame_name(name: &str) -> String {
    format!("{name}#drop_frame")
}

/// A DefId no type uses yet, for the frame and `Poll` types the transform
/// makes.
fn fresh_def(tys: &TyInterner) -> DefId {
    let mut n = u32::MAX / 2;
    while tys.tcx().adt_def(DefId(n)).is_some() {
        n += 1;
    }
    DefId(n)
}

fn poll_of(tys: &TyInterner, polls: &mut BTreeMap<Ty, Ty>, t: Ty) -> Ty {
    if let Some(&p) = polls.get(&t) {
        return p;
    }
    let def = fresh_def(tys);
    let id = tys.add_adt(AdtDef {
        def,
        name: format!("Poll[{}]", tys.display(t)),
        is_enum: true,
        variants: vec![
            VariantDef {
                name: "Ready".into(),
                fields: vec![("0".into(), t)],
            },
            VariantDef {
                name: "Pending".into(),
                fields: vec![],
            },
        ],
        has_drop_impl: false,
        is_copy: false,
        user: false,
        sync: false,
    });
    let p = tys.intern(TyKind::Adt(id));
    polls.insert(t, p);
    p
}

/// Rewrites every coroutine among `bodies` into its `resume` body
/// (`F#resume(frame: mut ref F#Frame) -> Poll[T]`), registering the frame
/// and `Poll` types in `tys`. The other bodies are returned unchanged; the
/// original coroutine bodies are not returned, since nothing may call one
/// except through its `resume`.
///
/// Refuses a recursive cycle of coroutines, whose frame would contain
/// itself.
pub fn transform(
    bodies: &[Body],
    tys: &TyInterner,
) -> Result<(Vec<Body>, BTreeMap<String, FrameInfo>), String> {
    transform_from(bodies, tys, &BTreeSet::new(), None)
}

/// [`transform`], with the coroutines found from `suspending`, the
/// instances the effects report says suspend ([`coroutines_from`]).
pub fn transform_from(
    bodies: &[Body],
    tys: &TyInterner,
    suspending: &BTreeSet<String>,
    borrows: Option<&crate::mir::lower::BorrowSummary>,
) -> Result<(Vec<Body>, BTreeMap<String, FrameInfo>), String> {
    // A spawned closure runs as a task: its task body is a task root, and
    // a coroutine, whether or not the closure suspends.
    let mut bodies: Vec<Body> = bodies.to_vec();
    direct_span_bodies(&mut bodies, tys, suspending);
    let tasks = rewrite_spawns(&mut bodies, tys)?;
    let task_names: BTreeSet<String> = tasks.iter().map(|b| b.instance.name.clone()).collect();
    bodies.extend(tasks);
    for b in &mut bodies {
        insert_group_ends(b, tys)?;
    }
    let mut seed = suspending.clone();
    seed.extend(task_names.iter().cloned());
    let dyn_calls = dyn_calls(&bodies, tys, &seed);
    let set = dyn_calls.set;
    let bodies = bodies.as_slice();
    // A spawned closure is made as a value, but its one caller is its task
    // body.
    let spawned: BTreeSet<String> = bodies
        .iter()
        .filter(|b| {
            task_names.contains(&task_name(&b.instance.name))
                || task_names.contains(&ref_task_name(&b.instance.name))
        })
        .map(|b| b.instance.name.clone())
        .collect();
    let direct: BTreeSet<String> = set.difference(&spawned).cloned().collect();
    if let Some((name, native)) = passed_to_native(bodies, &direct, tys) {
        return Err(format!(
            "{name} suspends and is passed to `{native}`, which calls it where it cannot wait"
        ));
    }
    // The `Poll` types the calls through values and the `dstart` bodies
    // name, before the frames' (below).
    let mut polls = BTreeMap::new();
    let dstarts: Vec<Body> = bodies
        .iter()
        .filter(|b| dyn_calls.erased.contains(&b.instance.name))
        .map(|b| dstart(b, tys, &mut polls))
        .collect();
    // The coroutines' calls through values that may suspend, and their
    // suspending natives behind readiness loops.
    let bodies: Vec<Body> = bodies
        .iter()
        .map(|b| {
            let mut b = b.clone();
            if set.contains(&b.instance.name) {
                rewrite_dyn_calls(&mut b, tys, &dyn_calls.tainted, &mut polls)?;
                insert_ready_loops(&mut b, tys)?;
            }
            Ok(b)
        })
        .collect::<Result<_, String>>()?;
    let bodies = bodies.as_slice();
    let by_name: BTreeMap<&str, &Body> = bodies
        .iter()
        .filter(|b| set.contains(&b.instance.name))
        .map(|b| (b.instance.name.as_str(), b))
        .collect();
    // What each call's result borrows, for the frame's loans: the program
    // bodies' as the borrow check saw them. `KARAC_MIR_NARROW_FRAMES=0`
    // keeps every borrowed local in the frame.
    let names: rustc_hash::FxHashSet<String> =
        bodies.iter().map(|b| b.instance.name.clone()).collect();
    let borrows =
        borrows.filter(|_| std::env::var("KARAC_MIR_NARROW_FRAMES").as_deref() != Ok("0"));
    let has_receiver = borrows.map(|s| move |i: &InstanceId| s.result_borrows(&names, i));
    let loans = has_receiver.as_ref().map(|h| {
        (
            tys,
            h as &dyn Fn(&InstanceId) -> super::borrowck::ResultBorrows,
        )
    });
    let layouts: BTreeMap<&str, CoroutineLayout> = by_name
        .iter()
        .map(|(&n, b)| (n, layout_with(b, &set, loans)))
        .collect();
    // A frame holds its callees' frames, so every frame type is named
    // before any is defined: a recursive coroutine's frame holds its own
    // type, which the compiled backends box (`layout::boxed_field`) and the
    // interpreter makes when the call starts it.
    let order: Vec<&str> = layouts.keys().copied().collect();
    // The `Poll` types first: they take fresh DefIds too, which must not
    // be the ones the frames are named by before they are defined.
    let poll_tys: BTreeMap<&str, Ty> = order
        .iter()
        .map(|&n| (n, poll_of(tys, &mut polls, by_name[n].return_ty())))
        .collect();
    let mut next_def = fresh_def(tys).0;
    let frame_tys: BTreeMap<&str, (DefId, Ty)> = order
        .iter()
        .map(|&n| {
            let def = DefId(next_def);
            next_def += 1;
            while tys.tcx().adt_def(DefId(next_def)).is_some() {
                next_def += 1;
            }
            (n, (def, tys.intern(TyKind::Adt(AdtId(def.0)))))
        })
        .collect();
    let u32t = tys.int(IntTy::U32);
    let mut frames: BTreeMap<String, FrameInfo> = BTreeMap::new();
    for &n in &order {
        let body = by_name[n];
        let lay = &layouts[n];
        // The arguments first, as fields 1..=n, where a frame started
        // without its body (`spawn_frame`, `FRAME_START`) puts them; the
        // return place may be held too.
        let held: BTreeSet<Local> = lay.frame.iter().copied().collect();
        let locals: Vec<Local> = body
            .args()
            .chain(
                held.into_iter()
                    .filter(|l| l.index() == 0 || l.index() > body.arg_count),
            )
            .collect();
        let mut fields_def = vec![("state".to_string(), u32t)];
        let mut fields = BTreeMap::new();
        for l in locals {
            fields.insert(l, fields_def.len() as u32);
            fields_def.push((format!("l{}", l.0), body.local(l).ty));
        }
        let mut subs = BTreeMap::new();
        for (k, p) in lay.points.iter().enumerate() {
            if let Some(&(_, t)) = frame_tys.get(p.callee.as_str()) {
                subs.insert(k, fields_def.len() as u32);
                fields_def.push((format!("sub{k}"), t));
            }
        }
        let (def, ty) = frame_tys[n];
        tys.add_adt(AdtDef {
            def,
            name: frame_name(n),
            is_enum: false,
            variants: vec![VariantDef {
                name: frame_name(n),
                fields: fields_def,
            }],
            has_drop_impl: false,
            is_copy: false,
            user: false,
            sync: false,
        });
        frames.insert(
            n.to_string(),
            FrameInfo {
                ty,
                fields,
                subs,
                poll: poll_tys[n],
                returned: lay.points.len() as u32 + 1,
            },
        );
    }
    let mut out: Vec<Body> = bodies
        .iter()
        .filter(|b| !set.contains(&b.instance.name))
        .cloned()
        .chain(dstarts)
        .collect();
    for &n in &order {
        out.push(Resume::new(by_name[n], &layouts[n], &frames, tys).build()?);
        out.push(drop_frame(by_name[n], &layouts[n], &frames, tys)?);
    }
    Ok((out, frames))
}

/// The natives `with_span` becomes around a closure that suspends.
pub const SPAN_ENTER: &str = "with_span_enter";
pub const SPAN_EXIT: &str = "with_span_exit";

/// Rewrites `with_span(span, closure)` whose closure suspends into the
/// span's enter, a direct call of the closure's body, and the exit
/// (B-2026-10-20-26). The native would call the closure through a function
/// value, which cannot run a coroutine; the closure is made in the same
/// block, so the call can name its body, and the caller then suspends with
/// it like any other caller of a coroutine.
fn direct_span_bodies(bodies: &mut [Body], tys: &TyInterner, suspending: &BTreeSet<String>) {
    let set = coroutines_from(bodies.iter(), suspending);
    // Each suspending closure's body and the type of its environment
    // parameter: the closure itself, or a borrow of it.
    let closures: BTreeMap<DefId, (InstanceId, Ty)> = bodies
        .iter()
        .filter(|b| set.contains(&b.instance.name) && b.arg_count >= 1)
        .map(|b| (b.instance.def, (b.instance.clone(), b.locals[1].ty)))
        .collect();
    if closures.is_empty() {
        return;
    }
    let native = |name: &str| {
        Operand::Const(Const {
            ty: tys.unit(),
            kind: ConstKind::FnDef(InstanceId {
                def: DefId(u32::MAX),
                args: Vec::new(),
                name: name.to_string(),
            }),
        })
    };
    for b in bodies.iter_mut() {
        let mut bi = 0;
        while bi < b.blocks.len() {
            let data = &b.blocks[bi];
            let TerminatorKind::Call {
                func:
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(callee),
                        ..
                    }),
                args,
                destination,
                target,
                unwind,
            } = &data.terminator.kind
            else {
                bi += 1;
                continue;
            };
            let (span, Some(Operand::Move(value))) = (args.first(), args.get(1)) else {
                bi += 1;
                continue;
            };
            if callee.name != "with_span" || !value.projection.is_empty() || span.is_none() {
                bi += 1;
                continue;
            }
            // `value = move closure as OnceFn() -> T`, `closure` made here.
            let cast = data.statements.iter().rposition(|st| {
                matches!(&st.kind, StatementKind::Assign(p, Rvalue::Cast(_, Operand::Move(_), _))
                    if p.local == value.local && p.projection.is_empty())
            });
            let Some(cast) = cast else {
                bi += 1;
                continue;
            };
            let StatementKind::Assign(_, Rvalue::Cast(_, Operand::Move(closure), _)) =
                &data.statements[cast].kind
            else {
                unreachable!()
            };
            let body = data.statements[..cast]
                .iter()
                .rev()
                .find_map(|st| match &st.kind {
                    StatementKind::Assign(
                        p,
                        Rvalue::Aggregate(AggregateKind::Closure { ty }, _),
                    ) if p == closure => match tys.kind(*ty) {
                        TyKind::Closure(def, _) => closures.get(&def).cloned(),
                        _ => None,
                    },
                    _ => None,
                });
            let Some((body, env_ty)) = body else {
                bi += 1;
                continue;
            };
            let by_ref = matches!(tys.kind(env_ty), TyKind::Ref(_));
            let (closure, span) = (closure.clone(), span.cloned().unwrap());
            let (destination, target, unwind) = (destination.clone(), *target, *unwind);
            let info = data.terminator.source_info;
            let unit = |b: &mut Body| {
                b.locals.push(LocalDecl {
                    ty: tys.unit(),
                    mutability: Mutability::Mut,
                    kind: LocalKind::Temp,
                    source_info: info,
                });
                Place::local(Local((b.locals.len() - 1) as u32))
            };
            let (entered, exited) = (unit(b), unit(b));
            let run = BasicBlock(b.blocks.len() as u32);
            let exit = BasicBlock(run.0 + 1);
            let dropped = BasicBlock(run.0 + 2);
            let call = |func, args, destination, target| Terminator {
                kind: TerminatorKind::Call {
                    func,
                    args,
                    destination,
                    target,
                    unwind,
                },
                source_info: info,
            };
            b.blocks[bi].statements.remove(cast);
            b.blocks[bi].terminator = call(native(SPAN_ENTER), vec![span], entered, Some(run));
            let direct = Operand::Const(Const {
                ty: tys.unit(),
                kind: ConstKind::FnDef(body),
            });
            // A closure called through a borrow is still the caller's after
            // the call, and drops then, as `with_span` would have dropped it.
            let (statements, env, after) = if by_ref {
                b.locals.push(LocalDecl {
                    ty: env_ty,
                    mutability: Mutability::Not,
                    kind: LocalKind::Temp,
                    source_info: info,
                });
                let r = Place::local(Local((b.locals.len() - 1) as u32));
                let borrow = Statement {
                    kind: StatementKind::Assign(
                        r.clone(),
                        Rvalue::Ref(BorrowKind::Shared, closure.clone()),
                    ),
                    source_info: info,
                };
                (vec![borrow], Operand::Move(r), dropped)
            } else {
                (Vec::new(), Operand::Move(closure.clone()), exit)
            };
            b.blocks.push(BasicBlockData {
                statements,
                terminator: call(direct, vec![env], destination, Some(after)),
            });
            b.blocks.push(BasicBlockData {
                statements: Vec::new(),
                terminator: call(native(SPAN_EXIT), Vec::new(), exited, target),
            });
            if by_ref {
                b.blocks.push(BasicBlockData {
                    statements: Vec::new(),
                    terminator: Terminator {
                        kind: TerminatorKind::Drop {
                            place: closure,
                            target: exit,
                            unwind,
                        },
                        source_info: info,
                    },
                });
            }
            bi += 1;
        }
    }
}

/// A coroutine of `set` that is used other than as the callee of a direct
/// call: a function item taken as a value, or a closure that is made. The
/// parallel build keeps such a body from becoming a coroutine, whose calls
/// through a value would then each start a frame on the heap.
pub(crate) fn used_as_value(
    bodies: &[Body],
    set: &BTreeSet<String>,
    tys: &TyInterner,
) -> Option<String> {
    let closure_defs: BTreeMap<DefId, &str> = bodies
        .iter()
        .filter(|b| set.contains(&b.instance.name))
        .map(|b| (b.instance.def, b.instance.name.as_str()))
        .collect();
    let named = |o: &Operand| match o {
        Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) if set.contains(&inst.name) => Some(inst.name.clone()),
        _ => None,
    };
    for b in bodies {
        // A closure value whose only use is a direct call of its body (the
        // split `with_span`, `direct_span_bodies`) is no function value.
        let called: BTreeSet<Local> = b
            .blocks
            .iter()
            .filter_map(|d| match &d.terminator.kind {
                TerminatorKind::Call {
                    func:
                        Operand::Const(Const {
                            kind: ConstKind::FnDef(inst),
                            ..
                        }),
                    args,
                    ..
                } if closure_defs.contains_key(&inst.def) && set.contains(&inst.name) => {
                    match args.first() {
                        Some(Operand::Move(p)) if p.projection.is_empty() => Some(p.local),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        // ... or a borrow of it, made only for that call.
        let mut called = called;
        for d in &b.blocks {
            for st in &d.statements {
                if let StatementKind::Assign(r, Rvalue::Ref(_, c)) = &st.kind {
                    if r.projection.is_empty()
                        && c.projection.is_empty()
                        && called.contains(&r.local)
                    {
                        called.insert(c.local);
                    }
                }
            }
        }
        for data in &b.blocks {
            for st in &data.statements {
                let StatementKind::Assign(dest, rv) = &st.kind else {
                    continue;
                };
                if let Rvalue::Aggregate(AggregateKind::Closure { ty }, _) = rv {
                    if let TyKind::Closure(def, _) = tys.kind(*ty) {
                        if let Some(n) = closure_defs.get(&def) {
                            let direct = dest.projection.is_empty()
                                && called.contains(&dest.local)
                                && !uses_elsewhere(b, dest.local, &called);
                            if !direct {
                                return Some(n.to_string());
                            }
                        }
                    }
                }
                let ops: Vec<&Operand> = match rv {
                    Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => vec![o],
                    Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => vec![a, b],
                    Rvalue::Aggregate(_, ops) => ops.iter().collect(),
                    _ => vec![],
                };
                if let Some(n) = ops.into_iter().find_map(named) {
                    return Some(n);
                }
            }
            if let TerminatorKind::Call { args, .. } = &data.terminator.kind {
                if let Some(n) = args.iter().find_map(named) {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// A coroutine of `set` handed as a function item or closure (not erased)
/// to a native, which would call it synchronously, and that native. A
/// function item or closure passed to a body is called directly there,
/// since the callee is specialised on its type, and an erased one is
/// called through its own frame ([`FN_START`]).
fn passed_to_native(
    bodies: &[Body],
    set: &BTreeSet<String>,
    tys: &TyInterner,
) -> Option<(String, String)> {
    let names: BTreeSet<&str> = bodies.iter().map(|b| b.instance.name.as_str()).collect();
    let by_def: BTreeMap<DefId, &str> = bodies
        .iter()
        .filter(|b| set.contains(&b.instance.name))
        .map(|b| (b.instance.def, b.instance.name.as_str()))
        .collect();
    for b in bodies {
        for d in &b.blocks {
            let TerminatorKind::Call { func, args, .. } = &d.terminator.kind else {
                continue;
            };
            let Some(native) = direct_callee(func).filter(|c| !names.contains(c)) else {
                continue;
            };
            for a in args {
                if let Some(n) = direct_callee(a).filter(|n| set.contains(*n)) {
                    return Some((n.to_string(), native.to_string()));
                }
                let Some(mut t) = operand_ty(b, tys, a) else {
                    continue;
                };
                while let TyKind::Ref(i) | TyKind::MutRef(i) = tys.kind(t) {
                    t = i;
                }
                if let TyKind::FnDef(def) | TyKind::Closure(def, _) = tys.kind(t) {
                    if let Some(n) = by_def.get(&def) {
                        return Some((n.to_string(), native.to_string()));
                    }
                }
            }
        }
    }
    None
}

/// Whether `l` is read anywhere but as the first argument of a call, or
/// by a borrow into one of `called` (the temporaries those calls take).
fn uses_elsewhere(b: &Body, l: Local, called: &BTreeSet<Local>) -> bool {
    let is = |o: &Operand| matches!(o, Operand::Move(p) | Operand::Copy(p) if p.local == l);
    b.blocks.iter().any(|d| {
        d.statements.iter().any(|st| match &st.kind {
            StatementKind::Assign(r, Rvalue::Ref(_, p))
                if p.local == l && r.projection.is_empty() && called.contains(&r.local) =>
            {
                false
            }
            StatementKind::Assign(_, rv) => match rv {
                Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => is(o),
                Rvalue::BinaryOp(_, x, y) | Rvalue::CheckedBinaryOp(_, x, y) => is(x) || is(y),
                Rvalue::Aggregate(_, ops) => ops.iter().any(is),
                Rvalue::Ref(_, p) => p.local == l,
                _ => false,
            },
            _ => false,
        }) || match &d.terminator.kind {
            TerminatorKind::Call { func, args, .. } => is(func) || args.iter().skip(1).any(is),
            _ => false,
        }
    })
}

struct Resume<'a> {
    body: &'a Body,
    layout: &'a CoroutineLayout,
    frames: &'a BTreeMap<String, FrameInfo>,
    me: &'a FrameInfo,
    tys: &'a TyInterner,
    locals: Vec<LocalDecl>,
    blocks: Vec<BasicBlockData>,
}

impl<'a> Resume<'a> {
    fn new(
        body: &'a Body,
        layout: &'a CoroutineLayout,
        frames: &'a BTreeMap<String, FrameInfo>,
        tys: &'a TyInterner,
    ) -> Self {
        Resume {
            body,
            layout,
            frames,
            me: &frames[&body.instance.name],
            tys,
            locals: Vec::new(),
            blocks: Vec::new(),
        }
    }

    fn info(&self) -> SourceInfo {
        SourceInfo {
            span: self.body.span,
            scope: SourceScope(0),
        }
    }

    fn temp(&mut self, ty: Ty) -> Local {
        self.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: self.info(),
        });
        Local(self.locals.len() as u32 - 1)
    }

    fn stmt(&self, kind: StatementKind) -> Statement {
        Statement {
            kind,
            source_info: self.info(),
        }
    }

    fn assign(&self, p: Place, rv: Rvalue) -> Statement {
        self.stmt(StatementKind::Assign(p, rv))
    }

    /// `(*frame).f`, of type `ty`.
    fn field(&self, f: u32, ty: Ty) -> Place {
        Place {
            local: FRAME,
            projection: vec![ProjElem::Deref, ProjElem::Field(FieldIdx(f), ty)],
        }
    }

    fn state(&self) -> Place {
        self.field(0, self.tys.int(IntTy::U32))
    }

    fn u32_const(&self, v: u32) -> Operand {
        Operand::Const(Const {
            ty: self.tys.int(IntTy::U32),
            kind: ConstKind::Scalar(v as u128),
        })
    }

    /// `state = s; _0 = Poll::Pending; return`, after `stmts`.
    fn pending(&self, mut stmts: Vec<Statement>, s: u32) -> BasicBlockData {
        stmts.push(self.assign(self.state(), Rvalue::Use(self.u32_const(s))));
        stmts.push(self.assign(
            Place::local(Local::RETURN_PLACE),
            Rvalue::Aggregate(
                AggregateKind::Adt {
                    ty: self.me.poll,
                    variant: VariantIdx(1),
                },
                vec![],
            ),
        ));
        BasicBlockData {
            statements: stmts,
            terminator: Terminator {
                kind: TerminatorKind::Return,
                source_info: self.info(),
            },
        }
    }

    fn local(&self, l: Local) -> Result<Place, String> {
        Ok(match self.me.fields.get(&l) {
            Some(&f) => self.field(f, self.body.local(l).ty),
            None => Place::local(Local(l.0 + 2)),
        })
    }

    fn place(&self, p: &Place) -> Result<Place, String> {
        let mut out = self.local(p.local)?;
        for e in &p.projection {
            out.projection.push(match e {
                ProjElem::Index(i) => match self.local(*i)? {
                    Place { local, projection } if projection.is_empty() => ProjElem::Index(local),
                    _ => {
                        return Err(format!(
                            "{}: index {i} is held across a suspension point",
                            self.body.instance.name
                        ))
                    }
                },
                e => *e,
            });
        }
        Ok(out)
    }

    fn operand(&self, o: &Operand) -> Result<Operand, String> {
        Ok(match o {
            Operand::Copy(p) => Operand::Copy(self.place(p)?),
            Operand::Move(p) => Operand::Move(self.place(p)?),
            Operand::Const(c) => Operand::Const(c.clone()),
        })
    }

    fn rvalue(&self, rv: &Rvalue) -> Result<Rvalue, String> {
        Ok(match rv {
            Rvalue::Use(o) => Rvalue::Use(self.operand(o)?),
            Rvalue::UnaryOp(op, o) => Rvalue::UnaryOp(*op, self.operand(o)?),
            Rvalue::Cast(k, o, t) => Rvalue::Cast(*k, self.operand(o)?, *t),
            Rvalue::BinaryOp(op, a, b) => Rvalue::BinaryOp(*op, self.operand(a)?, self.operand(b)?),
            Rvalue::CheckedBinaryOp(op, a, b) => {
                Rvalue::CheckedBinaryOp(*op, self.operand(a)?, self.operand(b)?)
            }
            Rvalue::Aggregate(k, ops) => Rvalue::Aggregate(
                k.clone(),
                ops.iter()
                    .map(|o| self.operand(o))
                    .collect::<Result<_, _>>()?,
            ),
            Rvalue::Ref(k, p) => Rvalue::Ref(*k, self.place(p)?),
            Rvalue::Retain(p) => Rvalue::Retain(self.place(p)?),
            Rvalue::Discriminant(p) => Rvalue::Discriminant(self.place(p)?),
            Rvalue::Len(p) => Rvalue::Len(self.place(p)?),
            Rvalue::NullaryOp(op, t) => Rvalue::NullaryOp(*op, *t),
        })
    }

    fn statement(&self, s: &Statement) -> Result<Statement, String> {
        let frame_local = |l: &Local| self.me.fields.contains_key(l);
        let kind = match &s.kind {
            StatementKind::Assign(p, rv) => StatementKind::Assign(self.place(p)?, self.rvalue(rv)?),
            StatementKind::StorageLive(l) | StatementKind::StorageDead(l) if frame_local(l) => {
                StatementKind::Nop
            }
            StatementKind::StorageLive(l) => StatementKind::StorageLive(Local(l.0 + 2)),
            StatementKind::StorageDead(l) => StatementKind::StorageDead(Local(l.0 + 2)),
            StatementKind::SetDiscriminant(p, v) => {
                StatementKind::SetDiscriminant(self.place(p)?, *v)
            }
            StatementKind::BorrowFlag(op) => StatementKind::BorrowFlag(match op {
                FlagOp::Acquire { place, kind, loan } => FlagOp::Acquire {
                    place: self.place(place)?,
                    kind: *kind,
                    loan: *loan,
                },
                FlagOp::Release { loan } => FlagOp::Release { loan: *loan },
                FlagOp::Check { place, kind } => FlagOp::Check {
                    place: self.place(place)?,
                    kind: *kind,
                },
            }),
            StatementKind::FreeBox(p) => StatementKind::FreeBox(self.place(p)?),
            StatementKind::Nop => StatementKind::Nop,
        };
        Ok(Statement {
            kind,
            source_info: s.source_info,
        })
    }

    fn build(mut self) -> Result<Body, String> {
        let name = &self.body.instance.name;
        let b = |bb: BasicBlock| BasicBlock(bb.0 + 1);
        let frame_ref = self.tys.intern(TyKind::MutRef(self.me.ty));
        // `_0: Poll[T]`, `_1: mut ref F#Frame`, then the original locals.
        self.locals.push(LocalDecl {
            ty: self.me.poll,
            mutability: Mutability::Mut,
            kind: LocalKind::ReturnPlace,
            source_info: self.info(),
        });
        self.locals.push(LocalDecl {
            ty: frame_ref,
            mutability: Mutability::Not,
            kind: LocalKind::Arg {
                name: "frame".into(),
                node: NodeId(0),
            },
            source_info: self.info(),
        });
        for d in &self.body.locals {
            let mut d = d.clone();
            if matches!(d.kind, LocalKind::ReturnPlace | LocalKind::Arg { .. }) {
                d.kind = LocalKind::Temp;
            }
            self.locals.push(d);
        }
        // bb0 dispatches on the state; the original blocks follow.
        self.blocks.push(BasicBlockData {
            statements: vec![],
            terminator: Terminator {
                kind: TerminatorKind::Unreachable,
                source_info: self.info(),
            },
        });
        let points: BTreeMap<BasicBlock, usize> = self
            .layout
            .points
            .iter()
            .enumerate()
            .map(|(k, p)| (p.block, k))
            .collect();
        let mut resumes: Vec<(u128, BasicBlock)> = Vec::new();
        let mut extra: Vec<BasicBlockData> = Vec::new();
        let first_extra = 1 + self.body.blocks.len() as u32;
        for (i, data) in self.body.blocks.iter().enumerate() {
            let mut statements = data
                .statements
                .iter()
                .map(|s| self.statement(s))
                .collect::<Result<Vec<_>, _>>()?;
            let t = &data.terminator;
            let kind = match &t.kind {
                TerminatorKind::Call {
                    args,
                    destination,
                    target: Some(next),
                    ..
                } if points.contains_key(&BasicBlock(i as u32)) => {
                    let k = points[&BasicBlock(i as u32)];
                    let state = k as u32 + 1;
                    let callee = &self.layout.points[k].callee;
                    let dest = self.place(destination)?;
                    let at =
                        |extra: &Vec<BasicBlockData>| BasicBlock(first_extra + extra.len() as u32);
                    match self.frames.get(callee) {
                        None => {
                            // `__yield_now` is pending once, then `()`. The
                            // other suspending natives are behind readiness
                            // loops, whose `__yield_now` this is.
                            if callee != YIELD_NOW {
                                return Err(format!("{callee} suspends without a readiness loop"));
                            }
                            let unit = self.tys.unit();
                            let back = at(&extra);
                            let resumed = BasicBlockData {
                                statements: vec![self.assign(
                                    dest,
                                    Rvalue::Use(Operand::Const(Const {
                                        ty: unit,
                                        kind: ConstKind::Unit,
                                    })),
                                )],
                                terminator: Terminator {
                                    kind: TerminatorKind::Goto { target: b(*next) },
                                    source_info: t.source_info,
                                },
                            };
                            extra.push(resumed);
                            resumes.push((state as u128, back));
                            self.blocks.push(self.pending(statements, state));
                            continue;
                        }
                        Some(callee_frame) => {
                            let callee_body_ret = match self.tys.kind(callee_frame.poll) {
                                TyKind::Adt(a) => self.tys.adt(a).variants[0].fields[0].1,
                                _ => unreachable!("Poll is an ADT"),
                            };
                            let sub_f = self.me.subs[&k];
                            let sub = self.field(sub_f, callee_frame.ty);
                            // Start the callee: its state and its arguments.
                            let mut start = sub.clone();
                            start
                                .projection
                                .push(ProjElem::Field(FieldIdx(0), self.tys.int(IntTy::U32)));
                            statements.push(self.assign(start, Rvalue::Use(self.u32_const(0))));
                            for (j, a) in args.iter().enumerate() {
                                let l = Local(j as u32 + 1);
                                let f = callee_frame.fields[&l];
                                let ty = match self.tys.kind(callee_frame.ty) {
                                    TyKind::Adt(id) => {
                                        self.tys.adt(id).variants[0].fields[f as usize].1
                                    }
                                    _ => unreachable!(),
                                };
                                let mut p = sub.clone();
                                p.projection.push(ProjElem::Field(FieldIdx(f), ty));
                                statements.push(self.assign(p, Rvalue::Use(self.operand(a)?)));
                            }
                            let r = self.temp(callee_frame.poll);
                            let tref = self.temp(self.tys.intern(TyKind::MutRef(callee_frame.ty)));
                            let d = self.temp(self.tys.int(IntTy::I64));
                            let poll = at(&extra);
                            let check = BasicBlock(poll.0 + 1);
                            let ready = BasicBlock(poll.0 + 2);
                            let pend = BasicBlock(poll.0 + 3);
                            let unit = self.tys.unit();
                            extra.push(BasicBlockData {
                                statements: vec![self.assign(
                                    Place::local(tref),
                                    Rvalue::Ref(BorrowKind::Mut, sub.clone()),
                                )],
                                terminator: Terminator {
                                    kind: TerminatorKind::Call {
                                        func: Operand::Const(Const {
                                            ty: unit,
                                            kind: ConstKind::FnDef(InstanceId {
                                                def: DefId(0),
                                                args: vec![],
                                                name: resume_name(callee),
                                            }),
                                        }),
                                        args: vec![Operand::Move(Place::local(tref))],
                                        destination: Place::local(r),
                                        target: Some(check),
                                        unwind: UnwindAction::Abort,
                                    },
                                    source_info: t.source_info,
                                },
                            });
                            extra.push(BasicBlockData {
                                statements: vec![self.assign(
                                    Place::local(d),
                                    Rvalue::Discriminant(Place::local(r)),
                                )],
                                terminator: Terminator {
                                    kind: TerminatorKind::SwitchInt {
                                        discr: Operand::Copy(Place::local(d)),
                                        targets: SwitchTargets {
                                            values: vec![(0, ready)],
                                            otherwise: pend,
                                        },
                                    },
                                    source_info: t.source_info,
                                },
                            });
                            // A returned callee owns nothing, so a boxed
                            // (recursive) callee frame is freed until the
                            // next call makes it again.
                            let free = Statement {
                                kind: StatementKind::FreeBox(sub.clone()),
                                source_info: self.info(),
                            };
                            extra.push(BasicBlockData {
                                statements: vec![
                                    self.assign(
                                        dest,
                                        Rvalue::Use(Operand::Move(Place {
                                            local: r,
                                            projection: vec![
                                                ProjElem::Downcast(VariantIdx(0)),
                                                ProjElem::Field(FieldIdx(0), callee_body_ret),
                                            ],
                                        })),
                                    ),
                                    free,
                                ],
                                terminator: Terminator {
                                    kind: TerminatorKind::Goto { target: b(*next) },
                                    source_info: t.source_info,
                                },
                            });
                            extra.push(self.pending(vec![], state));
                            resumes.push((state as u128, poll));
                            TerminatorKind::Goto { target: poll }
                        }
                    }
                }
                TerminatorKind::Return => {
                    let ret = self.local(Local::RETURN_PLACE)?;
                    statements.push(self.assign(
                        Place::local(Local::RETURN_PLACE),
                        Rvalue::Aggregate(
                            AggregateKind::Adt {
                                ty: self.me.poll,
                                variant: VariantIdx(0),
                            },
                            vec![Operand::Move(ret)],
                        ),
                    ));
                    statements.push(
                        self.assign(self.state(), Rvalue::Use(self.u32_const(self.me.returned))),
                    );
                    TerminatorKind::Return
                }
                TerminatorKind::Goto { target } => TerminatorKind::Goto { target: b(*target) },
                TerminatorKind::SwitchInt { discr, targets } => TerminatorKind::SwitchInt {
                    discr: self.operand(discr)?,
                    targets: SwitchTargets {
                        values: targets.values.iter().map(|&(v, t)| (v, b(t))).collect(),
                        otherwise: b(targets.otherwise),
                    },
                },
                TerminatorKind::Call {
                    func,
                    args,
                    destination,
                    target,
                    unwind,
                } => TerminatorKind::Call {
                    func: self.operand(func)?,
                    args: args
                        .iter()
                        .map(|a| self.operand(a))
                        .collect::<Result<_, _>>()?,
                    destination: self.place(destination)?,
                    target: target.map(b),
                    unwind: *unwind,
                },
                TerminatorKind::Drop {
                    place,
                    target,
                    unwind,
                } => TerminatorKind::Drop {
                    place: self.place(place)?,
                    target: b(*target),
                    unwind: *unwind,
                },
                k @ (TerminatorKind::Abort { .. } | TerminatorKind::Unreachable) => k.clone(),
            };
            self.blocks.push(BasicBlockData {
                statements,
                terminator: Terminator {
                    kind,
                    source_info: t.source_info,
                },
            });
        }
        self.blocks.extend(extra);
        let resumed_after_return = BasicBlock(self.blocks.len() as u32);
        self.blocks.push(BasicBlockData {
            statements: vec![],
            terminator: Terminator {
                kind: TerminatorKind::Abort {
                    reason: AbortReason::Panic,
                },
                source_info: self.info(),
            },
        });
        let mut values = vec![(0, BasicBlock(1))];
        values.extend(resumes);
        self.blocks[0].terminator.kind = TerminatorKind::SwitchInt {
            discr: Operand::Copy(self.state()),
            targets: SwitchTargets {
                values,
                otherwise: resumed_after_return,
            },
        };
        let mut instance = self.body.instance.clone();
        instance.name = resume_name(name);
        Ok(Body {
            instance,
            locals: self.locals,
            arg_count: 1,
            blocks: self.blocks,
            scopes: self.body.scopes.clone(),
            phase: self.body.phase,
            span: self.body.span,
            provided: Vec::new(),
            auto_par_candidates: Vec::new(),
            par_regions: self
                .body
                .par_regions
                .iter()
                .map(|r| ParRegion {
                    kind: r.kind.clone(),
                    span: r.span,
                    branches: r
                        .branches
                        .iter()
                        .map(|br| br.iter().map(|&x| b(x)).collect())
                        .collect(),
                })
                .collect(),
            suspends: false,
            drop_flags: Vec::new(),
        })
    }
}

/// What `drop_frame` does to one place of a suspended frame.
enum Release {
    Drop(Place),
    /// Drop the place when the flag field is set.
    Guarded(Place, Place),
    /// Drop the callee's frame through `G#drop_frame`.
    Callee(String, Place, Ty),
}

/// `F#drop_frame(frame: mut ref F#Frame)`: drops what a frame suspended at
/// each point still owns, innermost first (the callee's frame, then the
/// locals in reverse order), and marks the frame returned. A frame that has
/// not started or has returned owns nothing.
///
/// Which places are initialized at a point comes from the same dataflow
/// drop elaboration uses: a place that is initialized on every path is
/// dropped, one that is on some paths is dropped under the flag
/// elaboration made for it, and one with no flag of its own is opened into
/// its parts, as elaboration opens it.
fn drop_frame(
    body: &Body,
    layout: &CoroutineLayout,
    frames: &BTreeMap<String, FrameInfo>,
    tys: &TyInterner,
) -> Result<Body, String> {
    use super::elaborate::{
        apply, dataflow, entry_state, gather_move_paths, statement_effects, terminator_effects,
        Effect, MovePaths, PathIdx, State,
    };
    let name = &body.instance.name;
    let r = Resume::new(body, layout, frames, tys);
    let me = r.me;
    let paths = gather_move_paths(body, tys, &|t| tys.needs_drop(t))?;
    let stmt_effects: Vec<Vec<Vec<Effect>>> = body
        .blocks
        .iter()
        .map(|b| {
            b.statements
                .iter()
                .map(|s| statement_effects(&paths, &s.kind))
                .collect()
        })
        .collect();
    let term_effects: Vec<Vec<Effect>> = body
        .blocks
        .iter()
        .map(|b| terminator_effects(&paths, &b.terminator.kind))
        .collect();
    let states = dataflow(
        body,
        &paths,
        &entry_state(body, &paths),
        &stmt_effects,
        &term_effects,
    );
    fn release(
        r: &Resume,
        paths: &MovePaths,
        st: &State,
        p: PathIdx,
        out: &mut Vec<Release>,
    ) -> Result<(), String> {
        let place = paths.place(p);
        if st.init[p] && !st.uninit[p] {
            out.push(Release::Drop(r.place(place)?));
        } else if st.init[p] {
            match r.body.drop_flags.iter().find(|(pl, _)| pl == place) {
                Some((_, flag)) => {
                    if !r.me.fields.contains_key(flag) {
                        return Err(format!(
                            "{}: the drop flag of {place:?} is not in the frame",
                            r.body.instance.name
                        ));
                    }
                    out.push(Release::Guarded(r.local(*flag)?, r.place(place)?));
                }
                None => {
                    for &c in paths.children(p).iter().rev() {
                        release(r, paths, st, c, out)?;
                    }
                }
            }
        }
        Ok(())
    }
    let mut per_state: Vec<Vec<Release>> = Vec::new();
    for (k, pt) in layout.points.iter().enumerate() {
        let bi = pt.block.index();
        let mut st = states[bi].clone();
        for effs in &stmt_effects[bi] {
            apply(&paths, &mut st, effs);
        }
        // The call's moves have happened; its destination is not written
        // yet.
        let moved: Vec<Effect> = term_effects[bi]
            .iter()
            .copied()
            .filter(|e| matches!(e, Effect::Uninit(_)))
            .collect();
        apply(&paths, &mut st, &moved);
        let mut out = Vec::new();
        if let Some(callee) = frames.get(&pt.callee) {
            out.push(Release::Callee(
                pt.callee.clone(),
                r.field(me.subs[&k], callee.ty),
                callee.ty,
            ));
        }
        for &l in pt.across.iter().rev() {
            if let Some(p) = paths.lookup(&Place::local(l)) {
                release(&r, &paths, &st, p, &mut out)?;
            }
        }
        per_state.push(out);
    }

    let mut r = r;
    let unit = tys.unit();
    r.locals.push(LocalDecl {
        ty: unit,
        mutability: Mutability::Mut,
        kind: LocalKind::ReturnPlace,
        source_info: r.info(),
    });
    r.locals.push(LocalDecl {
        ty: tys.intern(TyKind::MutRef(me.ty)),
        mutability: Mutability::Not,
        kind: LocalKind::Arg {
            name: "frame".into(),
            node: NodeId(0),
        },
        source_info: r.info(),
    });
    let block = |r: &Resume, statements, kind| BasicBlockData {
        statements,
        terminator: Terminator {
            kind,
            source_info: r.info(),
        },
    };
    // bb0 dispatches, bb1 marks the frame returned.
    r.blocks
        .push(block(&r, vec![], TerminatorKind::Unreachable));
    let done = vec![
        r.assign(r.state(), Rvalue::Use(r.u32_const(me.returned))),
        r.assign(
            Place::local(Local::RETURN_PLACE),
            Rvalue::Use(Operand::Const(Const {
                ty: unit,
                kind: ConstKind::Unit,
            })),
        ),
    ];
    r.blocks.push(block(&r, done, TerminatorKind::Return));
    let mut values = Vec::new();
    for (k, rel) in per_state.into_iter().enumerate() {
        // Built back to front: each release continues to the next one.
        let mut next = BasicBlock(1);
        for a in rel.into_iter().rev() {
            let at = BasicBlock(r.blocks.len() as u32);
            match a {
                Release::Drop(place) => {
                    let kind = TerminatorKind::Drop {
                        place,
                        target: next,
                        unwind: UnwindAction::Abort,
                    };
                    r.blocks.push(block(&r, vec![], kind));
                }
                Release::Guarded(flag, place) => {
                    let kind = TerminatorKind::Drop {
                        place,
                        target: next,
                        unwind: UnwindAction::Abort,
                    };
                    r.blocks.push(block(&r, vec![], kind));
                    let kind = TerminatorKind::SwitchInt {
                        discr: Operand::Copy(flag),
                        targets: SwitchTargets::if_else(at, next),
                    };
                    r.blocks.push(block(&r, vec![], kind));
                }
                Release::Callee(callee, sub, ty) => {
                    // Then the callee's frame box, when it is boxed.
                    let free = Statement {
                        kind: StatementKind::FreeBox(sub.clone()),
                        source_info: r.info(),
                    };
                    r.blocks
                        .push(block(&r, vec![free], TerminatorKind::Goto { target: next }));
                    let next = BasicBlock(r.blocks.len() as u32 - 1);
                    let t = r.temp(tys.intern(TyKind::MutRef(ty)));
                    let u = r.temp(unit);
                    let kind = TerminatorKind::Call {
                        func: Operand::Const(Const {
                            ty: unit,
                            kind: ConstKind::FnDef(InstanceId {
                                def: DefId(0),
                                args: vec![],
                                name: drop_frame_name(&callee),
                            }),
                        }),
                        args: vec![Operand::Move(Place::local(t))],
                        destination: Place::local(u),
                        target: Some(next),
                        unwind: UnwindAction::Abort,
                    };
                    let stmts = vec![r.assign(Place::local(t), Rvalue::Ref(BorrowKind::Mut, sub))];
                    r.blocks.push(block(&r, stmts, kind));
                }
            }
            next = BasicBlock(r.blocks.len() as u32 - 1);
        }
        values.push((k as u128 + 1, next));
    }
    r.blocks[0].terminator.kind = TerminatorKind::SwitchInt {
        discr: Operand::Copy(r.state()),
        targets: SwitchTargets {
            values,
            otherwise: BasicBlock(1),
        },
    };
    let mut instance = body.instance.clone();
    instance.name = drop_frame_name(name);
    Ok(Body {
        instance,
        locals: r.locals,
        arg_count: 1,
        blocks: r.blocks,
        scopes: body.scopes.clone(),
        phase: body.phase,
        span: body.span,
        auto_par_candidates: Vec::new(),
        par_regions: Vec::new(),
        provided: Vec::new(),
        suspends: false,
        drop_flags: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_module;

    const SRC: &str = "
fn leaf() -> () suspends {
    let mut _0: ();
    let _1: ();
    bb0: {
        _1 = __yield_now() -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn mid(_1: i64) -> i64 {
    let mut _0: i64;
    let _2: i64;
    let _3: ();
    let _4: ref i64;
    let _5: i64;
    bb0: {
        _2 = Add(copy _1, const 1_i64);
        _4 = &_2;
        _3 = leaf() -> bb1;
    }
    bb1: {
        _5 = copy (*_4);
        _0 = Add(copy _5, copy _1);
        return;
    }
}

fn plain(_1: i64) -> i64 {
    let mut _0: i64;
    bb0: {
        _0 = copy _1;
        return;
    }
}

fn top() -> i64 {
    let mut _0: i64;
    let _1: i64;
    bb0: {
        _1 = plain(const 2_i64) -> bb1;
    }
    bb1: {
        _0 = mid(copy _1) -> bb2;
    }
    bb2: {
        return;
    }
}
";

    #[test]
    fn coroutine_set_is_transitive_over_direct_calls() {
        let m = parse_module(SRC).unwrap();
        let set = coroutines(&m.bodies);
        let names: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
        assert_eq!(names, ["leaf", "mid", "top"]);
        assert!(recursive_cycles(&m.bodies, &set).is_empty());
    }

    #[test]
    fn frame_holds_what_is_live_across_and_what_is_borrowed() {
        let m = parse_module(SRC).unwrap();
        let set = coroutines(&m.bodies);
        let mid = layout(m.body("mid").unwrap(), &set);
        assert_eq!(mid.points.len(), 1);
        let p = &mid.points[0];
        assert_eq!(
            (p.block, p.callee.as_str(), p.resume),
            (BasicBlock(0), "leaf", BasicBlock(1))
        );
        // `_1` and the reference `_4` are used after; `_3`, the destination,
        // is written by the resume.
        assert_eq!(p.across, [Local(1), Local(4)]);
        // `_2` is not used again, but `_4` points at it.
        assert_eq!(mid.frame, [Local(1), Local(2), Local(4)]);

        let top = layout(m.body("top").unwrap(), &set);
        // `plain` is not a coroutine, so only the call to `mid` suspends;
        // its argument is moved into the call and nothing else is live.
        assert_eq!(top.points.len(), 1);
        assert_eq!(top.points[0].callee, "mid");
        assert!(top.frame.is_empty(), "{:?}", top.frame);

        let leaf = layout(m.body("leaf").unwrap(), &set);
        assert_eq!(leaf.points[0].callee, "__yield_now");
        assert!(leaf.frame.is_empty());
        assert!(layout(m.body("plain").unwrap(), &set).points.is_empty());
    }

    #[test]
    fn recursive_coroutines_are_found() {
        let src = "
fn a() -> () suspends {
    let mut _0: ();
    bb0: {
        _0 = b() -> bb1;
    }
    bb1: {
        return;
    }
}

fn b() -> () {
    let mut _0: ();
    bb0: {
        _0 = a() -> bb1;
    }
    bb1: {
        return;
    }
}

fn c() -> () {
    let mut _0: ();
    bb0: {
        _0 = c() -> bb1;
    }
    bb1: {
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let set = coroutines(&m.bodies);
        // `c` recurses but never suspends, so it is not a coroutine.
        assert_eq!(
            set.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            recursive_cycles(&m.bodies, &set),
            [vec!["a".to_string(), "b".to_string()]]
        );
    }

    const LOOP: &str = "
fn leaf(_1: i64) -> i64 suspends {
    let mut _0: i64;
    let _2: ();
    let _3: ref i64;
    let _4: i64;
    bb0: {
        _3 = &_1;
        _2 = __yield_now() -> bb1;
    }
    bb1: {
        _4 = copy (*_3);
        _0 = Add(copy _4, const 10_i64);
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: i64;
    let _2: bool;
    let _3: i64;
    let _4: ();
    bb0: {
        _1 = const 0_i64;
        goto -> bb1;
    }
    bb1: {
        _2 = Lt(copy _1, const 3_i64);
        switchInt(copy _2) -> [0: bb5, otherwise: bb2];
    }
    bb2: {
        _3 = leaf(copy _1) -> bb3;
    }
    bb3: {
        _4 = println(copy _3) -> bb4;
    }
    bb4: {
        _1 = Add(copy _1, const 1_i64);
        goto -> bb1;
    }
    bb5: {
        _0 = const ();
        return;
    }
}
";

    /// The executor runs the state machine to the output the synchronous
    /// run prints, resuming once per yield: a loop variable and a borrowed
    /// argument live in the frames across each one.
    #[test]
    fn executor_matches_the_synchronous_run() {
        use crate::mir::interp::{run, run_coroutines, Program};
        let m = parse_module(LOOP).unwrap();
        let prog = Program::from_module(&m);
        let sync = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(sync.output, "10\n11\n12\n", "{:?}", sync.outcome);
        let tasks = run_coroutines(&prog, &m.tys, "main", vec![]);
        assert_eq!(tasks.output, sync.output, "{:?}", tasks.outcome);
        assert_eq!(tasks.exit_code(), Some(0), "{:?}", tasks.outcome);
        assert_eq!(tasks.resumes, 3);

        let (bodies, frames) = transform(&m.bodies, &m.tys).unwrap();
        let names: BTreeSet<&str> = bodies.iter().map(|b| b.instance.name.as_str()).collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "leaf#drop_frame",
                "leaf#resume",
                "main#drop_frame",
                "main#resume"
            ])
        );
        // `main` keeps `i` and `leaf`'s frame; `leaf` keeps its argument
        // and the reference to it.
        assert_eq!(
            frames["main"].fields.keys().copied().collect::<Vec<_>>(),
            [Local(1)]
        );
        assert_eq!(frames["main"].subs.len(), 1);
        assert_eq!(
            frames["leaf"].fields.keys().copied().collect::<Vec<_>>(),
            [Local(1), Local(3)]
        );
    }

    /// A recursive coroutine's frame holds a frame of its own type, which
    /// the compiled backends keep in a box.
    #[test]
    fn transform_gives_a_recursive_coroutine_a_frame_of_its_own_type() {
        let src = "
fn a() -> () suspends {
    let mut _0: ();
    bb0: {
        _0 = a() -> bb1;
    }
    bb1: {
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let (_, frames) = transform(&m.bodies, &m.tys).unwrap();
        let a = &frames["a"];
        let sub = a.subs[&0];
        assert_eq!(m.tys.field_ty(a.ty, None, sub), Some(a.ty));
        assert!(crate::mir::layout::boxed_field(&m.tys, a.ty, None, sub));
    }

    /// A value with a Drop body held across a yield is dropped once, after
    /// the resume, in both the caller's and the callee's frame.
    #[test]
    fn drops_across_a_yield_run_once() {
        use crate::mir::interp::{run, run_coroutines, Program};
        let src = "
struct R: Drop { id: i64 }

fn R.drop(_1: mut ref R) -> () {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = println(const \"drop\", copy (*_1).0) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn leaf(_1: R) -> () suspends {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = __yield_now() -> bb1;
    }
    bb1: {
        _2 = println(const \"leaf\", copy _1.0) -> bb2;
    }
    bb2: {
        drop(_1) -> bb3;
    }
    bb3: {
        _0 = const ();
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: R;
    let _2: R;
    let _3: ();
    bb0: {
        _1 = R { const 8_i64 };
        _2 = R { const 7_i64 };
        _0 = leaf(move _2) -> bb1;
    }
    bb1: {
        _3 = println(const \"main\", copy _1.0) -> bb2;
    }
    bb2: {
        drop(_1) -> bb3;
    }
    bb3: {
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let prog = Program::from_module(&m);
        let sync = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(
            sync.output, "leaf7\ndrop7\nmain8\ndrop8\n",
            "{:?}",
            sync.outcome
        );
        let tasks = run_coroutines(&prog, &m.tys, "main", vec![]);
        assert_eq!(tasks.output, sync.output, "{:?}", tasks.outcome);
        assert_eq!(tasks.exit_code(), Some(0), "{:?}", tasks.outcome);
        assert_eq!(tasks.resumes, 1);
    }

    const FLAGGED: &str = "
struct R: Drop { id: i64 }

fn R.drop(_1: mut ref R) -> () {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = println(const \"drop\", copy (*_1).0) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn consume(_1: R) -> () {
    let mut _0: ();
    bb0: {
        drop(_1) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn leaf(_1: R, _2: bool) -> () suspends {
    let mut _0: ();
    let _3: ();
    bb0: {
        switchInt(copy _2) -> [0: bb2, otherwise: bb1];
    }
    bb1: {
        _3 = consume(move _1) -> bb2;
    }
    bb2: {
        _3 = __yield_now() -> bb3;
    }
    bb3: {
        _3 = println(const \"resumed\") -> bb4;
    }
    bb4: {
        drop(_1) -> bb5;
    }
    bb5: {
        _0 = const ();
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: R;
    let _2: R;
    bb0: {
        _1 = R { const 8_i64 };
        _2 = R { const 7_i64 };
        _0 = leaf(move _2, const MOVED) -> bb1;
    }
    bb1: {
        drop(_1) -> bb2;
    }
    bb2: {
        return;
    }
}
";

    /// A frame dropped while suspended drops what it owns at that point:
    /// the callee's frame first, then its own locals; a conditionally
    /// moved argument under the flag elaboration made for it.
    #[test]
    fn drop_frame_drops_a_suspended_frame() {
        use crate::mir::elaborate_drops;
        use crate::mir::interp::{run, run_coroutines, run_coroutines_cancelled, Program};
        for (moved, sync_out, cancelled_out) in [
            ("false", "resumed\ndrop7\ndrop8\n", "drop7\ndrop8\n"),
            ("true", "drop7\nresumed\ndrop8\n", "drop7\ndrop8\n"),
        ] {
            let mut m = parse_module(&FLAGGED.replace("MOVED", moved)).unwrap();
            for b in &mut m.bodies {
                elaborate_drops(b, &mut m.tys).unwrap();
            }
            let leaf = m.body("leaf").unwrap();
            assert_eq!(leaf.drop_flags.len(), 1, "{moved}: the flag for _1");
            let prog = Program::from_module(&m);
            let sync = run(&prog, &m.tys, "main", vec![]);
            assert_eq!(sync.output, sync_out, "{moved}: {:?}", sync.outcome);
            let tasks = run_coroutines(&prog, &m.tys, "main", vec![]);
            assert_eq!(tasks.output, sync_out, "{moved}: {:?}", tasks.outcome);
            assert_eq!(tasks.resumes, 1);
            let c = run_coroutines_cancelled(&prog, &m.tys, "main", vec![], 1);
            assert_eq!(c.output, cancelled_out, "{moved}: {:?}", c.outcome);
            assert_eq!(c.exit_code(), Some(0), "{moved}: {:?}", c.outcome);
        }
    }

    /// A borrow flag taken before a yield is still held after it: the
    /// second, shared borrow of the same field panics in both runs.
    #[test]
    fn borrow_flags_stay_held_across_a_yield() {
        use crate::mir::interp::{run, run_coroutines, Program};
        let src = "
struct Bag { items: Vec[i64] }

fn main() -> () suspends {
    let mut _0: ();
    let _1: shared Bag;
    let _2: Vec[i64];
    let _3: mut ref Vec[i64];
    let _4: ();
    let _5: ref Vec[i64];
    bb0: {
        _2 = Vec[i64].new() -> bb1;
    }
    bb1: {
        _1 = shared Bag { move _2 };
        flag_acquire(&mut _1.0, L0);
        _3 = &mut _1.0;
        _4 = __yield_now() -> bb2;
    }
    bb2: {
        flag_acquire(&_1.0, L1);
        _5 = &_1.0;
        _4 = println(const \"no conflict\") -> bb3;
    }
    bb3: {
        flag_release(L1);
        flag_release(L0);
        drop(_1) -> bb4;
    }
    bb4: {
        _0 = const ();
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let prog = Program::from_module(&m);
        let sync = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(
            (sync.output.as_str(), sync.exit_code()),
            ("", Some(101)),
            "{:?}",
            sync.outcome
        );
        let tasks = run_coroutines(&prog, &m.tys, "main", vec![]);
        assert_eq!(
            (tasks.output.as_str(), tasks.exit_code()),
            ("", Some(101)),
            "{:?}",
            tasks.outcome
        );
        assert_eq!(tasks.resumes, 1);
    }

    #[test]
    fn transform_refuses_a_coroutine_passed_to_a_native() {
        let src = "
fn leaf() -> () suspends {
    let mut _0: ();
    bb0: {
        _0 = __yield_now() -> bb1;
    }
    bb1: {
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    bb0: {
        _0 = run_later(const leaf) -> bb1;
    }
    bb1: {
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let e = transform(&m.bodies, &m.tys).unwrap_err();
        assert!(
            e.contains("leaf suspends and is passed to `run_later`"),
            "{e}"
        );
    }

    /// A suspending native other than `__yield_now` sits behind a readiness
    /// loop, which suspends at its `__yield_now` only while the call cannot
    /// complete; a frame cancelled there still owns what the call reads.
    #[test]
    fn a_suspending_native_waits_in_a_readiness_loop() {
        use crate::mir::interp::{run, run_coroutines, run_coroutines_cancelled, Program};
        let src = "
struct R: Drop { id: i64 }

fn R.drop(_1: mut ref R) -> () {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = println(const \"drop\", copy (*_1).0) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: R;
    let _2: ();
    let _3: i64;
    bb0: {
        _1 = R { const 5_i64 };
        _3 = const 20_i64;
        _2 = sleep_ms(copy _3) -> bb1;
    }
    bb1: {
        _2 = println(const \"woke\", copy _1.0) -> bb2;
    }
    bb2: {
        drop(_1) -> bb3;
    }
    bb3: {
        _0 = const ();
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let set = coroutines(&m.bodies);
        // Not a suspension point by itself.
        assert!(layout(m.body("main").unwrap(), &set).points.is_empty());
        let mut looped = m.body("main").unwrap().clone();
        insert_ready_loops(&mut looped, &m.tys).unwrap();
        let main = layout(&looped, &set);
        assert_eq!(main.points.len(), 1);
        assert_eq!(main.points[0].callee, YIELD_NOW);
        // The argument `_3` is read again by `__ready` and the call, after
        // the resume.
        assert_eq!(main.points[0].across, [Local(1), Local(3)]);
        let prog = Program::from_module(&m);
        let sync = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(sync.output, "woke5\ndrop5\n", "{:?}", sync.outcome);
        let tasks = run_coroutines(&prog, &m.tys, "main", vec![]);
        assert_eq!(tasks.output, sync.output, "{:?}", tasks.outcome);
        // The pause is not over at the first check.
        assert!(tasks.resumes >= 1, "{}", tasks.resumes);
        let c = run_coroutines_cancelled(&prog, &m.tys, "main", vec![], 1);
        assert_eq!(c.output, "drop5\n", "{:?}", c.outcome);
    }
}
