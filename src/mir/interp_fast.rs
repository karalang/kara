//! The interpreter's decoded form of a body: each block's common steps
//! (scalar copies, integer arithmetic and casts, comparisons, switches,
//! reborrows, reads through a reference, `Vec` indexing) worked out once,
//! so running them does not walk the MIR or ask the type interner again.
//!
//! A decoded step does only what the general step it stands for would do
//! on its happy path. Anything else (an uninitialized read, an overflow, an
//! index out of bounds, a value of an unexpected shape) is left to the
//! general step, which runs from scratch and reports exactly what it always
//! did: a decoded step changes nothing until it knows it can finish.

use super::*;

/// Where a scalar operand is read from.
#[derive(Clone, Copy)]
pub(super) enum Src {
    /// A bare local; `true` when the operand moves it.
    Local(u32, bool),
    /// A scalar field of a local aggregate, copied.
    Field(u32, u32),
    /// What a local reference points at, copied.
    Deref(u32),
    /// A constant, worked out when the body was decoded.
    Const(Scalar),
}

/// A plain value: nothing to drop, copied by bits.
#[derive(Clone, Copy)]
pub(super) enum Scalar {
    Int(i128),
    Bool(bool),
    Char(char),
    Unit,
}

impl Scalar {
    #[inline(always)]
    fn of(v: &Value) -> Option<Scalar> {
        Some(match *v {
            Value::Int(i) => Scalar::Int(i),
            Value::Bool(b) => Scalar::Bool(b),
            Value::Char(c) => Scalar::Char(c),
            Value::Unit => Scalar::Unit,
            _ => return None,
        })
    }

    #[inline(always)]
    fn value(self) -> Value {
        match self {
            Scalar::Int(i) => Value::Int(i),
            Scalar::Bool(b) => Value::Bool(b),
            Scalar::Char(c) => Value::Char(c),
            Scalar::Unit => Value::Unit,
        }
    }
}

/// One statement, decoded.
pub(super) enum Op {
    /// `dst = src`, a scalar into a scalar local.
    Use { dst: u32, src: Src },
    /// `dst = a op b` on integers of type `int`.
    Int {
        dst: u32,
        op: BinOp,
        a: Src,
        b: Src,
        int: IntTy,
    },
    /// `dst = Checked(a op b)`: the pair of the result and its overflow.
    Checked {
        dst: u32,
        op: BinOp,
        a: Src,
        b: Src,
        int: IntTy,
    },
    /// `dst = src as int`, an integer resize.
    IntCast { dst: u32, src: Src, int: IntTy },
    /// `dst = &(*src)` or `dst = &mut (*src)`: the reference `src` holds,
    /// again.
    Reborrow { dst: u32, src: u32 },
    /// `dst = !src` on a `bool` (`int` is `None`) or an integer, or
    /// `dst = -src` on an integer.
    Unary {
        dst: u32,
        op: UnOp,
        src: Src,
        int: Option<IntTy>,
    },
    /// `dst = (a, b, ..)`, a tuple of scalars.
    Tuple { dst: u32, parts: Box<[Src]> },
    /// `dst = &src` or `dst = &mut src`, a bare local.
    RefLocal { dst: u32, src: u32 },
    /// `(*dst) = src`: a scalar stored through a local reference.
    Store { dst: u32, src: Src },
    /// The forms above whose operands are copied locals or constants,
    /// read without going through [`Src`].
    CopyLocal { dst: u32, src: u32 },
    IntLL {
        dst: u32,
        op: BinOp,
        a: u32,
        b: u32,
        int: IntTy,
    },
    IntLC {
        dst: u32,
        op: BinOp,
        a: u32,
        b: i128,
        int: IntTy,
    },
    CheckedLL {
        dst: u32,
        op: BinOp,
        a: u32,
        b: u32,
        int: IntTy,
    },
    CheckedLC {
        dst: u32,
        op: BinOp,
        a: u32,
        b: i128,
        int: IntTy,
    },
    /// `StorageLive(l)`.
    Live(u32),
    /// `StorageDead(l)` of a scalar local, which owns nothing.
    DeadPlain(u32),
    /// Run the general statement.
    General,
}

/// A block's terminator, decoded.
pub(super) enum Term {
    Goto(BasicBlock),
    /// `switchInt` on a scalar: an integer of type `int`, a `bool` or a
    /// `char` (`int` is `None` for the last two).
    Switch {
        src: Src,
        int: Option<IntTy>,
    },
    /// `switchInt(copy l)` on a `bool` local.
    SwitchBool(u32),
    /// `switchInt(copy l.1)`: a checked step's overflow flag.
    SwitchFlag(u32),
    /// `dst = Vec[T].index(recv, index) -> target` (or `index_mut`), where
    /// `recv` is a bare local reference; `true` when it is moved.
    VecIndex {
        dst: u32,
        recv: u32,
        moved: bool,
        index: Src,
        target: BasicBlock,
    },
    /// A call of a library function with no body, whose result has type
    /// `ret`: run without looking the name up among the bodies.
    Native {
        ret: Ty,
    },
    General,
}

pub(super) struct Block {
    pub(super) ops: Vec<Op>,
    pub(super) term: Term,
}

/// An integer operation, as [`int_binop_of`] does it, with the 64-bit
/// types' arithmetic in the machine's own width; `None` for an error.
#[inline(always)]
fn int_op(op: BinOp, x: i128, y: i128, int: IntTy) -> Option<(Value, bool)> {
    use BinOp::*;
    let bits = |r: i128| Some((Value::Int(r), false));
    match (int, op) {
        (_, Eq) => Some((Value::Bool(x == y), false)),
        (_, Ne) => Some((Value::Bool(x != y), false)),
        (IntTy::U128, _) => int_binop_of(op, x, y, Some(int)).ok(),
        (_, Lt) => Some((Value::Bool(x < y), false)),
        (_, Le) => Some((Value::Bool(x <= y), false)),
        (_, Gt) => Some((Value::Bool(x > y), false)),
        (_, Ge) => Some((Value::Bool(x >= y), false)),
        // Two values of the type combine bit by bit into another.
        (_, BitAnd) => bits(x & y),
        (_, BitOr) => bits(x | y),
        (_, BitXor) => bits(x ^ y),
        (IntTy::I64 | IntTy::Isize, Add | Sub | Mul) => {
            let (a, b) = (x as i64, y as i64);
            let (r, of) = match op {
                Add => a.overflowing_add(b),
                Sub => a.overflowing_sub(b),
                _ => a.overflowing_mul(b),
            };
            Some((Value::Int(r as i128), of))
        }
        (IntTy::U64 | IntTy::Usize, Add | Sub | Mul) => {
            let (a, b) = (x as u64, y as u64);
            let (r, of) = match op {
                Add => a.overflowing_add(b),
                Sub => a.overflowing_sub(b),
                _ => a.overflowing_mul(b),
            };
            Some((Value::Int(r as i128), of))
        }
        _ => int_binop_of(op, x, y, Some(int)).ok(),
    }
}

/// `op` with its operands read straight from the frame when they are
/// copied locals or integer constants.
fn specialize(op: Op) -> Op {
    match op {
        Op::Use {
            dst,
            src: Src::Local(src, false),
        } => Op::CopyLocal { dst, src },
        Op::Int {
            dst,
            op,
            a: Src::Local(a, false),
            b: Src::Local(b, false),
            int,
        } => Op::IntLL { dst, op, a, b, int },
        Op::Int {
            dst,
            op,
            a: Src::Local(a, false),
            b: Src::Const(Scalar::Int(b)),
            int,
        } => Op::IntLC { dst, op, a, b, int },
        Op::Checked {
            dst,
            op,
            a: Src::Local(a, false),
            b: Src::Local(b, false),
            int,
        } => Op::CheckedLL { dst, op, a, b, int },
        Op::Checked {
            dst,
            op,
            a: Src::Local(a, false),
            b: Src::Const(Scalar::Int(b)),
            int,
        } => Op::CheckedLC { dst, op, a, b, int },
        op => op,
    }
}

/// Stores a checked step's pair in `slot` when it already holds one: `false`
/// leaves the store to [`Interp::store_checked`].
#[inline(always)]
fn store_checked_in_place(slot: &mut Value, v: &Value, overflow: bool) -> bool {
    if let Value::Agg(fs) = slot {
        if fs.len() == 2 && matches!(fs[0], Value::Int(_) | Value::Bool(_)) {
            Value::overwrite(&mut fs[0], v.copy());
            Value::overwrite(&mut fs[1], Value::Bool(overflow));
            return true;
        }
    }
    false
}

/// A type whose values are plain scalars.
fn plain_ty(tys: &TyInterner, ty: Ty) -> bool {
    matches!(
        tys.kind(ty),
        TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Unit
    )
}

impl<'a> Interp<'a> {
    /// `body` decoded, made on its first call.
    pub(super) fn decoded(&mut self, body: &'a Body) -> std::rc::Rc<[Block]> {
        let key = body as *const Body as usize;
        if let Some(d) = self.decoded.get(&key) {
            return d.clone();
        }
        let d: std::rc::Rc<[Block]> = body
            .blocks
            .iter()
            .map(|b| Block {
                ops: b
                    .statements
                    .iter()
                    .map(|s| {
                        self.decode_statement(body, &s.kind)
                            .map_or(Op::General, specialize)
                    })
                    .collect(),
                term: self
                    .decode_term(body, &b.terminator.kind)
                    .unwrap_or(Term::General),
            })
            .collect();
        self.decoded.insert(key, d.clone());
        d
    }

    /// A bare local of a scalar type.
    fn plain_local(&self, body: &Body, p: &Place) -> Option<u32> {
        (p.projection.is_empty() && plain_ty(self.tys, body.local(p.local).ty)).then_some(p.local.0)
    }

    /// A scalar operand: a plain local, a field of one or what a local
    /// reference points at, or a constant.
    fn decode_src(&self, body: &Body, o: &Operand) -> Option<(Src, Ty)> {
        match o {
            Operand::Copy(p) | Operand::Move(p) => {
                let local = body.local(p.local);
                let copy = matches!(o, Operand::Copy(_));
                match p.projection.as_slice() {
                    [] if plain_ty(self.tys, local.ty) => {
                        Some((Src::Local(p.local.0, !copy), local.ty))
                    }
                    [ProjElem::Field(f, fty)]
                        if copy
                            && plain_ty(self.tys, *fty)
                            && matches!(self.tys.kind(local.ty), TyKind::Tuple(_)) =>
                    {
                        Some((Src::Field(p.local.0, f.0), *fty))
                    }
                    [ProjElem::Deref] if copy => {
                        let to = self.tys.pointee(local.ty)?;
                        plain_ty(self.tys, to).then_some((Src::Deref(p.local.0), to))
                    }
                    _ => None,
                }
            }
            Operand::Const(c) => {
                let v = self.constant(c).ok()?;
                Some((Src::Const(Scalar::of(&v)?), c.ty))
            }
        }
    }

    fn decode_statement(&self, body: &Body, s: &StatementKind) -> Option<Op> {
        let int_of = |ty: Ty| match self.tys.kind(ty) {
            TyKind::Int(it) => Some(it),
            _ => None,
        };
        Some(match s {
            StatementKind::Assign(p, Rvalue::Use(o))
                if matches!(p.projection.as_slice(), [ProjElem::Deref])
                    && self
                        .tys
                        .pointee(body.local(p.local).ty)
                        .is_some_and(|t| plain_ty(self.tys, t)) =>
            {
                let (src, _) = self.decode_src(body, o)?;
                Op::Store {
                    dst: p.local.0,
                    src,
                }
            }
            StatementKind::Assign(p, Rvalue::Use(o)) => {
                let dst = self.plain_local(body, p)?;
                let (src, _) = self.decode_src(body, o)?;
                Op::Use { dst, src }
            }
            StatementKind::Assign(p, Rvalue::BinaryOp(op, a, b)) => {
                let dst = self.plain_local(body, p)?;
                let (a, ty) = self.decode_src(body, a)?;
                let (b, _) = self.decode_src(body, b)?;
                Op::Int {
                    dst,
                    op: *op,
                    a,
                    b,
                    int: int_of(ty)?,
                }
            }
            StatementKind::Assign(p, Rvalue::CheckedBinaryOp(op, a, b))
                if p.projection.is_empty() =>
            {
                let (a, ty) = self.decode_src(body, a)?;
                let (b, _) = self.decode_src(body, b)?;
                Op::Checked {
                    dst: p.local.0,
                    op: *op,
                    a,
                    b,
                    int: int_of(ty)?,
                }
            }
            StatementKind::Assign(p, Rvalue::Cast(CastKind::IntToInt, o, to)) => {
                let dst = self.plain_local(body, p)?;
                let (src, from) = self.decode_src(body, o)?;
                int_of(from)?;
                Op::IntCast {
                    dst,
                    src,
                    int: int_of(*to)?,
                }
            }
            StatementKind::Assign(p, Rvalue::Ref(_, q))
                if p.projection.is_empty()
                    && matches!(q.projection.as_slice(), [ProjElem::Deref])
                    && self.tys.pointee(body.local(q.local).ty).is_some() =>
            {
                Op::Reborrow {
                    dst: p.local.0,
                    src: q.local.0,
                }
            }
            StatementKind::Assign(p, Rvalue::UnaryOp(op, o)) => {
                let dst = self.plain_local(body, p)?;
                let (src, ty) = self.decode_src(body, o)?;
                let int = match (op, self.tys.kind(ty)) {
                    (UnOp::Not, TyKind::Bool) => None,
                    (UnOp::Not | UnOp::Neg, TyKind::Int(it)) => Some(it),
                    _ => return None,
                };
                Op::Unary {
                    dst,
                    op: *op,
                    src,
                    int,
                }
            }
            StatementKind::Assign(p, Rvalue::Aggregate(AggregateKind::Tuple, ops))
                if p.projection.is_empty() && !ops.is_empty() =>
            {
                let parts = ops
                    .iter()
                    .map(|o| self.decode_src(body, o).map(|(s, _)| s))
                    .collect::<Option<Box<[Src]>>>()?;
                // A tuple of scalars owns nothing.
                let TyKind::Tuple(ts) = self.tys.kind(body.local(p.local).ty) else {
                    return None;
                };
                if !ts.iter().all(|t| plain_ty(self.tys, *t)) {
                    return None;
                }
                Op::Tuple {
                    dst: p.local.0,
                    parts,
                }
            }
            StatementKind::Assign(p, Rvalue::Ref(_, q))
                if p.projection.is_empty() && q.projection.is_empty() =>
            {
                Op::RefLocal {
                    dst: p.local.0,
                    src: q.local.0,
                }
            }
            StatementKind::StorageLive(l) => Op::Live(l.0),
            StatementKind::StorageDead(l) if plain_ty(self.tys, body.local(*l).ty) => {
                Op::DeadPlain(l.0)
            }
            _ => return None,
        })
    }

    fn decode_term(&self, body: &Body, t: &TerminatorKind) -> Option<Term> {
        Some(match t {
            TerminatorKind::Goto { target } => Term::Goto(*target),
            TerminatorKind::SwitchInt { discr, .. } => {
                let (src, ty) = self.decode_src(body, discr)?;
                let int = match self.tys.kind(ty) {
                    TyKind::Int(it) => Some(it),
                    TyKind::Bool | TyKind::Char => None,
                    _ => return None,
                };
                match (src, self.tys.kind(ty)) {
                    (Src::Local(l, false), TyKind::Bool) => Term::SwitchBool(l),
                    (Src::Field(l, 1), TyKind::Bool) => Term::SwitchFlag(l),
                    _ => Term::Switch { src, int },
                }
            }
            TerminatorKind::Call {
                func:
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(inst),
                        ..
                    }),
                args,
                destination,
                target: Some(target),
                unwind: UnwindAction::Abort,
            } if destination.projection.is_empty()
                && inst.name.starts_with("Vec[")
                && (inst.name.ends_with("].index") || inst.name.ends_with("].index_mut"))
                // An element that is a reference is the result itself
                // (`collapse_ref_of_ref`), which the native call gives.
                && !inst.name.starts_with("Vec[ref ")
                && !inst.name.starts_with("Vec[mut ref ")
                && !self.program.bodies.contains_key(&inst.name) =>
            {
                let [Operand::Copy(r) | Operand::Move(r), index] = args.as_slice() else {
                    return None;
                };
                if !r.projection.is_empty() {
                    return None;
                }
                let (index, _) = self.decode_src(body, index)?;
                Term::VecIndex {
                    dst: destination.local.0,
                    recv: r.local.0,
                    moved: matches!(args[0], Operand::Move(_)),
                    index,
                    target: *target,
                }
            }
            TerminatorKind::Call {
                func:
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(inst),
                        ..
                    }),
                destination,
                target: Some(_),
                unwind: UnwindAction::Abort,
                ..
            } if !self.program.bodies.contains_key(&inst.name)
                // The general call reads a `Vec`'s index its own way.
                && !(inst.name.starts_with("Vec[")
                    && (inst.name.ends_with("].index") || inst.name.ends_with("].index_mut"))) =>
            {
                Term::Native {
                    ret: place_ty(body, self.tys, destination).ok()?.ty,
                }
            }
            _ => return None,
        })
    }

    /// A decoded native call: the general call's steps for a function
    /// with no body, in the same order.
    pub(super) fn native_call(
        &mut self,
        body: &Body,
        kind: &TerminatorKind,
        ret_ty: Ty,
    ) -> R<Option<BasicBlock>> {
        let TerminatorKind::Call {
            func:
                Operand::Const(Const {
                    kind: ConstKind::FnDef(inst),
                    ..
                }),
            args,
            destination,
            target,
            ..
        } = kind
        else {
            unreachable!("decoded from a call of a function item");
        };
        let name = inst.name.as_str();
        let mut vals = Vec::with_capacity(args.len());
        let mut arg_tys = Vec::with_capacity(args.len());
        for a in args {
            let (v, t) = self.operand(body, a)?;
            vals.push(v);
            arg_tys.push(t);
        }
        let v = self.native(name, vals, &arg_tys, ret_ty)?;
        let v = self.collapse_ref_of_ref(v);
        let ret = self.inline_str(v, ret_ty)?;
        self.finish_call(body, destination, target, ret, name)
    }

    #[inline(always)]
    fn locals(&mut self) -> &mut Vec<Value> {
        &mut self.frames.last_mut().expect("frame").locals
    }

    /// `dst = (v, overflow)`, a checked step's pair, in the pair already
    /// there or in a reused buffer.
    #[inline(always)]
    fn store_checked(&mut self, dst: u32, v: Value, overflow: bool) {
        if store_checked_in_place(&mut self.locals()[dst as usize], &v, overflow) {
            return;
        }
        let mut pair = self.small_buf();
        pair.push(v);
        pair.push(Value::Bool(overflow));
        let old = std::mem::replace(&mut self.locals()[dst as usize], Value::Agg(pair));
        self.recycle_value(old);
    }

    /// The scalar at `src`, when it is initialized; reading it changes
    /// nothing (a move is finished by [`Interp::take`]).
    #[inline(always)]
    fn peek(&self, src: Src) -> Option<Scalar> {
        let locals = &self.frames.last()?.locals;
        match src {
            Src::Local(l, _) => Scalar::of(locals.get(l as usize)?),
            Src::Field(l, f) => match locals.get(l as usize)? {
                Value::Agg(fs) => Scalar::of(fs.get(f as usize)?),
                _ => None,
            },
            Src::Deref(l) => match locals.get(l as usize)? {
                Value::Ref(a) => Scalar::of(self.slot_ref(a).ok()?),
                _ => None,
            },
            Src::Const(c) => Some(c),
        }
    }

    #[inline(always)]
    fn peek_int(&self, src: Src) -> Option<i128> {
        match self.peek(src)? {
            Scalar::Int(i) => Some(i),
            _ => None,
        }
    }

    /// Finishes reading `src`: a moved local is left uninitialized.
    #[inline(always)]
    fn take(&mut self, src: Src) {
        if let Src::Local(l, true) = src {
            self.locals()[l as usize] = Value::Uninit;
        }
    }

    /// Runs a decoded statement; `false` leaves it, untouched, to the
    /// general statement.
    #[inline(always)]
    pub(super) fn fast_op(&mut self, op: &Op) -> bool {
        match *op {
            Op::Use { dst, src } => {
                let Some(v) = self.peek(src) else {
                    return false;
                };
                self.take(src);
                Value::overwrite(&mut self.locals()[dst as usize], v.value());
                true
            }
            Op::Int { dst, op, a, b, int } => {
                let (Some(x), Some(y)) = (self.peek_int(a), self.peek_int(b)) else {
                    return false;
                };
                let Some((v, false)) = int_op(op, x, y, int) else {
                    return false;
                };
                self.take(a);
                self.take(b);
                Value::overwrite(&mut self.locals()[dst as usize], v);
                true
            }
            Op::Checked { dst, op, a, b, int } => {
                let (Some(x), Some(y)) = (self.peek_int(a), self.peek_int(b)) else {
                    return false;
                };
                let Some((v, overflow)) = int_op(op, x, y, int) else {
                    return false;
                };
                self.take(a);
                self.take(b);
                self.store_checked(dst, v, overflow);
                true
            }
            Op::IntCast { dst, src, int } => {
                let Some(x) = self.peek_int(src) else {
                    return false;
                };
                self.take(src);
                Value::overwrite(&mut self.locals()[dst as usize], Value::Int(wrap(x, int)));
                true
            }
            Op::Reborrow { dst, src } => {
                let Some(Value::Ref(a)) =
                    self.frames.last().and_then(|f| f.locals.get(src as usize))
                else {
                    return false;
                };
                // The general step checks the place is still there.
                if self.slot_ref(a).is_err() {
                    return false;
                }
                let r = Value::Ref(a.clone());
                let slot = &mut self.locals()[dst as usize];
                if !matches!(slot, Value::Uninit | Value::Ref(_)) {
                    return false;
                }
                *slot = r;
                true
            }
            Op::Unary { dst, op, src, int } => {
                let v = match (op, self.peek(src), int) {
                    (UnOp::Not, Some(Scalar::Bool(b)), None) => Value::Bool(!b),
                    (UnOp::Not, Some(Scalar::Int(i)), Some(it)) => Value::Int(wrap(!i, it)),
                    (UnOp::Neg, Some(Scalar::Int(i)), Some(it)) => match i.overflowing_neg() {
                        (r, false) if wrap(r, it) == r => Value::Int(r),
                        _ => return false,
                    },
                    _ => return false,
                };
                self.take(src);
                Value::overwrite(&mut self.locals()[dst as usize], v);
                true
            }
            Op::Tuple { dst, ref parts } => {
                let mut vals = self.small_buf();
                for &p in parts.iter() {
                    let Some(v) = self.peek(p) else {
                        return false;
                    };
                    vals.push(v.value());
                }
                for &p in parts.iter() {
                    self.take(p);
                }
                self.locals()[dst as usize] = Value::Agg(vals);
                true
            }
            Op::RefLocal { dst, src } => {
                let fi = self.frames.len() - 1;
                let f = &self.frames[fi];
                if !matches!(f.locals[dst as usize], Value::Uninit | Value::Ref(_)) {
                    return false;
                }
                let r = Value::Ref(Addr {
                    root: Root::Local {
                        frame: fi,
                        id: f.id,
                        local: Local(src),
                    },
                    path: Path::new(),
                });
                self.locals()[dst as usize] = r;
                true
            }
            Op::Store { dst, src } => {
                let Some(v) = self.peek(src) else {
                    return false;
                };
                let Some(Value::Ref(a)) =
                    self.frames.last().and_then(|f| f.locals.get(dst as usize))
                else {
                    return false;
                };
                let a = a.clone();
                match self.slot_ref(&a) {
                    Ok(old) if old.is_plain() => {}
                    _ => return false,
                }
                self.take(src);
                match self.slot_mut(&a) {
                    Ok(slot) => Value::overwrite(slot, v.value()),
                    Err(_) => unreachable!("the slot was just read"),
                }
                true
            }
            Op::CopyLocal { dst, src } => {
                let locals = self.locals();
                let v = match locals[src as usize] {
                    Value::Int(i) => Value::Int(i),
                    Value::Bool(b) => Value::Bool(b),
                    Value::Char(c) => Value::Char(c),
                    Value::Unit => Value::Unit,
                    _ => return false,
                };
                Value::overwrite(&mut locals[dst as usize], v);
                true
            }
            Op::IntLL { dst, op, a, b, int } => {
                let locals = self.locals();
                let (&Value::Int(x), &Value::Int(y)) = (&locals[a as usize], &locals[b as usize])
                else {
                    return false;
                };
                let Some((v, false)) = int_op(op, x, y, int) else {
                    return false;
                };
                Value::overwrite(&mut locals[dst as usize], v);
                true
            }
            Op::IntLC { dst, op, a, b, int } => {
                let locals = self.locals();
                let Value::Int(x) = locals[a as usize] else {
                    return false;
                };
                let Some((v, false)) = int_op(op, x, b, int) else {
                    return false;
                };
                Value::overwrite(&mut locals[dst as usize], v);
                true
            }
            Op::CheckedLL { dst, op, a, b, int } => {
                let locals = self.locals();
                let (&Value::Int(x), &Value::Int(y)) = (&locals[a as usize], &locals[b as usize])
                else {
                    return false;
                };
                let Some((v, overflow)) = int_op(op, x, y, int) else {
                    return false;
                };
                self.store_checked(dst, v, overflow);
                true
            }
            Op::CheckedLC { dst, op, a, b, int } => {
                let locals = self.locals();
                let Value::Int(x) = locals[a as usize] else {
                    return false;
                };
                let Some((v, overflow)) = int_op(op, x, b, int) else {
                    return false;
                };
                self.store_checked(dst, v, overflow);
                true
            }
            Op::Live(l) => {
                self.locals()[l as usize] = Value::Uninit;
                true
            }
            Op::DeadPlain(l) => {
                Value::overwrite(&mut self.locals()[l as usize], Value::Uninit);
                true
            }
            Op::General => false,
        }
    }

    /// The block a decoded terminator goes to, or `None` to run the
    /// general one.
    #[inline(always)]
    pub(super) fn fast_term(&mut self, term: &Term, kind: &TerminatorKind) -> Option<BasicBlock> {
        match (term, kind) {
            (Term::Goto(t), _) => Some(*t),
            (&Term::SwitchBool(l), TerminatorKind::SwitchInt { targets, .. }) => {
                let Value::Bool(b) = self.locals()[l as usize] else {
                    return None;
                };
                Some(pick(targets, b as u128))
            }
            (&Term::SwitchFlag(l), TerminatorKind::SwitchInt { targets, .. }) => {
                let Value::Agg(fs) = &self.locals()[l as usize] else {
                    return None;
                };
                let Some(&Value::Bool(b)) = fs.get(1) else {
                    return None;
                };
                Some(pick(targets, b as u128))
            }
            (Term::Switch { src, int }, TerminatorKind::SwitchInt { targets, .. }) => {
                let bits = match (self.peek(*src)?, int) {
                    (Scalar::Bool(b), None) => b as u128,
                    (Scalar::Char(c), None) => c as u128,
                    (Scalar::Int(i), Some(it)) => to_bits(i, *it),
                    _ => return None,
                };
                self.take(*src);
                Some(pick(targets, bits))
            }
            (
                &Term::VecIndex {
                    dst,
                    recv,
                    moved,
                    index,
                    target,
                },
                _,
            ) => {
                let i = self.peek_int(index)?;
                let id = {
                    let locals = &self.frames.last()?.locals;
                    let mut v = locals.get(recv as usize)?;
                    // `box_behind`: through references to the box.
                    loop {
                        match v {
                            Value::Box(a) => break *a,
                            Value::Ref(addr) => v = self.slot_ref(addr).ok()?,
                            _ => return None,
                        }
                    }
                };
                let len = match self.heap.get(id.0 as usize) {
                    Some(Some(obj)) => match &obj.value {
                        Value::Agg(fs) => fs.len() as i128,
                        _ => return None,
                    },
                    _ => return None,
                };
                if i < 0 || i >= len {
                    return None;
                }
                let locals = self.locals();
                if !matches!(locals[dst as usize], Value::Uninit | Value::Ref(_)) {
                    return None;
                }
                if moved {
                    locals[recv as usize] = Value::Uninit;
                }
                self.take(index);
                self.locals()[dst as usize] = Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: Path::from_slice(&[i as u64]),
                });
                Some(target)
            }
            _ => None,
        }
    }
}

/// The block a `switchInt` on `bits` goes to.
#[inline(always)]
fn pick(targets: &SwitchTargets, bits: u128) -> BasicBlock {
    targets
        .values
        .iter()
        .find(|(v, _)| *v == bits)
        .map_or(targets.otherwise, |(_, b)| *b)
}
