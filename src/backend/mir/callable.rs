//! Function values: the `Erase` cast that makes an erased `Fn` value, the
//! calls through a function item, closure or erased value, and the
//! adapters a native that takes a closure (`sort_by`, `sort_by_key`) hands
//! the runtime.
//!
//! An erased value is `{code, env, drop}` (`src/mir/layout.rs`). `code` is
//! a thunk `void(ptr ret, ptr env, i8 consume, ptr arg...)` built per body
//! and erased type: it calls the body with its environment (by reference,
//! or by value for a body that takes it so) and its arguments as the body
//! declares them. `env` is a heap block holding the closure's captures, or
//! null for a function item; `drop` drops them, or is null when they own
//! nothing. A call that consumes the value (`consume` 1) leaves the thunk
//! to drop and free the environment; the caller does not drop it again.
//! Reference semantics: the interpreter's `erase`, `call_erased` and
//! `call_callee` (`src/mir/interp.rs`).

use std::collections::HashMap;

use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::{coroutine, Body, Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

/// A deferred function: built after the bodies, like glue.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum Shim {
    /// The thunk of body `name` behind an erased value of type `Ty`.
    Thunk(String, Ty),
    /// `i32 (ptr ctx, ptr a, ptr b)` for a `sort_by` comparator of type
    /// `Ty`, or (`by_key`) the key order of a `sort_by_key` key function.
    SortBy { f: Ty, by_key: bool, el: Ty },
    /// `void (ptr buf, i64 n, ptr ctx)`: the runtime's stable merge sort
    /// (`karac_mir_sort_ctx`) over `n` elements of `size` bytes, emitted
    /// with a direct call to that comparator so LLVM can inline it.
    MergeSort {
        f: Ty,
        by_key: bool,
        el: Ty,
        size: u64,
    },
    /// `void (ptr buf, ptr tmp, i64 n, ptr ctx, i64 width, i64 limit)`: a
    /// [`Shim::MergeSort`]'s passes of `width`, `2 * width`, ... below
    /// `limit`, between `buf` and `tmp`, ending in `buf`.
    MergeWidths {
        f: Ty,
        by_key: bool,
        el: Ty,
        size: u64,
    },
    /// `void (ptr buf, i64 n, ptr ctx)`: a [`Shim::MergeSort`] that first
    /// splits a long input of few distinct keys by partitioning
    /// ([`Shim::Partition`]), as the interpreter's `sort_order` does.
    PartSort {
        f: Ty,
        by_key: bool,
        el: Ty,
        size: u64,
    },
    /// `void (ptr buf, ptr tmp, ptr tags, i64 lo, i64 hi, ptr ctx, i64
    /// depth)`: one stable three-way partition of `buf[lo..hi]` around its
    /// middle element, then the same on the less and greater parts.
    Partition {
        f: Ty,
        by_key: bool,
        el: Ty,
        size: u64,
    },
    /// `i8 (ptr a, ptr b)`: the poll function of a task whose root is the
    /// resume body `resume` (`task.rs`).
    TaskPoll { resume: String, spawned: bool },
}

#[derive(Default)]
pub(super) struct Shims<'ctx> {
    fns: HashMap<Shim, FunctionValue<'ctx>>,
    todo: Vec<(Shim, FunctionValue<'ctx>)>,
}

/// What a function-typed operand calls.
#[derive(Clone)]
pub(super) enum Callee<'ctx> {
    /// A body, with its environment's address when it is a closure (and
    /// whether the body takes the environment by value).
    Body {
        name: String,
        env: Option<(PointerValue<'ctx>, bool)>,
    },
    /// An erased value at this address; `consume` when moved into the call.
    Erased {
        at: PointerValue<'ctx>,
        ty: Ty,
        consume: bool,
    },
}

/// An argument: where its value is and the value's type at the call.
pub(super) type Arg<'ctx> = (PointerValue<'ctx>, Ty);

impl<'ctx> Cx<'ctx, '_> {
    pub(super) fn shims_pending(&self) -> bool {
        !self.shims.todo.is_empty()
    }

    pub(super) fn shim_fn(&mut self, s: Shim) -> FunctionValue<'ctx> {
        if let Some(&f) = self.shims.fns.get(&s) {
            return f;
        }
        let p = self.ptr_ty();
        let (name, ty) = match &s {
            Shim::Thunk(name, fty) => {
                let n = match self.tys.kind(*fty) {
                    TyKind::Fn { params, .. } => params.len(),
                    _ => 0,
                };
                let mut ps: Vec<BasicMetadataTypeEnum> =
                    vec![p.into(), p.into(), self.llcx.i8_type().into()];
                ps.extend((0..n).map(|_| BasicMetadataTypeEnum::from(p)));
                (
                    format!("thunk.{name}"),
                    self.llcx.void_type().fn_type(&ps, false),
                )
            }
            Shim::SortBy { f, by_key, .. } => (
                format!(
                    "{}.{}",
                    if *by_key { "sort_by_key" } else { "sort_by" },
                    self.tys.display(*f)
                ),
                self.llcx
                    .i32_type()
                    .fn_type(&[p.into(), p.into(), p.into()], false),
            ),
            Shim::MergeSort {
                f, by_key, size, ..
            } => (
                format!(
                    "merge_sort.{}.{}.{size}",
                    if *by_key { "by_key" } else { "by" },
                    self.tys.display(*f)
                ),
                self.llcx
                    .void_type()
                    .fn_type(&[p.into(), self.llcx.i64_type().into(), p.into()], false),
            ),
            Shim::MergeWidths {
                f, by_key, size, ..
            } => {
                let i = self.llcx.i64_type();
                (
                    format!(
                        "merge_widths.{}.{}.{size}",
                        if *by_key { "by_key" } else { "by" },
                        self.tys.display(*f)
                    ),
                    self.llcx.void_type().fn_type(
                        &[p.into(), p.into(), i.into(), p.into(), i.into(), i.into()],
                        false,
                    ),
                )
            }
            Shim::PartSort {
                f, by_key, size, ..
            } => (
                format!(
                    "part_sort.{}.{}.{size}",
                    if *by_key { "by_key" } else { "by" },
                    self.tys.display(*f)
                ),
                self.llcx
                    .void_type()
                    .fn_type(&[p.into(), self.llcx.i64_type().into(), p.into()], false),
            ),
            Shim::Partition {
                f, by_key, size, ..
            } => {
                let i = self.llcx.i64_type();
                (
                    format!(
                        "partition.{}.{}.{size}",
                        if *by_key { "by_key" } else { "by" },
                        self.tys.display(*f)
                    ),
                    self.llcx.void_type().fn_type(
                        &[
                            p.into(),
                            p.into(),
                            p.into(),
                            i.into(),
                            i.into(),
                            p.into(),
                            i.into(),
                            i.into(),
                            i.into(),
                        ],
                        false,
                    ),
                )
            }
            Shim::TaskPoll { resume, spawned } => (
                format!("poll.{}{resume}", if *spawned { "" } else { "root." }),
                self.llcx.i8_type().fn_type(&[p.into(), p.into()], false),
            ),
        };
        let f = self.module.add_function(&name, ty, Some(Linkage::Internal));
        self.shims.fns.insert(s.clone(), f);
        self.shims.todo.push((s, f));
        f
    }

    pub(super) fn finish_shims(&mut self) -> R<()> {
        while let Some((s, f)) = self.shims.todo.pop() {
            let entry = self.llcx.append_basic_block(f, "entry");
            self.builder.position_at_end(entry);
            let param = |i: u32| -> R<PointerValue<'ctx>> {
                Ok(f.get_nth_param(i)
                    .ok_or("a shim is missing a parameter")?
                    .into_pointer_value())
            };
            match s {
                Shim::Thunk(name, fty) => {
                    let TyKind::Fn { params, .. } = self.tys.kind(fty) else {
                        return Err("a thunk of a non-function type".into());
                    };
                    let (ret, env) = (param(0)?, param(1)?);
                    let consume = f
                        .get_nth_param(2)
                        .ok_or("a thunk has no consume flag")?
                        .into_int_value();
                    let args: Vec<Arg> = params
                        .iter()
                        .enumerate()
                        .map(|(i, t)| Ok((param(3 + i as u32)?, *t)))
                        .collect::<R<_>>()?;
                    if self.dyn_fns() {
                        self.build_dyn_thunk(&name, ret, env, consume, &args)?;
                    } else {
                        self.build_thunk(&name, ret, env, consume, &args)?;
                    }
                    self.builder.build_return(None).map_err(e)?;
                }
                Shim::SortBy { f: fty, by_key, el } => {
                    let (ctx, a, b) = (param(0)?, param(1)?, param(2)?);
                    let o = self.sort_by_order(fty, by_key, el, ctx, a, b)?;
                    self.builder.build_return(Some(&o)).map_err(e)?;
                }
                Shim::MergeSort {
                    f: fty,
                    by_key,
                    el,
                    size,
                } => {
                    let widths = self.shim_fn(Shim::MergeWidths {
                        f: fty,
                        by_key,
                        el,
                        size,
                    });
                    let n = f
                        .get_nth_param(1)
                        .ok_or("a merge sort has no length")?
                        .into_int_value();
                    self.build_merge_sort(widths, param(0)?, n, param(2)?, size)?;
                    self.builder.build_return(None).map_err(e)?;
                }
                Shim::MergeWidths {
                    f: fty,
                    by_key,
                    el,
                    size,
                } => {
                    let cmp = self.shim_fn(Shim::SortBy { f: fty, by_key, el });
                    let int = |i: u32| -> R<IntValue<'ctx>> {
                        Ok(f.get_nth_param(i)
                            .ok_or("merge passes are missing a parameter")?
                            .into_int_value())
                    };
                    let (n, width, limit) = (int(2)?, int(4)?, int(5)?);
                    self.build_merge_widths(
                        cmp,
                        param(0)?,
                        param(1)?,
                        n,
                        param(3)?,
                        width,
                        limit,
                        size,
                    )?;
                    self.builder.build_return(None).map_err(e)?;
                }
                Shim::PartSort {
                    f: fty,
                    by_key,
                    el,
                    size,
                } => {
                    let n = f
                        .get_nth_param(1)
                        .ok_or("a sort has no length")?
                        .into_int_value();
                    self.build_part_sort(fty, by_key, el, size, param(0)?, n, param(2)?)?;
                    self.builder.build_return(None).map_err(e)?;
                }
                Shim::Partition {
                    f: fty,
                    by_key,
                    el,
                    size,
                } => {
                    self.build_partition(f, fty, by_key, el, size)?;
                    self.builder.build_return(None).map_err(e)?;
                }
                Shim::TaskPoll { resume, spawned } => {
                    self.build_task_poll(f, &resume, spawned)?;
                }
            }
        }
        Ok(())
    }

    /// The body a closure or function-item type names.
    pub(super) fn body_of_def(&self, ty: Ty) -> R<&'_ Body> {
        let def = match self.tys.kind(ty) {
            TyKind::Closure(def, _) | TyKind::FnDef(def) => def,
            _ => return Err(format!("`{}` names no body", self.tys.display(ty))),
        };
        let program = self.program;
        program
            .bodies
            .values()
            .find(|b| b.instance.def == def)
            .ok_or_else(|| format!("no body for `{}`", self.tys.display(ty)))
    }

    /// Whether `body` (a closure's) takes its environment by value.
    fn env_by_value(&self, body: &Body) -> bool {
        body.args().next().is_some_and(|l| {
            !matches!(
                self.tys.kind(body.local(l).ty),
                TyKind::Ref(_) | TyKind::MutRef(_)
            )
        })
    }

    /// What the function-typed operand `op` calls.
    pub(super) fn callee_of(&mut self, op: &Operand) -> R<Callee<'ctx>> {
        if let Operand::Const(c) = op {
            if let crate::mir::ConstKind::FnDef(inst) = &c.kind {
                return Ok(Callee::Body {
                    name: inst.name.clone(),
                    env: None,
                });
            }
        }
        let mut ty = self.operand_type(op)?;
        let mut at = self.operand_ptr(op)?;
        let mut by_ref = false;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(ty) {
            at = self.load_ptr(at)?;
            ty = i;
            by_ref = true;
        }
        match self.tys.kind(ty) {
            TyKind::FnDef(_) => Ok(Callee::Body {
                name: self.body_of_def(ty)?.instance.name.clone(),
                env: None,
            }),
            TyKind::Closure(..) => {
                let body = self.body_of_def(ty)?;
                let by_value = self.env_by_value(body);
                Ok(Callee::Body {
                    name: body.instance.name.clone(),
                    env: Some((at, by_value && !by_ref)),
                })
            }
            TyKind::Fn { .. } => Ok(Callee::Erased {
                at,
                ty,
                consume: !by_ref && matches!(op, Operand::Move(_)),
            }),
            _ => Err(format!(
                "MIR to LLVM: a call through a `{}`",
                self.tys.display(ty)
            )),
        }
    }

    /// [`Self::callee_of`] for a callee called more than once: an erased
    /// value is called through a reference, never consumed, so the caller
    /// drops it once after the last call.
    pub(super) fn callee_by_ref(&mut self, op: &Operand) -> R<Callee<'ctx>> {
        Ok(match self.callee_of(op)? {
            Callee::Erased { at, ty, .. } => Callee::Erased {
                at,
                ty,
                consume: false,
            },
            c => c,
        })
    }

    /// The argument `a` as the parameter of type `want` takes it: a
    /// reference handed to a by-value parameter (`|a: i64, b: i64|` for a
    /// `Fn(ref T, ref T)`) passes a copy of what it points at.
    fn adapt_arg(&mut self, a: Arg<'ctx>, want: Ty) -> R<PointerValue<'ctx>> {
        let (p, t) = a;
        match self.tys.kind(t) {
            TyKind::Ref(inner) | TyKind::MutRef(inner)
                if !matches!(self.tys.kind(want), TyKind::Ref(_) | TyKind::MutRef(_))
                    && self.layout(t)?.size == 8 =>
            {
                let q = self.load_ptr(p)?;
                let tmp = self.temp(inner, "arg")?;
                self.copy(tmp, q, inner)?;
                Ok(tmp)
            }
            // A value handed to a borrowing parameter (`|c| c > 0` for a
            // `Fn(i64) -> bool`) is lent where it is.
            _ if matches!(self.tys.kind(want), TyKind::Ref(i) | TyKind::MutRef(i) if i == t)
                && self.layout(want)?.size == 8 =>
            {
                let slot = self.entry_slot(8, "arg")?;
                self.store(slot, p.into())?;
                Ok(slot)
            }
            _ => {
                let tmp = self.temp(t, "arg")?;
                self.copy(tmp, p, t)?;
                Ok(tmp)
            }
        }
    }

    /// Whether calling `c` disposes of the closure value it was made from
    /// (a by-value environment, or a consuming erased call), so the caller
    /// must not drop that value again.
    pub(super) fn consumes(c: &Callee<'ctx>) -> bool {
        match c {
            Callee::Body { env, .. } => matches!(env, Some((_, true))),
            Callee::Erased { consume, .. } => *consume,
        }
    }

    /// Calls `c` with `args`, writing its result at `ret`.
    pub(super) fn call_callee(
        &mut self,
        c: &Callee<'ctx>,
        ret: PointerValue<'ctx>,
        args: &[Arg<'ctx>],
    ) -> R<()> {
        match c {
            Callee::Body { name, env } => {
                let program = self.program;
                let body = program
                    .bodies
                    .get(name)
                    .ok_or_else(|| format!("no body named `{name}`"))?;
                let f = self.function(name)?;
                let mut vals: Vec<BasicMetadataValueEnum> = vec![ret.into()];
                let skip = usize::from(env.is_some());
                if let Some((p, by_value)) = env {
                    if *by_value {
                        vals.push((*p).into());
                    } else {
                        let slot = self.entry_slot(8, "env")?;
                        self.store(slot, (*p).into())?;
                        vals.push(slot.into());
                    }
                }
                for (i, a) in args.iter().enumerate() {
                    let want = body.locals[1 + skip + i].ty;
                    let q = self.adapt_arg(*a, want)?;
                    vals.push(q.into());
                }
                self.builder.build_call(f, &vals, "").map_err(e)?;
                Ok(())
            }
            Callee::Erased { at, ty, consume } => {
                let TyKind::Fn { params, .. } = self.tys.kind(*ty) else {
                    return Err("an erased callee of a non-function type".into());
                };
                let code = self.load_ptr(*at)?;
                let env = self.load_ptr(self.offset(*at, 8)?)?;
                let flag = self.llcx.i8_type().const_int(u64::from(*consume), false);
                let mut vals: Vec<BasicMetadataValueEnum> =
                    vec![ret.into(), env.into(), flag.into()];
                for (a, want) in args.iter().zip(params.iter()) {
                    let q = self.adapt_arg(*a, *want)?;
                    vals.push(q.into());
                }
                let p = self.ptr_ty();
                let mut ps: Vec<BasicMetadataTypeEnum> =
                    vec![p.into(), p.into(), self.llcx.i8_type().into()];
                ps.extend(params.iter().map(|_| BasicMetadataTypeEnum::from(p)));
                let fn_ty = self.llcx.void_type().fn_type(&ps, false);
                self.builder
                    .build_indirect_call(fn_ty, code, &vals, "")
                    .map_err(e)?;
                Ok(())
            }
        }
    }

    /// A `Call` whose function is a place or a non-`FnDef` operand.
    pub(super) fn call_value(&mut self, func: &Operand, args: &[Operand], dest: &Place) -> R<()> {
        let c = self.callee_of(func)?;
        let dty = self.place_type(dest)?;
        let ret = self.temp(dty, "ret")?;
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            let t = self.operand_type(a)?;
            let slot = self.temp(t, "arg")?;
            self.store_operand(slot, a)?;
            vals.push((slot, t));
        }
        self.call_callee(&c, ret, &vals)?;
        let d = self.place_ptr(dest)?;
        self.copy(d, ret, dty)
    }

    /// The thunk's body: calls `name` with the environment at `env` and
    /// the arguments, then (for a consuming call) disposes of the
    /// environment.
    fn build_thunk(
        &mut self,
        name: &str,
        ret: PointerValue<'ctx>,
        env: PointerValue<'ctx>,
        consume: inkwell::values::IntValue<'ctx>,
        args: &[Arg<'ctx>],
    ) -> R<()> {
        let program = self.program;
        let body = program
            .bodies
            .get(name)
            .ok_or_else(|| format!("no body named `{name}`"))?;
        let has_env = body.arg_count == args.len() + 1;
        if !has_env {
            let c = Callee::Body {
                name: name.to_string(),
                env: None,
            };
            return self.call_callee(&c, ret, args);
        }
        let by_value = self.env_by_value(body);
        let c = Callee::Body {
            name: name.to_string(),
            env: Some((env, by_value)),
        };
        self.call_callee(&c, ret, args)?;
        let env_ty = body.locals[1].ty;
        let clo = match self.tys.kind(env_ty) {
            TyKind::Ref(i) | TyKind::MutRef(i) => i,
            _ => env_ty,
        };
        if by_value {
            // The body took the captures; only their block is left.
            let live = self.builder.build_is_not_null(env, "").map_err(e)?;
            return self.if_then(live, |cx| cx.free(env));
        }
        let consumed = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                consume,
                consume.get_type().const_zero(),
                "",
            )
            .map_err(e)?;
        self.if_then(consumed, |cx| {
            if cx.tys.needs_drop(clo) {
                cx.call_glue(env, clo)?;
            }
            let live = cx.builder.build_is_not_null(env, "").map_err(e)?;
            cx.if_then(live, |cx| cx.free(env))
        })
    }

    /// Whether the program calls a suspending function through a value
    /// (M4 item 4): it has a `dstart` body, and every thunk then answers
    /// the modes of [`Self::build_dyn_thunk`].
    fn dyn_fns(&mut self) -> bool {
        if self.dyn_fns.is_none() {
            let any = self.program.bodies.keys().any(|n| coroutine::is_dstart(n));
            self.dyn_fns = Some(any);
        }
        self.dyn_fns == Some(true)
    }

    /// The thunk of `name` in a program that calls suspending functions
    /// through values. Its `consume` byte is a mode: 0 and 1 call `name`
    /// (by reference, or consuming the value) as [`Self::build_thunk`]
    /// does, 2 and 3 start it in a frame of its own (`coroutine::FN_START`)
    /// by calling its `dstart` the same way, and 4 writes at `ret` whether
    /// it suspends (`coroutine::FN_SUSPENDS`). A suspending `name` has no
    /// body to call synchronously, so modes 0 and 1 abort; a call from the
    /// program's own code asks first, and a native never gets one
    /// (`coroutine::passed_to_native`).
    fn build_dyn_thunk(
        &mut self,
        name: &str,
        ret: PointerValue<'ctx>,
        env: PointerValue<'ctx>,
        mode: IntValue<'ctx>,
        args: &[Arg<'ctx>],
    ) -> R<()> {
        let start = coroutine::dstart_name(name);
        let program = self.program;
        let suspends = program.bodies.get(&start);
        let f = self.cur_fn()?;
        let [call, by_ref, consuming, query, done] =
            ["call", "start", "start.own", "query", "done"]
                .map(|n| self.llcx.append_basic_block(f, n));
        let i8t = self.llcx.i8_type();
        self.builder
            .build_switch(
                mode,
                call,
                &[
                    (i8t.const_int(2, false), by_ref),
                    (i8t.const_int(3, false), consuming),
                    (i8t.const_int(4, false), query),
                ],
            )
            .map_err(e)?;
        self.builder.position_at_end(query);
        let answer = i8t.const_int(u64::from(suspends.is_some()), false);
        self.store(ret, answer.into())?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        for (at, own) in [(by_ref, false), (consuming, true)] {
            self.builder.position_at_end(at);
            match suspends {
                Some(b) if own && b.arg_count == args.len() + 1 && !self.env_by_value(b) => {
                    // The frame would borrow captures the call gives back.
                    self.abort_with(
                        "a suspending closure that borrows its captures, called by value \
                         through a function value, is not supported yet",
                    )?;
                }
                Some(_) => {
                    let flag = i8t.const_int(u64::from(own), false);
                    self.build_thunk(&start, ret, env, flag, args)?;
                    self.builder.build_unconditional_branch(done).map_err(e)?;
                }
                None => {
                    self.builder.build_unreachable().map_err(e)?;
                }
            }
        }
        self.builder.position_at_end(call);
        if suspends.is_some() {
            self.abort_with("a suspending function value was called where it cannot wait")?;
        } else {
            let one = self
                .builder
                .build_int_compare(IntPredicate::NE, mode, i8t.const_zero(), "")
                .map_err(e)?;
            let consume = self.builder.build_int_z_extend(one, i8t, "").map_err(e)?;
            self.build_thunk(name, ret, env, consume, args)?;
            self.builder.build_unconditional_branch(done).map_err(e)?;
        }
        self.builder.position_at_end(done);
        Ok(())
    }

    /// `coroutine::FN_SUSPENDS` and `coroutine::FN_START`: the call through
    /// the erased value `args[0]` in mode 4 (whether it suspends, written
    /// at `dest`), or 2 or 3 (its frame started, as an erased poller at
    /// `dest`), as [`Self::build_dyn_thunk`] reads them.
    pub(super) fn fn_value_native(&mut self, start: bool, args: &[Operand], dest: &Place) -> R<()> {
        let (f, rest) = args
            .split_first()
            .ok_or("a function-value native takes a value")?;
        let Callee::Erased { at, ty, consume } = self.callee_of(f)? else {
            return Err("a function-value native's operand is not erased".into());
        };
        let TyKind::Fn { params, .. } = self.tys.kind(ty) else {
            return Err("an erased callee of a non-function type".into());
        };
        let i8t = self.llcx.i8_type();
        let (mode, ret, args) = if start {
            let mut vals = Vec::with_capacity(rest.len());
            for a in rest {
                let t = self.operand_type(a)?;
                let slot = self.temp(t, "arg")?;
                self.store_operand(slot, a)?;
                vals.push((slot, t));
            }
            (2 + u64::from(consume), self.place_ptr(dest)?, vals)
        } else {
            (4, self.entry_slot(1, "suspends")?, Vec::new())
        };
        let code = self.load_ptr(at)?;
        let env = self.load_ptr(self.offset(at, 8)?)?;
        let mut vals: Vec<BasicMetadataValueEnum> =
            vec![ret.into(), env.into(), i8t.const_int(mode, false).into()];
        let p = self.ptr_ty();
        let null = p.const_null();
        for (i, want) in params.iter().enumerate() {
            vals.push(match args.get(i) {
                Some(a) => self.adapt_arg(*a, *want)?.into(),
                None => null.into(),
            });
        }
        let mut ps: Vec<BasicMetadataTypeEnum> = vec![p.into(), p.into(), i8t.into()];
        ps.extend(params.iter().map(|_| BasicMetadataTypeEnum::from(p)));
        let fn_ty = self.llcx.void_type().fn_type(&ps, false);
        self.builder
            .build_indirect_call(fn_ty, code, &vals, "")
            .map_err(e)?;
        if !start {
            let b = self
                .builder
                .build_load(i8t, ret, "")
                .map_err(e)?
                .into_int_value();
            let on = self
                .builder
                .build_int_compare(IntPredicate::NE, b, i8t.const_zero(), "")
                .map_err(e)?;
            self.store_bool(dest, on)?;
        }
        Ok(())
    }

    /// `coroutine::FRAME_START`: a frame of the resume body `args[0]` on
    /// the heap, zeroed (its first state), with the rest of `args` moved
    /// into its argument fields, as the erased poller at `dest` whose calls
    /// resume it and whose drop drops it through its `drop_frame`.
    pub(super) fn frame_start(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let (name, rest) = args
            .split_first()
            .ok_or("__frame_start takes a resume body")?;
        let resume = Self::str_operand(name)?.to_string();
        let (frame_ty, _) = self.task_types(&resume)?;
        let size = self.layout(frame_ty)?.size;
        let malloc = self.libc_fn("malloc");
        let frame = self
            .builder
            .build_call(malloc, &[self.i64(size.max(1)).into()], "frame")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memset(frame, 8, self.llcx.i8_type().const_zero(), self.i64(size))
            .map_err(e)?;
        for (j, a) in rest.iter().enumerate() {
            let at = self.offset(frame, self.field_offset(frame_ty, None, j as u32 + 1)?)?;
            self.store_operand(at, a)?;
        }
        let poller = self.place_type(dest)?;
        let thunk = self.shim_fn(Shim::Thunk(resume, poller));
        let drop = if self.tys.needs_drop(frame_ty) {
            self.glue_fn(frame_ty).as_global_value().as_pointer_value()
        } else {
            self.ptr_ty().const_null()
        };
        let d = self.place_ptr(dest)?;
        self.store(d, thunk.as_global_value().as_pointer_value().into())?;
        self.store(self.offset(d, 8)?, frame.into())?;
        self.store(self.offset(d, 16)?, drop.into())
    }

    /// The `Erase` cast: `from` as an erased value at `dst`.
    pub(super) fn erase_into(
        &mut self,
        dst: PointerValue<'ctx>,
        a: &Operand,
        from: Ty,
        to: Ty,
    ) -> R<()> {
        let null = self.ptr_ty().const_null();
        if matches!(self.tys.kind(from), TyKind::Fn { .. }) {
            let src = self.operand_ptr(a)?;
            return self.copy(dst, src, from);
        }
        let name = match a {
            Operand::Const(c) => match &c.kind {
                crate::mir::ConstKind::FnDef(inst) => inst.name.clone(),
                _ => return Err("erasing a constant that is not a function".into()),
            },
            // A coroutine's closure is named by the function its generated
            // bodies were made from.
            _ => coroutine::made_from(&self.body_of_def(from)?.instance.name).to_string(),
        };
        let thunk = self.shim_fn(Shim::Thunk(name, to));
        self.store(dst, thunk.as_global_value().as_pointer_value().into())?;
        let (env, drop) = match self.tys.kind(from) {
            TyKind::Closure(..) => {
                let size = self.layout(from)?.size;
                let malloc = self.libc_fn("malloc");
                let env = self
                    .builder
                    .build_call(malloc, &[self.i64(size.max(1)).into()], "env")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let src = self.operand_ptr(a)?;
                self.copy(env, src, from)?;
                let drop = if self.tys.needs_drop(from) {
                    self.glue_fn(from).as_global_value().as_pointer_value()
                } else {
                    null
                };
                (env, drop)
            }
            _ => (null, null),
        };
        self.store(self.offset(dst, 8)?, env.into())?;
        self.store(self.offset(dst, 16)?, drop.into())
    }

    /// The comparator adapter's body: the closure at `ctx` called on the
    /// elements `a` and `b` (by reference), as -1 / 0 / 1.
    fn sort_by_order(
        &mut self,
        fty: Ty,
        by_key: bool,
        el: Ty,
        ctx: PointerValue<'ctx>,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
    ) -> R<inkwell::values::IntValue<'ctx>> {
        // `ctx` is where the function value is (the closure itself, or an
        // erased value), as `callee_of` would find it.
        let c = match self.tys.kind(fty) {
            TyKind::Fn { .. } => Callee::Erased {
                at: ctx,
                ty: fty,
                consume: false,
            },
            TyKind::Closure(..) => Callee::Body {
                name: self.body_of_def(fty)?.instance.name.clone(),
                env: Some((ctx, false)),
            },
            _ => Callee::Body {
                name: self.body_of_def(fty)?.instance.name.clone(),
                env: None,
            },
        };
        let el_ref = self.tys.intern(TyKind::Ref(el));
        let (sa, sb) = (self.entry_slot(8, "a")?, self.entry_slot(8, "b")?);
        self.store(sa, a.into())?;
        self.store(sb, b.into())?;
        let ret_ty = self.callee_ret(&c)?;
        if !by_key {
            let ret = self.temp(ret_ty, "ordering")?;
            self.call_callee(&c, ret, &[(sa, el_ref), (sb, el_ref)])?;
            return self.ordering_value(ret, ret_ty);
        }
        // The keys of both elements, compared by the key type's order and
        // then dropped.
        let (ka, kb) = (self.temp(ret_ty, "key")?, self.temp(ret_ty, "key")?);
        self.call_callee(&c, ka, &[(sa, el_ref)])?;
        self.call_callee(&c, kb, &[(sb, el_ref)])?;
        let op = if self.user_cmp_body(ret_ty).is_some() {
            super::key::KeyOp::User
        } else {
            super::key::KeyOp::Cmp
        };
        let o = self.cmp_call(op, ka, kb, ret_ty)?;
        if self.tys.needs_drop(ret_ty) {
            self.call_glue(ka, ret_ty)?;
            self.call_glue(kb, ret_ty)?;
        }
        Ok(o)
    }

    /// The return type of what `c` calls.
    pub(super) fn callee_ret(&self, c: &Callee<'ctx>) -> R<Ty> {
        match c {
            Callee::Body { name, .. } => Ok(self.program.bodies[name].return_ty()),
            Callee::Erased { ty, .. } => match self.tys.kind(*ty) {
                TyKind::Fn { ret, .. } => Ok(ret),
                _ => Err("an erased callee of a non-function type".into()),
            },
        }
    }

    /// The comparator a `sort_by` / `sort_by_key` hands the runtime, and
    /// the context it is called with.
    pub(super) fn sort_by_args(
        &mut self,
        f: &Operand,
        by_key: bool,
        el: Ty,
    ) -> R<(PointerValue<'ctx>, PointerValue<'ctx>)> {
        let mut ty = self.operand_type(f)?;
        let mut at = self.operand_ptr(f)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(ty) {
            at = self.load_ptr(at)?;
            ty = i;
        }
        let cmp = self.shim_fn(Shim::SortBy { f: ty, by_key, el });
        Ok((cmp.as_global_value().as_pointer_value(), at))
    }

    /// `sort_by` / `sort_by_key` of the `n` elements of `el` at `buf` with
    /// the function operand `f`: the emitted merge sort, which calls the
    /// comparator directly (B-2026-10-13-18).
    pub(super) fn sort_with(
        &mut self,
        buf: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        size: u64,
        f: &Operand,
        by_key: bool,
        el: Ty,
    ) -> R<()> {
        if size == 0 {
            // Every element is the same zero-sized value.
            return Ok(());
        }
        let (_, ctx) = self.sort_by_args(f, by_key, el)?;
        let mut ty = self.operand_type(f)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(ty) {
            ty = i;
        }
        let sort = self.shim_fn(Shim::PartSort {
            f: ty,
            by_key,
            el,
            size,
        });
        self.builder
            .build_call(sort, &[buf.into(), n.into(), ctx.into()], "")
            .map_err(e)?;
        Ok(())
    }

    /// The body of a [`Shim::MergeSort`]: bottom-up merge passes of widths
    /// 1, 2, 4, ... between `buf` and one scratch buffer, the right element
    /// first only when strictly less. These are `merge_passes`' comparisons
    /// in its order (runtime/src/mir.rs), so the result is the
    /// interpreter's even for a comparator that is not an order.
    #[allow(clippy::too_many_arguments)]
    fn build_merge_widths(
        &mut self,
        cmp: FunctionValue<'ctx>,
        buf: PointerValue<'ctx>,
        scratch: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        ctx: PointerValue<'ctx>,
        width: IntValue<'ctx>,
        limit: IntValue<'ctx>,
        size: u64,
    ) -> R<()> {
        let b = &self.builder;
        let i64t = self.llcx.i64_type();
        let i8t = self.llcx.i8_type();
        let p = self.ptr_ty();
        let f = b
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("no function")?;
        let bb = |name: &str| self.llcx.append_basic_block(f, name);
        let (w_head, w_body, lo_head, lo_body) = (bb("w"), bb("w.body"), bb("run"), bb("run.body"));
        let (m_head, m_test, m_body, take_j, take_i) =
            (bb("m"), bb("m.right"), bb("m.cmp"), bb("m.j"), bb("m.i"));
        let (tail, w_next, finish, done) = (bb("run.tail"), bb("w.next"), bb("copy"), bb("done"));
        let sz = i64t.const_int(size, false);
        let one = i64t.const_int(1, false);
        let zero = i64t.const_zero();
        let at = |q: PointerValue<'ctx>, k: IntValue<'ctx>| -> R<PointerValue<'ctx>> {
            let off = b.build_int_mul(k, sz, "").map_err(e)?;
            // SAFETY of the address: `k` is below `n` (or equal, with a
            // zero-length copy), in a buffer of `n` elements.
            unsafe { b.build_in_bounds_gep(i8t, q, &[off], "") }.map_err(e)
        };
        let var =
            |name: &str, t: inkwell::types::BasicTypeEnum<'ctx>| b.build_alloca(t, name).map_err(e);
        let (v_src, v_dst, v_w, v_lo, v_k, v_i, v_j) = (
            var("src", p.into())?,
            var("dst", p.into())?,
            var("width", i64t.into())?,
            var("lo", i64t.into())?,
            var("k", i64t.into())?,
            var("i", i64t.into())?,
            var("j", i64t.into())?,
        );
        let ld = |v: PointerValue<'ctx>| -> R<IntValue<'ctx>> {
            Ok(b.build_load(i64t, v, "").map_err(e)?.into_int_value())
        };
        let ldp = |v: PointerValue<'ctx>| -> R<PointerValue<'ctx>> {
            Ok(b.build_load(p, v, "").map_err(e)?.into_pointer_value())
        };
        let lt = |x: IntValue<'ctx>, y: IntValue<'ctx>| {
            b.build_int_compare(IntPredicate::ULT, x, y, "").map_err(e)
        };
        let umin = |x: IntValue<'ctx>, y: IntValue<'ctx>| -> R<IntValue<'ctx>> {
            let c = lt(x, y)?;
            Ok(b.build_select(c, x, y, "").map_err(e)?.into_int_value())
        };
        let room = b.build_int_mul(n, sz, "").map_err(e)?;
        b.build_store(v_src, buf).map_err(e)?;
        b.build_store(v_dst, scratch).map_err(e)?;
        b.build_store(v_w, width).map_err(e)?;
        // A start at width 1 insertion-sorts runs of INSERTION_RUN in
        // place instead of the narrowest passes (interp.rs
        // `insertion_runs`): an element moves left past each it is
        // strictly less than.
        let run = i64t.const_int(crate::mir::interp::INSERTION_RUN as u64, false);
        let x = b
            .build_alloca(i8t.array_type(size as u32), "elem")
            .map_err(e)?;
        let (ins, r_head, r_body, k_head, k_body, s_head, s_test, s_body, s_done) = (
            bb("ins"),
            bb("ins.run"),
            bb("ins.run.body"),
            bb("ins.k"),
            bb("ins.k.body"),
            bb("ins.shift"),
            bb("ins.shift.cmp"),
            bb("ins.shift.body"),
            bb("ins.put"),
        );
        let first = b
            .build_int_compare(IntPredicate::EQ, width, one, "")
            .map_err(e)?;
        b.build_conditional_branch(first, ins, w_head).map_err(e)?;
        b.position_at_end(ins);
        b.build_store(v_lo, zero).map_err(e)?;
        b.build_unconditional_branch(r_head).map_err(e)?;
        // for lo in (0..n).step_by(RUN)
        b.position_at_end(r_head);
        let lo = ld(v_lo)?;
        let ins_done = bb("ins.done");
        b.build_conditional_branch(lt(lo, n)?, r_body, ins_done)
            .map_err(e)?;
        b.position_at_end(r_body);
        let lo = ld(v_lo)?;
        b.build_store(v_k, b.build_int_add(lo, one, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(k_head).map_err(e)?;
        // for k in lo + 1 .. min(lo + RUN, n)
        b.position_at_end(k_head);
        let (lo, k) = (ld(v_lo)?, ld(v_k)?);
        let hi = umin(b.build_int_add(lo, run, "").map_err(e)?, n)?;
        let r_next = bb("ins.run.next");
        b.build_conditional_branch(lt(k, hi)?, k_body, r_next)
            .map_err(e)?;
        b.position_at_end(r_next);
        b.build_store(v_lo, b.build_int_add(lo, run, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(r_head).map_err(e)?;
        b.position_at_end(k_body);
        let k = ld(v_k)?;
        b.build_memcpy(x, 1, at(buf, k)?, 1, sz).map_err(e)?;
        b.build_store(v_j, k).map_err(e)?;
        b.build_unconditional_branch(s_head).map_err(e)?;
        // while j > lo && cmp(x, buf[j - 1]) < 0
        b.position_at_end(s_head);
        let (lo, j) = (ld(v_lo)?, ld(v_j)?);
        b.build_conditional_branch(lt(lo, j)?, s_test, s_done)
            .map_err(e)?;
        b.position_at_end(s_test);
        let j = ld(v_j)?;
        let j1 = b.build_int_sub(j, one, "").map_err(e)?;
        let pj1 = at(buf, j1)?;
        let o = b
            .build_call(cmp, &[ctx.into(), x.into(), pj1.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let less = b
            .build_int_compare(IntPredicate::SLT, o, o.get_type().const_zero(), "")
            .map_err(e)?;
        b.build_conditional_branch(less, s_body, s_done)
            .map_err(e)?;
        b.position_at_end(s_body);
        let j = ld(v_j)?;
        let j1 = b.build_int_sub(j, one, "").map_err(e)?;
        b.build_memcpy(at(buf, j)?, 1, at(buf, j1)?, 1, sz)
            .map_err(e)?;
        b.build_store(v_j, j1).map_err(e)?;
        b.build_unconditional_branch(s_head).map_err(e)?;
        b.position_at_end(s_done);
        let j = ld(v_j)?;
        b.build_memcpy(at(buf, j)?, 1, x, 1, sz).map_err(e)?;
        let k = ld(v_k)?;
        b.build_store(v_k, b.build_int_add(k, one, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(k_head).map_err(e)?;
        // The merges start past the sorted runs.
        b.position_at_end(ins_done);
        b.build_store(v_w, run).map_err(e)?;
        b.build_unconditional_branch(w_head).map_err(e)?;
        // while width < limit
        b.position_at_end(w_head);
        let w = ld(v_w)?;
        b.build_conditional_branch(lt(w, limit)?, w_body, finish)
            .map_err(e)?;
        b.position_at_end(w_body);
        b.build_store(v_k, zero).map_err(e)?;
        b.build_store(v_lo, zero).map_err(e)?;
        b.build_unconditional_branch(lo_head).map_err(e)?;
        // for lo in (0..n).step_by(2 * width)
        b.position_at_end(lo_head);
        let lo = ld(v_lo)?;
        b.build_conditional_branch(lt(lo, n)?, lo_body, w_next)
            .map_err(e)?;
        b.position_at_end(lo_body);
        let w = ld(v_w)?;
        let lo = ld(v_lo)?;
        let mid = umin(b.build_int_add(lo, w, "").map_err(e)?, n)?;
        let w2 = b.build_int_add(w, w, "").map_err(e)?;
        let hi = umin(b.build_int_add(lo, w2, "").map_err(e)?, n)?;
        b.build_store(v_i, lo).map_err(e)?;
        b.build_store(v_j, mid).map_err(e)?;
        b.build_unconditional_branch(m_head).map_err(e)?;
        // while i < mid && j < hi
        b.position_at_end(m_head);
        b.build_conditional_branch(lt(ld(v_i)?, mid)?, m_test, tail)
            .map_err(e)?;
        b.position_at_end(m_test);
        b.build_conditional_branch(lt(ld(v_j)?, hi)?, m_body, tail)
            .map_err(e)?;
        b.position_at_end(m_body);
        let (src, dst) = (ldp(v_src)?, ldp(v_dst)?);
        let (i, j, k) = (ld(v_i)?, ld(v_j)?, ld(v_k)?);
        let (pj, pi, pk) = (at(src, j)?, at(src, i)?, at(dst, k)?);
        let o = b
            .build_call(cmp, &[ctx.into(), pj.into(), pi.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let less = b
            .build_int_compare(IntPredicate::SLT, o, o.get_type().const_zero(), "")
            .map_err(e)?;
        let k1 = b.build_int_add(k, one, "").map_err(e)?;
        b.build_store(v_k, k1).map_err(e)?;
        // Branch free: which side is taken is as random as the data, so
        // a branch on it mispredicts about half the time. LLVM turns a
        // `select` here back into a branch, so the choice is arithmetic on
        // a mask it cannot see through.
        let took_j = b.build_int_z_extend(less, i64t, "").map_err(e)?;
        let took_j = opaque_i64(self.llcx, b, took_j)?;
        let mask = b.build_int_sub(zero, took_j, "").map_err(e)?;
        let gap = b.build_int_sub(j, i, "").map_err(e)?;
        let off = b.build_and(gap, mask, "").map_err(e)?;
        let x = b.build_int_add(i, off, "").map_err(e)?;
        b.build_memcpy(pk, 1, at(src, x)?, 1, sz).map_err(e)?;
        let took_i = b.build_int_sub(one, took_j, "").map_err(e)?;
        b.build_store(v_j, b.build_int_add(j, took_j, "").map_err(e)?)
            .map_err(e)?;
        b.build_store(v_i, b.build_int_add(i, took_i, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(m_head).map_err(e)?;
        // The rest of each run, an element at a time: a merge of random
        // runs leaves a few, too few for a call to `memcpy`.
        b.position_at_end(tail);
        b.build_unconditional_branch(take_i).map_err(e)?;
        b.position_at_end(take_i);
        let (src, dst) = (ldp(v_src)?, ldp(v_dst)?);
        let (i, j, k) = (ld(v_i)?, ld(v_j)?, ld(v_k)?);
        let from_i = lt(i, mid)?;
        let more = b.build_or(from_i, lt(j, hi)?, "").map_err(e)?;
        let rest = bb("run.rest");
        b.build_conditional_branch(more, take_j, rest).map_err(e)?;
        b.position_at_end(take_j);
        let x = b
            .build_select(from_i, i, j, "")
            .map_err(e)?
            .into_int_value();
        b.build_memcpy(at(dst, k)?, 1, at(src, x)?, 1, sz)
            .map_err(e)?;
        let step_i = b.build_int_z_extend(from_i, i64t, "").map_err(e)?;
        let step_j = b.build_int_sub(one, step_i, "").map_err(e)?;
        b.build_store(v_i, b.build_int_add(i, step_i, "").map_err(e)?)
            .map_err(e)?;
        b.build_store(v_j, b.build_int_add(j, step_j, "").map_err(e)?)
            .map_err(e)?;
        b.build_store(v_k, b.build_int_add(k, one, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(take_i).map_err(e)?;
        b.position_at_end(rest);
        let lo = ld(v_lo)?;
        b.build_store(v_lo, b.build_int_add(lo, w2, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(lo_head).map_err(e)?;
        // Swap the buffers and double the width.
        b.position_at_end(w_next);
        let (src, dst) = (ldp(v_src)?, ldp(v_dst)?);
        b.build_store(v_src, dst).map_err(e)?;
        b.build_store(v_dst, src).map_err(e)?;
        let w = ld(v_w)?;
        b.build_store(v_w, b.build_int_add(w, w, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(w_head).map_err(e)?;
        // The sorted elements back into `buf` when they ended in scratch.
        b.position_at_end(finish);
        let src = ldp(v_src)?;
        let moved = b
            .build_int_compare(IntPredicate::NE, src, buf, "")
            .map_err(e)?;
        let copy = bb("copy.back");
        b.build_conditional_branch(moved, copy, done).map_err(e)?;
        b.position_at_end(copy);
        b.build_memcpy(buf, 1, src, 1, room).map_err(e)?;
        b.build_unconditional_branch(done).map_err(e)?;
        b.position_at_end(done);
        Ok(())
    }

    /// The body of a [`Shim::MergeSort`]: one scratch buffer, then the
    /// interpreter's schedule (src/mir/interp.rs `merge_order`). Past
    /// `MERGE_BLOCK` elements, the widths below it run block by block,
    /// then the wider passes over the whole buffer.
    fn build_merge_sort(
        &mut self,
        widths: FunctionValue<'ctx>,
        buf: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        ctx: PointerValue<'ctx>,
        size: u64,
    ) -> R<()> {
        let i64t = self.llcx.i64_type();
        let i8t = self.llcx.i8_type();
        let f = self.cur_fn()?;
        let bb = |name: &str| self.llcx.append_basic_block(f, name);
        let (setup, small, blocks, head, each, wide, free, done) = (
            bb("sort"),
            bb("sort.small"),
            bb("sort.blocks"),
            bb("sort.block.next"),
            bb("sort.block"),
            bb("sort.wide"),
            bb("sort.free"),
            bb("sort.done"),
        );
        let block = i64t.const_int(crate::mir::interp::MERGE_BLOCK as u64, false);
        let sz = i64t.const_int(size, false);
        let one = i64t.const_int(1, false);
        let malloc = self.libc_fn("malloc");
        let b = &self.builder;
        let v_b = b.build_alloca(i64t, "block").map_err(e)?;
        let short = b
            .build_int_compare(IntPredicate::ULT, n, i64t.const_int(2, false), "")
            .map_err(e)?;
        b.build_conditional_branch(short, done, setup).map_err(e)?;
        b.position_at_end(setup);
        let room = b.build_int_mul(n, sz, "").map_err(e)?;
        let tmp = b
            .build_call(malloc, &[room.into()], "scratch")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let big = b
            .build_int_compare(IntPredicate::UGT, n, block, "")
            .map_err(e)?;
        b.build_conditional_branch(big, blocks, small).map_err(e)?;
        let call = |p: PointerValue<'ctx>,
                    t: PointerValue<'ctx>,
                    len: IntValue<'ctx>,
                    w: IntValue<'ctx>,
                    lim: IntValue<'ctx>|
         -> R<()> {
            b.build_call(
                widths,
                &[
                    p.into(),
                    t.into(),
                    len.into(),
                    ctx.into(),
                    w.into(),
                    lim.into(),
                ],
                "",
            )
            .map_err(e)?;
            Ok(())
        };
        b.position_at_end(small);
        call(buf, tmp, n, one, n)?;
        b.build_unconditional_branch(free).map_err(e)?;
        // Blocks of MERGE_BLOCK, the last one short.
        b.position_at_end(blocks);
        b.build_store(v_b, i64t.const_zero()).map_err(e)?;
        b.build_unconditional_branch(head).map_err(e)?;
        b.position_at_end(head);
        let at = b.build_load(i64t, v_b, "").map_err(e)?.into_int_value();
        let more = b
            .build_int_compare(IntPredicate::ULT, at, n, "")
            .map_err(e)?;
        b.build_conditional_branch(more, each, wide).map_err(e)?;
        b.position_at_end(each);
        let left = b.build_int_sub(n, at, "").map_err(e)?;
        let fits = b
            .build_int_compare(IntPredicate::ULT, left, block, "")
            .map_err(e)?;
        let len = b
            .build_select(fits, left, block, "")
            .map_err(e)?
            .into_int_value();
        let off = b.build_int_mul(at, sz, "").map_err(e)?;
        // SAFETY of the addresses: `at < n`, in buffers of `n` elements.
        let bp = unsafe { b.build_in_bounds_gep(i8t, buf, &[off], "") }.map_err(e)?;
        let tp = unsafe { b.build_in_bounds_gep(i8t, tmp, &[off], "") }.map_err(e)?;
        call(bp, tp, len, one, block)?;
        let next = b.build_int_add(at, block, "").map_err(e)?;
        b.build_store(v_b, next).map_err(e)?;
        b.build_unconditional_branch(head).map_err(e)?;
        b.position_at_end(wide);
        call(buf, tmp, n, block, n)?;
        b.build_unconditional_branch(free).map_err(e)?;
        b.position_at_end(free);
        self.free(tmp)?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(done);
        Ok(())
    }

    /// The body of a [`Shim::PartSort`]. A short input, or one whose
    /// sampled elements rarely tie with the middle one, takes the merge
    /// sort; the rest is partitioned, which puts each element in place in
    /// about log2(distinct keys) passes of one comparison each, where the
    /// merge sort takes log2(n) (legacy's `emit_sort_partition_body`).
    #[allow(clippy::too_many_arguments)]
    fn build_part_sort(
        &mut self,
        fty: Ty,
        by_key: bool,
        el: Ty,
        size: u64,
        buf: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        ctx: PointerValue<'ctx>,
    ) -> R<()> {
        // The interpreter's `sort_order` thresholds: the same calls.
        use crate::mir::interp::{PART_MIN, PART_ORD_HI, PART_ORD_LO, PART_PROBE, PART_PROBE_GATE};
        let (part_min, probe_n, probe_gate) =
            (PART_MIN as u64, PART_PROBE as u64, PART_PROBE_GATE as u64);
        let merge = self.shim_fn(Shim::MergeSort {
            f: fty,
            by_key,
            el,
            size,
        });
        let part = self.shim_fn(Shim::Partition {
            f: fty,
            by_key,
            el,
            size,
        });
        let cmp = self.shim_fn(Shim::SortBy { f: fty, by_key, el });
        let malloc = self.libc_fn("malloc");
        let f = self.cur_fn()?;
        let bb = |name: &str| self.llcx.append_basic_block(f, name);
        let (probe, p_head, p_body, judge, spread, go, by_merge, done) = (
            bb("probe"),
            bb("probe.k"),
            bb("probe.body"),
            bb("probe.judge"),
            bb("probe.spread"),
            bb("part"),
            bb("merge"),
            bb("done"),
        );
        let b = &self.builder;
        let i64t = self.llcx.i64_type();
        let i8t = self.llcx.i8_type();
        let sz = i64t.const_int(size, false);
        let one = i64t.const_int(1, false);
        let zero = i64t.const_zero();
        let (v_k, v_ties, v_ord, v_gate) = (
            b.build_alloca(i64t, "k").map_err(e)?,
            b.build_alloca(i64t, "ties").map_err(e)?,
            b.build_alloca(i64t, "ord").map_err(e)?,
            b.build_alloca(i64t, "gate").map_err(e)?,
        );
        let ld = |v: PointerValue<'ctx>| -> R<IntValue<'ctx>> {
            Ok(b.build_load(i64t, v, "").map_err(e)?.into_int_value())
        };
        let at = |k: IntValue<'ctx>| -> R<PointerValue<'ctx>> {
            let off = b.build_int_mul(k, sz, "").map_err(e)?;
            // SAFETY of the address: `k < n`, in a buffer of `n` elements.
            unsafe { b.build_in_bounds_gep(i8t, buf, &[off], "") }.map_err(e)
        };
        let short = b
            .build_int_compare(IntPredicate::ULE, n, i64t.const_int(part_min, false), "")
            .map_err(e)?;
        b.build_conditional_branch(short, by_merge, probe)
            .map_err(e)?;
        b.position_at_end(probe);
        let stride = b
            .build_int_unsigned_div(n, i64t.const_int(probe_n, false), "")
            .map_err(e)?;
        let mid = b
            .build_int_unsigned_div(n, i64t.const_int(2, false), "")
            .map_err(e)?;
        let pivot = at(mid)?;
        b.build_store(v_k, zero).map_err(e)?;
        b.build_store(v_ties, zero).map_err(e)?;
        b.build_store(v_ord, zero).map_err(e)?;
        b.build_unconditional_branch(p_head).map_err(e)?;
        b.position_at_end(p_head);
        let k = ld(v_k)?;
        let more = b
            .build_int_compare(IntPredicate::ULT, k, i64t.const_int(probe_n, false), "")
            .map_err(e)?;
        b.build_conditional_branch(more, p_body, judge).map_err(e)?;
        b.position_at_end(p_body);
        let k = ld(v_k)?;
        let ks = b.build_int_mul(k, stride, "").map_err(e)?;
        let x = at(ks)?;
        let o = b
            .build_call(cmp, &[ctx.into(), x.into(), pivot.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let tie = b
            .build_int_compare(IntPredicate::EQ, o, o.get_type().const_zero(), "")
            .map_err(e)?;
        let tie = b.build_int_z_extend(tie, i64t, "").map_err(e)?;
        b.build_store(v_ties, b.build_int_add(ld(v_ties)?, tie, "").map_err(e)?)
            .map_err(e)?;
        // In order with the next one; `k * stride + 1 < n`, as stride >= 16.
        let y = at(b.build_int_add(ks, one, "").map_err(e)?)?;
        let o = b
            .build_call(cmp, &[ctx.into(), x.into(), y.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let up = b
            .build_int_compare(IntPredicate::SLE, o, o.get_type().const_zero(), "")
            .map_err(e)?;
        let up = b.build_int_z_extend(up, i64t, "").map_err(e)?;
        b.build_store(v_ord, b.build_int_add(ld(v_ord)?, up, "").map_err(e)?)
            .map_err(e)?;
        b.build_store(v_k, b.build_int_add(k, one, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(p_head).map_err(e)?;
        b.position_at_end(judge);
        let scaled = b
            .build_int_mul(ld(v_ties)?, i64t.const_int(probe_gate, false), "")
            .map_err(e)?;
        let few = b
            .build_int_compare(
                IntPredicate::UGE,
                scaled,
                i64t.const_int(probe_n, false),
                "",
            )
            .map_err(e)?;
        b.build_store(v_gate, one).map_err(e)?;
        b.build_conditional_branch(few, go, spread).map_err(e)?;
        // No runs to exploit either way: partition with no tie gate.
        b.position_at_end(spread);
        let ord = b
            .build_int_mul(ld(v_ord)?, i64t.const_int(100, false), "")
            .map_err(e)?;
        let lo_ok = b
            .build_int_compare(
                IntPredicate::UGE,
                ord,
                i64t.const_int(PART_ORD_LO as u64 * probe_n, false),
                "",
            )
            .map_err(e)?;
        let hi_ok = b
            .build_int_compare(
                IntPredicate::ULE,
                ord,
                i64t.const_int(PART_ORD_HI as u64 * probe_n, false),
                "",
            )
            .map_err(e)?;
        let band = b.build_and(lo_ok, hi_ok, "").map_err(e)?;
        b.build_store(v_gate, zero).map_err(e)?;
        b.build_conditional_branch(band, go, by_merge).map_err(e)?;
        b.position_at_end(go);
        let room = b.build_int_mul(n, sz, "").map_err(e)?;
        let tmp = b
            .build_call(malloc, &[room.into()], "scratch")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let tags = b
            .build_call(malloc, &[n.into()], "tags")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        b.build_call(
            part,
            &[
                buf.into(),
                tmp.into(),
                tags.into(),
                zero.into(),
                n.into(),
                ctx.into(),
                zero.into(),
                ld(v_gate)?.into(),
                one.into(),
            ],
            "",
        )
        .map_err(e)?;
        self.free(tmp)?;
        self.free(tags)?;
        let b = &self.builder;
        b.build_unconditional_branch(done).map_err(e)?;
        b.position_at_end(by_merge);
        b.build_call(merge, &[buf.into(), n.into(), ctx.into()], "")
            .map_err(e)?;
        b.build_unconditional_branch(done).map_err(e)?;
        b.position_at_end(done);
        Ok(())
    }

    /// The body of a [`Shim::Partition`] (`f`). Each element of the range
    /// is compared once with a copy of the middle one, its side kept in
    /// `tags`; then the range is written to the other buffer in three runs
    /// (less, tied, greater), each in the order it had, and the two sides
    /// are partitioned from there: the buffers swap roles each level, and
    /// a range reaches the sort's own buffer (`home`, the last parameter,
    /// says whether it is in it) only when it is finished, so no level
    /// copies the whole range back. A tied run is in place: it is stable,
    /// so for a comparator that is an order the result is the merge sort's. A range that is short, or too deep,
    /// or (with the tie gate on) ties too rarely to pay for the pass, is
    /// merge sorted.
    fn build_partition(
        &mut self,
        f: FunctionValue<'ctx>,
        fty: Ty,
        by_key: bool,
        el: Ty,
        size: u64,
    ) -> R<()> {
        // The interpreter's `partition` thresholds: the same calls.
        use crate::mir::interp::{PART_DEPTH, PART_GATE, PART_SPAN};
        let (span, depth_max, gate) = (PART_SPAN as u64, PART_DEPTH as u64, PART_GATE as u64);
        let cmp = self.shim_fn(Shim::SortBy { f: fty, by_key, el });
        let widths = self.shim_fn(Shim::MergeWidths {
            f: fty,
            by_key,
            el,
            size,
        });
        let merge = self.shim_fn(Shim::MergeSort {
            f: fty,
            by_key,
            el,
            size,
        });
        let pp = |i: u32| -> R<PointerValue<'ctx>> {
            Ok(f.get_nth_param(i)
                .ok_or("a partition is missing a parameter")?
                .into_pointer_value())
        };
        let pi = |i: u32| -> R<IntValue<'ctx>> {
            Ok(f.get_nth_param(i)
                .ok_or("a partition is missing a parameter")?
                .into_int_value())
        };
        // The range is in `buf`; `tmp` is the other buffer.
        let (buf, tmp, tags, lo, hi, ctx, depth, gated, home) = (
            pp(0)?,
            pp(1)?,
            pp(2)?,
            pi(3)?,
            pi(4)?,
            pp(5)?,
            pi(6)?,
            pi(7)?,
            pi(8)?,
        );
        let bb = |name: &str| self.llcx.append_basic_block(f, name);
        let (count, c_head, c_body, judge, split, s_head, s_body, s_done) = (
            bb("count"),
            bb("count.k"),
            bb("count.body"),
            bb("judge"),
            bb("split"),
            bb("scatter.k"),
            bb("scatter.body"),
            bb("scatter.done"),
        );
        let (left, after_left, right, leaf, leaf_small, leaf_big, done) = (
            bb("left"),
            bb("left.done"),
            bb("right"),
            bb("leaf"),
            bb("leaf.small"),
            bb("leaf.big"),
            bb("done"),
        );
        let (tied, back, settled, leaf_go, leaf_home) = (
            bb("tied"),
            bb("back"),
            bb("settled"),
            bb("leaf.go"),
            bb("leaf.home"),
        );
        let b = &self.builder;
        let i64t = self.llcx.i64_type();
        let i8t = self.llcx.i8_type();
        let sz = i64t.const_int(size, false);
        let one = i64t.const_int(1, false);
        let zero = i64t.const_zero();
        let var = |name: &str| b.build_alloca(i64t, name).map_err(e);
        let (v_k, v_lt, v_eq) = (var("k")?, var("lt")?, var("eq")?);
        // The three runs' next free slots, as scalars so they live in
        // registers: an array indexed by the tag would carry each step's
        // cursor through memory into the next one's.
        let curs = [var("cur.lt")?, var("cur.eq")?, var("cur.gt")?];
        let piv = b
            .build_alloca(i8t.array_type(size as u32), "pivot")
            .map_err(e)?;
        let ld = |v: PointerValue<'ctx>| -> R<IntValue<'ctx>> {
            Ok(b.build_load(i64t, v, "").map_err(e)?.into_int_value())
        };
        let at = |q: PointerValue<'ctx>, k: IntValue<'ctx>| -> R<PointerValue<'ctx>> {
            let off = b.build_int_mul(k, sz, "").map_err(e)?;
            // SAFETY of the address: `k` is in `lo..=hi`, inside buffers of
            // the whole sort's length.
            unsafe { b.build_in_bounds_gep(i8t, q, &[off], "") }.map_err(e)
        };
        let ult = |x: IntValue<'ctx>, y: IntValue<'ctx>| {
            b.build_int_compare(IntPredicate::ULT, x, y, "").map_err(e)
        };
        let len = b.build_int_sub(hi, lo, "len").map_err(e)?;
        let short = b
            .build_int_compare(IntPredicate::ULE, len, i64t.const_int(span, false), "")
            .map_err(e)?;
        let deep = b
            .build_int_compare(
                IntPredicate::UGE,
                depth,
                i64t.const_int(depth_max, false),
                "",
            )
            .map_err(e)?;
        let stop = b.build_or(short, deep, "").map_err(e)?;
        b.build_conditional_branch(stop, leaf, count).map_err(e)?;
        // Each element against the pivot: tag 0 less, 1 tied, 2 greater.
        b.position_at_end(count);
        let half = b.build_right_shift(len, one, false, "").map_err(e)?;
        let mid = b.build_int_add(lo, half, "").map_err(e)?;
        b.build_memcpy(piv, 1, at(buf, mid)?, 1, sz).map_err(e)?;
        b.build_store(v_k, lo).map_err(e)?;
        b.build_store(v_lt, zero).map_err(e)?;
        b.build_store(v_eq, zero).map_err(e)?;
        b.build_unconditional_branch(c_head).map_err(e)?;
        b.position_at_end(c_head);
        b.build_conditional_branch(ult(ld(v_k)?, hi)?, c_body, judge)
            .map_err(e)?;
        b.position_at_end(c_body);
        let k = ld(v_k)?;
        let o = b
            .build_call(cmp, &[ctx.into(), at(buf, k)?.into(), piv.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let oz = o.get_type().const_zero();
        let is_lt = b
            .build_int_compare(IntPredicate::SLT, o, oz, "")
            .map_err(e)?;
        let is_gt = b
            .build_int_compare(IntPredicate::SGT, o, oz, "")
            .map_err(e)?;
        let is_eq = b
            .build_int_compare(IntPredicate::EQ, o, oz, "")
            .map_err(e)?;
        let (lt8, gt8) = (
            b.build_int_z_extend(is_lt, i8t, "").map_err(e)?,
            b.build_int_z_extend(is_gt, i8t, "").map_err(e)?,
        );
        let tag = b
            .build_int_sub(
                b.build_int_add(i8t.const_int(1, false), gt8, "")
                    .map_err(e)?,
                lt8,
                "tag",
            )
            .map_err(e)?;
        // SAFETY of the address: `k < hi`, in a buffer of the sort's length.
        let tp = unsafe { b.build_in_bounds_gep(i8t, tags, &[k], "") }.map_err(e)?;
        b.build_store(tp, tag).map_err(e)?;
        let add = |v: PointerValue<'ctx>, c: IntValue<'ctx>| -> R<()> {
            let c = b.build_int_z_extend(c, i64t, "").map_err(e)?;
            b.build_store(v, b.build_int_add(ld(v)?, c, "").map_err(e)?)
                .map_err(e)?;
            Ok(())
        };
        add(v_lt, is_lt)?;
        add(v_eq, is_eq)?;
        b.build_store(v_k, b.build_int_add(k, one, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(c_head).map_err(e)?;
        // Too few ties to pay: merge. All tied: already in order.
        b.position_at_end(judge);
        let (lt, eq) = (ld(v_lt)?, ld(v_eq)?);
        let scaled = b
            .build_int_mul(eq, i64t.const_int(gate, false), "")
            .map_err(e)?;
        let rare = ult(scaled, len)?;
        let gate_on = b
            .build_int_compare(IntPredicate::NE, gated, zero, "")
            .map_err(e)?;
        let rare = b.build_and(rare, gate_on, "").map_err(e)?;
        let all = b
            .build_int_compare(IntPredicate::EQ, eq, len, "")
            .map_err(e)?;
        let to_split = bb("judge.split");
        b.build_conditional_branch(rare, leaf, to_split)
            .map_err(e)?;
        b.position_at_end(to_split);
        b.build_conditional_branch(all, tied, split).map_err(e)?;
        // All tied: in order where it is, so only taken home.
        b.position_at_end(tied);
        let bytes = b.build_int_mul(len, sz, "").map_err(e)?;
        let away = b
            .build_int_compare(IntPredicate::EQ, home, zero, "")
            .map_err(e)?;
        let to_home = bb("tied.home");
        b.build_conditional_branch(away, to_home, done).map_err(e)?;
        b.position_at_end(to_home);
        b.build_memcpy(at(tmp, lo)?, 1, at(buf, lo)?, 1, bytes)
            .map_err(e)?;
        b.build_unconditional_branch(done).map_err(e)?;
        // Each element to the next free slot of its run in `tmp`.
        b.position_at_end(split);
        let c1 = b.build_int_add(lo, lt, "").map_err(e)?;
        let c2 = b.build_int_add(c1, eq, "").map_err(e)?;
        for (v, c) in curs.iter().zip([lo, c1, c2]) {
            b.build_store(*v, c).map_err(e)?;
        }
        b.build_store(v_k, lo).map_err(e)?;
        b.build_unconditional_branch(s_head).map_err(e)?;
        b.position_at_end(s_head);
        b.build_conditional_branch(ult(ld(v_k)?, hi)?, s_body, s_done)
            .map_err(e)?;
        b.position_at_end(s_body);
        let k = ld(v_k)?;
        // SAFETY of the address: `k < hi`, as in the count.
        let tp = unsafe { b.build_in_bounds_gep(i8t, tags, &[k], "") }.map_err(e)?;
        let t = b.build_load(i8t, tp, "").map_err(e)?.into_int_value();
        let is = |n: u64| {
            b.build_int_compare(IntPredicate::EQ, t, i8t.const_int(n, false), "")
                .map_err(e)
        };
        let (t0, t1) = (is(0)?, is(1)?);
        let (k0, k1, k2) = (ld(curs[0])?, ld(curs[1])?, ld(curs[2])?);
        let c12 = b.build_select(t1, k1, k2, "").map_err(e)?;
        let c = b
            .build_select(t0, k0.into(), c12, "")
            .map_err(e)?
            .into_int_value();
        b.build_memcpy(at(tmp, c)?, 1, at(buf, k)?, 1, sz)
            .map_err(e)?;
        let t2 = b
            .build_not(b.build_or(t0, t1, "").map_err(e)?, "")
            .map_err(e)?;
        for (v, (kc, hit)) in curs.iter().zip([(k0, t0), (k1, t1), (k2, t2)]) {
            let inc = b.build_int_z_extend(hit, i64t, "").map_err(e)?;
            b.build_store(*v, b.build_int_add(kc, inc, "").map_err(e)?)
                .map_err(e)?;
        }
        b.build_store(v_k, b.build_int_add(k, one, "").map_err(e)?)
            .map_err(e)?;
        b.build_unconditional_branch(s_head).map_err(e)?;
        // The runs are in `tmp` now. When that is not home, the tied run
        // and a side of one element, which are finished, go back.
        b.position_at_end(s_done);
        let gt = b.build_int_sub(hi, c2, "").map_err(e)?;
        let tmp_away = b
            .build_int_compare(IntPredicate::NE, home, zero, "")
            .map_err(e)?;
        b.build_conditional_branch(tmp_away, back, settled)
            .map_err(e)?;
        b.position_at_end(back);
        let eq_bytes = b.build_int_mul(eq, sz, "").map_err(e)?;
        b.build_memcpy(at(buf, c1)?, 1, at(tmp, c1)?, 1, eq_bytes)
            .map_err(e)?;
        for (n, from) in [(lt, lo), (gt, c2)] {
            let single = b
                .build_int_compare(IntPredicate::EQ, n, one, "")
                .map_err(e)?;
            let bytes = b
                .build_select(single, sz, zero, "")
                .map_err(e)?
                .into_int_value();
            b.build_memcpy(at(buf, from)?, 1, at(tmp, from)?, 1, bytes)
                .map_err(e)?;
        }
        b.build_unconditional_branch(settled).map_err(e)?;
        b.position_at_end(settled);
        let flipped = b.build_xor(home, one, "").map_err(e)?;
        let deeper = b.build_int_add(depth, one, "").map_err(e)?;
        let recurse = |from: IntValue<'ctx>, to: IntValue<'ctx>| -> R<()> {
            b.build_call(
                f,
                &[
                    tmp.into(),
                    buf.into(),
                    tags.into(),
                    from.into(),
                    to.into(),
                    ctx.into(),
                    deeper.into(),
                    gated.into(),
                    flipped.into(),
                ],
                "",
            )
            .map_err(e)?;
            Ok(())
        };
        b.build_conditional_branch(ult(one, lt)?, left, after_left)
            .map_err(e)?;
        b.position_at_end(left);
        recurse(lo, c1)?;
        b.build_unconditional_branch(after_left).map_err(e)?;
        b.position_at_end(after_left);
        b.build_conditional_branch(ult(one, gt)?, right, done)
            .map_err(e)?;
        b.position_at_end(right);
        recurse(c2, hi)?;
        b.build_unconditional_branch(done).map_err(e)?;
        // The merge sort of the range, in its home buffer with the other as
        // scratch: its narrow passes here, or the whole schedule for a long
        // range.
        b.position_at_end(leaf);
        let at_home = b
            .build_int_compare(IntPredicate::NE, home, zero, "")
            .map_err(e)?;
        b.build_conditional_branch(at_home, leaf_go, leaf_home)
            .map_err(e)?;
        b.position_at_end(leaf_home);
        let bytes = b.build_int_mul(len, sz, "").map_err(e)?;
        b.build_memcpy(at(tmp, lo)?, 1, at(buf, lo)?, 1, bytes)
            .map_err(e)?;
        b.build_unconditional_branch(leaf_go).map_err(e)?;
        b.position_at_end(leaf_go);
        let (hb, ob) = (
            b.build_select(at_home, buf, tmp, "")
                .map_err(e)?
                .into_pointer_value(),
            b.build_select(at_home, tmp, buf, "")
                .map_err(e)?
                .into_pointer_value(),
        );
        let small = b
            .build_int_compare(
                IntPredicate::ULE,
                len,
                i64t.const_int(crate::mir::interp::MERGE_BLOCK as u64, false),
                "",
            )
            .map_err(e)?;
        b.build_conditional_branch(small, leaf_small, leaf_big)
            .map_err(e)?;
        b.position_at_end(leaf_small);
        let (bp, tp) = (at(hb, lo)?, at(ob, lo)?);
        b.build_call(
            widths,
            &[
                bp.into(),
                tp.into(),
                len.into(),
                ctx.into(),
                one.into(),
                len.into(),
            ],
            "",
        )
        .map_err(e)?;
        b.build_unconditional_branch(done).map_err(e)?;
        b.position_at_end(leaf_big);
        let bp = at(hb, lo)?;
        b.build_call(merge, &[bp.into(), len.into(), ctx.into()], "")
            .map_err(e)?;
        b.build_unconditional_branch(done).map_err(e)?;
        b.position_at_end(done);
        Ok(())
    }
}

/// `v`, through an empty `asm` the optimizer cannot look into, so
/// arithmetic on it is not folded back into a `select` or a branch.
fn opaque_i64<'ctx>(
    llcx: &'ctx inkwell::context::Context,
    b: &inkwell::builder::Builder<'ctx>,
    v: IntValue<'ctx>,
) -> R<IntValue<'ctx>> {
    let i64t = llcx.i64_type();
    let ft = i64t.fn_type(&[i64t.into()], false);
    let asm = llcx.create_inline_asm(
        ft,
        String::new(),
        "=r,0".to_string(),
        false,
        false,
        None,
        false,
    );
    Ok(b.build_indirect_call(ft, asm, &[v.into()], "")
        .map_err(e)?
        .try_as_basic_value()
        .unwrap_basic()
        .into_int_value())
}
