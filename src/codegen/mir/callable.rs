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
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};

use super::{Cx, R};
use crate::mir::{Body, Operand, Place, Ty, TyKind};

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

    fn shim_fn(&mut self, s: Shim) -> FunctionValue<'ctx> {
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
                    self.build_thunk(&name, ret, env, consume, &args)?;
                    self.builder.build_return(None).map_err(e)?;
                }
                Shim::SortBy { f: fty, by_key, el } => {
                    let (ctx, a, b) = (param(0)?, param(1)?, param(2)?);
                    let o = self.sort_by_order(fty, by_key, el, ctx, a, b)?;
                    self.builder.build_return(Some(&o)).map_err(e)?;
                }
            }
        }
        Ok(())
    }

    /// The body a closure or function-item type names.
    fn body_of_def(&self, ty: Ty) -> R<&'_ Body> {
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
            _ => self.body_of_def(from)?.instance.name.clone(),
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
    fn callee_ret(&self, c: &Callee<'ctx>) -> R<Ty> {
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
}
