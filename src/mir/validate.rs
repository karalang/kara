//! Structural validation of a MIR body (`docs/spikes/mir-types.md` §3).
//!
//! Runs after every phase. It checks what holds in every phase; the
//! dataflow invariants of `Checked` and `DropsElaborated` belong to the
//! passes that establish them.

use super::place_ty::{passes_through_shared, place_ty};
use super::pretty;
use super::syntax::*;
use super::ty::{IntTy, Ty, TyInterner, TyKind};

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
            StatementKind::Nop => {}
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
            Rvalue::Cast(_, o, t) => {
                self.operand(o);
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
            TerminatorKind::Call {
                func,
                args,
                destination,
                target,
            } => {
                self.operand(func);
                for a in args {
                    self.operand(a);
                }
                self.place(destination);
                if let Some(b) = target {
                    self.target(*b);
                }
            }
            TerminatorKind::Drop { place, target } => {
                if let Some(ty) = self.place(place) {
                    if place.is_move_forbidden() {
                        self.err("drop of a place behind a reference or an index");
                    }
                    let _ = ty;
                }
                self.target(*target);
            }
            TerminatorKind::Return | TerminatorKind::Abort { .. } | TerminatorKind::Unreachable => {
            }
        }
    }
}

/// `usize`, for index locals.
pub fn is_usize(tys: &TyInterner, t: Ty) -> bool {
    tys.kind(t) == TyKind::Int(IntTy::Usize)
}
