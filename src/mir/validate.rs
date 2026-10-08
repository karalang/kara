//! Structural validation of a MIR body (`docs/spikes/mir-types.md` §3).
//!
//! Runs after every phase. It checks what holds in every phase; the
//! dataflow invariants of `Checked` and `DropsElaborated` belong to the
//! passes that establish them.

use super::flags;
use super::place_ty::{passes_through_shared, place_ty};
use super::pretty;
use super::syntax::*;
use super::ty::{FnKind, IntTy, Ty, TyInterner, TyKind};

/// Every structural error in `body`, in block order. Empty means valid.
pub fn validate(body: &Body, tys: &TyInterner) -> Vec<String> {
    let mut v = Validator {
        body,
        tys,
        errors: Vec::new(),
        loc: String::new(),
    };
    v.run();
    v.errors
}

struct Validator<'a> {
    body: &'a Body,
    tys: &'a TyInterner,
    errors: Vec<String>,
    loc: String,
}

impl Validator<'_> {
    fn err(&mut self, msg: impl AsRef<str>) {
        self.errors.push(format!("{}: {}", self.loc, msg.as_ref()));
    }

    fn run(&mut self) {
        let body = self.body;
        self.loc = "locals".into();
        if body.locals.is_empty() || body.locals[0].kind != LocalKind::ReturnPlace {
            self.err("_0 must be the return place");
        }
        if body.arg_count >= body.locals.len() {
            self.err("arg_count exceeds the number of locals");
        }
        for (i, decl) in body.locals.iter().enumerate().skip(1) {
            let is_arg = matches!(decl.kind, LocalKind::Arg { .. });
            if is_arg != (i <= body.arg_count) {
                self.err(format!(
                    "_{i}: parameters must be exactly _1..=_{}",
                    body.arg_count
                ));
            }
            if decl.kind == LocalKind::DropFlag && self.tys.kind(decl.ty) != TyKind::Bool {
                self.err(format!("_{i}: a drop flag must be bool"));
            }
        }
        if body.blocks.is_empty() {
            self.loc = "body".into();
            self.err("no blocks");
        }
        for (bi, block) in body.blocks.iter().enumerate() {
            for (si, st) in block.statements.iter().enumerate() {
                self.loc = format!("bb{bi}[{si}]");
                self.statement(&st.kind);
            }
            self.loc = format!("bb{bi}[term]");
            self.terminator(&block.terminator.kind);
        }
    }

    fn local_ok(&mut self, l: Local) -> bool {
        if l.index() >= self.body.locals.len() {
            self.err(format!("{l} is not a local of this body"));
            false
        } else {
            true
        }
    }

    fn target(&mut self, b: BasicBlock) {
        if b.index() >= self.body.blocks.len() {
            self.err(format!("jump to missing {b}"));
        }
    }

    /// The type of `p`, reporting an ill-typed path.
    /// Does `p` go through a `Deref` of a shared (`ref`) reference?
    /// Is `p` reached through a `ref` with no `shared` handle projected
    /// after it? A `mut` field of a shared value is writable through any
    /// handle (core semantics §6.2), so the drop before its assignment is
    /// fine.
    fn behind_shared_ref(&self, p: &Place) -> bool {
        let mut behind = false;
        for i in 0..p.projection.len() {
            let prefix = Place {
                local: p.local,
                projection: p.projection[..i].to_vec(),
            };
            match place_ty(self.body, self.tys, &prefix).map(|pt| self.tys.kind(pt.ty)) {
                Ok(TyKind::Ref(_)) if p.projection[i] == ProjElem::Deref => behind = true,
                Ok(TyKind::Shared(_)) => behind = false,
                _ => {}
            }
        }
        behind
    }

    fn place(&mut self, p: &Place) -> Option<Ty> {
        if !self.local_ok(p.local) {
            return None;
        }
        match place_ty(self.body, self.tys, p) {
            Ok(pt) if pt.variant.is_none() => Some(pt.ty),
            Ok(_) => {
                self.err(format!(
                    "{} ends in a downcast; a downcast must be followed by a field",
                    pretty::place(self.body, self.tys, p)
                ));
                None
            }
            Err(e) => {
                self.err(e);
                None
            }
        }
    }

    fn operand(&mut self, o: &Operand) -> Option<Ty> {
        match o {
            Operand::Copy(p) => {
                let ty = self.place(p)?;
                if !self.tys.is_copy(ty) {
                    self.err(format!(
                        "copy of {} whose type {} is not Copy",
                        pretty::place(self.body, self.tys, p),
                        self.tys.display(ty)
                    ));
                }
                Some(ty)
            }
            Operand::Move(p) => {
                let ty = self.place(p)?;
                let what = pretty::place(self.body, self.tys, p);
                if p.is_move_forbidden() {
                    self.err(format!(
                        "move out of {what}, which is behind a reference or an index (core semantics §3.7)"
                    ));
                } else if passes_through_shared(self.body, self.tys, p) {
                    self.err(format!(
                        "move out of {what}, which is inside a shared value (core semantics §3.7)"
                    ));
                }
                Some(ty)
            }
            Operand::Const(c) => Some(c.ty),
        }
    }

    fn statement(&mut self, s: &StatementKind) {
        match s {
            StatementKind::Assign(p, rv) => {
                let dest = self.place(p);
                let got = self.rvalue(rv);
                if let (Some(d), Some(g)) = (dest, got) {
                    if d != g {
                        self.err(format!(
                            "assigning {} to {} of type {}",
                            self.tys.display(g),
                            pretty::place(self.body, self.tys, p),
                            self.tys.display(d)
                        ));
                    }
                }
            }
            StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => {
                self.local_ok(*l);
            }
            StatementKind::SetDiscriminant(p, v) => {
                if let Some(ty) = self.place(p) {
                    match self.tys.kind(ty) {
                        TyKind::Adt(a) if self.tys.adt(a).is_enum => {
                            if v.index() >= self.tys.adt(a).variants.len() {
                                self.err(format!("variant {} out of range", v.0));
                            }
                        }
                        _ => self.err("set_discriminant on a non-enum place"),
                    }
                }
            }
            StatementKind::BorrowFlag(
                FlagOp::Acquire { place, .. } | FlagOp::Check { place, .. },
            ) => {
                if let Some(ty) = self.place(place) {
                    if !flags::is_flagged_place(self.body, self.tys, place) {
                        self.err(format!(
                            "borrow flag on {}, of type {}, which is not a mut field of a shared value",
                            pretty::place(self.body, self.tys, place),
                            self.tys.display(ty)
                        ));
                    }
                }
            }
            StatementKind::BorrowFlag(FlagOp::Release { .. }) | StatementKind::Nop => {}
        }
    }

    /// The type an rvalue produces, where it is determined by its operands.
    fn rvalue(&mut self, rv: &Rvalue) -> Option<Ty> {
        match rv {
            Rvalue::Use(o) => self.operand(o),
            Rvalue::Ref(_, p) | Rvalue::Retain(p) | Rvalue::Discriminant(p) | Rvalue::Len(p) => {
                let ty = self.place(p);
                if let (Rvalue::Retain(_), Some(t)) = (rv, ty) {
                    if !matches!(self.tys.kind(t), TyKind::Shared(_)) {
                        self.err(format!("retain of non-shared {}", self.tys.display(t)));
                    }
                }
                // Reference and integer result types are interned by the
                // builder; not re-derived here.
                None
            }
            Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
                let ta = self.operand(a);
                let tb = self.operand(b);
                if let (Some(x), Some(y)) = (ta, tb) {
                    let shift = matches!(
                        rv,
                        Rvalue::BinaryOp(BinOp::Shl | BinOp::Shr, ..)
                            | Rvalue::CheckedBinaryOp(BinOp::Shl | BinOp::Shr, ..)
                    );
                    if x != y && !shift {
                        self.err(format!(
                            "binary operands of different types {} and {}",
                            self.tys.display(x),
                            self.tys.display(y)
                        ));
                    }
                }
                None
            }
            Rvalue::UnaryOp(_, o) => self.operand(o),
            Rvalue::Cast(kind, o, t) => {
                if let Some(from) = self.operand(o) {
                    let ok = matches!(
                        (kind, self.tys.kind(from), self.tys.kind(*t)),
                        (CastKind::IntToInt, TyKind::Int(_), TyKind::Int(_))
                            | (CastKind::IntToFloat, TyKind::Int(_), TyKind::Float(_))
                            | (CastKind::FloatToInt, TyKind::Float(_), TyKind::Int(_))
                            | (CastKind::FloatToFloat, TyKind::Float(_), TyKind::Float(_))
                            | (CastKind::IntToChar, TyKind::Int(IntTy::U8), TyKind::Char)
                            | (CastKind::CharToInt, TyKind::Char, TyKind::Int(_))
                            | (CastKind::BoolToInt, TyKind::Bool, TyKind::Int(_))
                            // A closure's signature is not in its MIR type, so
                            // the interpreter checks it at the call.
                            | (
                                CastKind::Erase,
                                TyKind::Closure(..) | TyKind::FnDef(_),
                                TyKind::Fn { .. }
                            )
                    ) || matches!(
                        (kind, self.tys.kind(from), self.tys.kind(*t)),
                        (
                            CastKind::Erase,
                            TyKind::Fn { params: p, ret: r, kind: k },
                            TyKind::Fn { params: q, ret: s, kind: l },
                        ) if p == q && r == s && k <= l
                    ) || self.weak_cast(*kind, from, *t);
                    if !ok {
                        self.err(format!(
                            "{kind:?} cast from {} to {}",
                            self.tys.display(from),
                            self.tys.display(*t)
                        ));
                    }
                }
                Some(*t)
            }
            Rvalue::Aggregate(kind, ops) => {
                let tys: Vec<Option<Ty>> = ops.iter().map(|o| self.operand(o)).collect();
                match kind {
                    AggregateKind::Adt { ty, variant } | AggregateKind::Shared { ty, variant } => {
                        self.aggregate_fields(*ty, *variant, &tys);
                        Some(*ty)
                    }
                    AggregateKind::Array(e) => {
                        for t in tys.iter().flatten() {
                            if t != e {
                                self.err("array element of the wrong type");
                            }
                        }
                        None
                    }
                    AggregateKind::Closure { ty } => Some(*ty),
                    AggregateKind::Tuple => None,
                }
            }
        }
    }

    fn aggregate_fields(&mut self, ty: Ty, variant: VariantIdx, got: &[Option<Ty>]) {
        let a = match self.tys.kind(ty) {
            TyKind::Adt(a) | TyKind::Shared(a) => a,
            _ => {
                self.err(format!("ADT aggregate of non-ADT {}", self.tys.display(ty)));
                return;
            }
        };
        let adt = self.tys.adt(a);
        let Some(v) = adt.variants.get(variant.index()) else {
            self.err(format!("variant {} out of range", variant.0));
            return;
        };
        if v.fields.len() != got.len() {
            self.err(format!(
                "{} takes {} fields, got {}",
                adt.name,
                v.fields.len(),
                got.len()
            ));
            return;
        }
        // Instantiated for `ty`'s arguments, as a field projection is.
        let variant_arg = adt.is_enum.then_some(variant.0);
        let want: Vec<Ty> = (0..v.fields.len() as u32)
            .map(|i| {
                self.tys
                    .field_ty(ty, variant_arg, i)
                    .unwrap_or(v.fields[i as usize].1)
            })
            .collect();
        for (i, (w, g)) in want.iter().zip(got).enumerate() {
            if let Some(g) = g {
                if g != w {
                    self.err(format!("field {i} of {} has the wrong type", adt.name));
                }
            }
        }
    }

    fn terminator(&mut self, t: &TerminatorKind) {
        match t {
            TerminatorKind::Goto { target } => self.target(*target),
            TerminatorKind::SwitchInt { discr, targets } => {
                if let Some(ty) = self.operand(discr) {
                    if !matches!(
                        self.tys.kind(ty),
                        TyKind::Bool | TyKind::Char | TyKind::Int(_)
                    ) {
                        self.err(format!("switchInt on {}", self.tys.display(ty)));
                    }
                }
                for b in targets.all_targets() {
                    self.target(b);
                }
                let mut seen: Vec<u128> = targets.values.iter().map(|(v, _)| *v).collect();
                seen.sort_unstable();
                seen.dedup();
                if seen.len() != targets.values.len() {
                    self.err("switchInt lists a value twice");
                }
            }
            // `Abort` needs no check; a cleanup edge would.
            TerminatorKind::Call {
                func,
                args,
                destination,
                target,
                unwind: UnwindAction::Abort,
            } => {
                let fty = self.operand(func);
                let arg_tys: Vec<Option<Ty>> = args.iter().map(|a| self.operand(a)).collect();
                let dest = self.place(destination);
                if let Some(fty) = fty {
                    self.call_through_fn(func, fty, &arg_tys, dest);
                }
                if let Some(b) = target {
                    self.target(*b);
                }
            }
            TerminatorKind::Drop {
                place,
                target,
                unwind: UnwindAction::Abort,
            } => {
                // Dropping through a `mut ref` or an index is D4's drop of
                // the old value before `*r = v` / `a[i] = v`; through a
                // shared `ref` nothing may be overwritten, so nothing drops.
                if self.place(place).is_some() && self.behind_shared_ref(place) {
                    self.err("drop of a place behind a shared reference");
                }
                self.target(*target);
            }
            TerminatorKind::Return | TerminatorKind::Abort { .. } | TerminatorKind::Unreachable => {
            }
        }
    }
}

impl Validator<'_> {
    /// §6.5: `Downgrade` takes `ref shared T`, `ref weak T` or
    /// `ref Option[shared T]` to `weak T` (`None` gives an empty weak);
    /// `Upgrade` takes `ref weak T` to `Option[shared T]`.
    fn weak_cast(&self, kind: CastKind, from: Ty, to: Ty) -> bool {
        let TyKind::Ref(inner) = self.tys.kind(from) else {
            return false;
        };
        match (kind, self.tys.kind(inner), self.tys.kind(to)) {
            (CastKind::Downgrade, TyKind::Shared(_), TyKind::Weak(w)) => w == inner,
            (CastKind::Downgrade, TyKind::Weak(v), TyKind::Weak(w)) => v == w,
            (CastKind::Downgrade, TyKind::Adt(_), TyKind::Weak(w)) => self.option_of(inner, w),
            (CastKind::Upgrade, TyKind::Weak(w), TyKind::Adt(_)) => self.option_of(to, w),
            _ => false,
        }
    }

    /// Whether `ty` is `Option[shared T]` for the `shared T` that `w`
    /// names. Checked by shape, since MIR text names an instance with its
    /// arguments (`Option[shared Node]`).
    fn option_of(&self, ty: Ty, w: Ty) -> bool {
        let TyKind::Adt(a) = self.tys.kind(ty) else {
            return false;
        };
        let adt = self.tys.adt(a);
        let pos = |n: &str| adt.variants.iter().position(|v| v.name == n);
        adt.is_enum
            && adt.variants.len() == 2
            && pos("None").is_some()
            && pos("Some")
                .is_some_and(|some| self.tys.field_ty(ty, Some(some as u32), 0) == Some(w))
    }

    /// A call through an erased function value (core semantics §9.6):
    /// `Fn` is called through a `ref`, `MutFn` through a `mut ref`, and
    /// `OnceFn` by moving it; the arguments and result match its type.
    fn call_through_fn(&mut self, func: &Operand, fty: Ty, args: &[Option<Ty>], dest: Option<Ty>) {
        let (inner, how) = match self.tys.kind(fty) {
            TyKind::Ref(t) => (t, FnKind::Fn),
            TyKind::MutRef(t) => (t, FnKind::MutFn),
            _ => (fty, FnKind::OnceFn),
        };
        let TyKind::Fn { params, ret, kind } = self.tys.kind(inner) else {
            return;
        };
        if kind > how {
            self.err(format!(
                "a {} value is called through {}",
                kind.name(),
                match how {
                    FnKind::Fn => "a `ref`",
                    FnKind::MutFn => "a `mut ref`",
                    FnKind::OnceFn => "a move",
                }
            ));
        }
        if how == FnKind::OnceFn && !matches!(func, Operand::Move(_)) {
            self.err("a call by value through an `OnceFn` must move it");
        }
        if args.len() != params.len() {
            self.err(format!(
                "a {} takes {} arguments, given {}",
                self.tys.display(inner),
                params.len(),
                args.len()
            ));
        } else {
            for (i, (a, p)) in args.iter().zip(&params).enumerate() {
                if a.is_some_and(|a| a != *p) {
                    self.err(format!(
                        "argument {i} of a {} call has the wrong type",
                        kind.name()
                    ));
                }
            }
        }
        if dest.is_some_and(|d| d != ret) {
            self.err(format!(
                "the result of a {} call has the wrong type",
                kind.name()
            ));
        }
    }
}

/// `usize`, for index locals.
pub fn is_usize(tys: &TyInterner, t: Ty) -> bool {
    tys.kind(t) == TyKind::Int(IntTy::Usize)
}
