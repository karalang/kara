//! The stable textual form of a MIR body (`docs/spikes/mir-types.md` §7).
//!
//! The output depends only on the body: locals and blocks print in index
//! order, and nothing reads a hash map's iteration order or an address,
//! so two builds of the same program print byte-identical MIR.

use std::fmt::Write;

use super::place_ty::{place_ty, PlaceTy};
use super::syntax::*;
use super::ty::{TyInterner, TyKind};

pub fn pretty_body(body: &Body, tys: &TyInterner) -> String {
    let mut out = String::new();
    let args: Vec<String> = body
        .args()
        .map(|a| format!("{a}: {}", tys.display(body.local(a).ty)))
        .collect();
    let _ = writeln!(
        out,
        "fn {}({}) -> {} {{",
        body.instance.name,
        args.join(", "),
        tys.display(body.return_ty())
    );
    for (i, decl) in body.locals.iter().enumerate() {
        if (1..=body.arg_count).contains(&i) {
            continue;
        }
        let m = match decl.mutability {
            Mutability::Mut => "mut ",
            Mutability::Not => "",
        };
        let note = match &decl.kind {
            LocalKind::User { name, .. } => format!(" // {name}"),
            LocalKind::DropFlag => " // drop flag".to_string(),
            _ => String::new(),
        };
        let _ = writeln!(out, "    let {m}_{i}: {};{note}", tys.display(decl.ty));
    }
    for (i, r) in body.par_regions.iter().enumerate() {
        let (kind, limit) = match &r.kind {
            ParKind::Block => ("block", String::new()),
            ParKind::For { limit: None } => ("for", String::new()),
            ParKind::For { limit: Some(n) } => ("for", format!(" limit {}", operand(body, tys, n))),
        };
        let branches: Vec<String> = r
            .branches
            .iter()
            .map(|bs| {
                let names: Vec<String> = bs.iter().map(|b| b.to_string()).collect();
                format!("[{}]", names.join(" "))
            })
            .collect();
        let _ = writeln!(out, "    par#{i} {kind} {}{limit}", branches.join(" "));
    }
    for (i, block) in body.blocks.iter().enumerate() {
        let _ = writeln!(out, "    bb{i}: {{");
        for st in &block.statements {
            let _ = writeln!(out, "        {};", statement(body, tys, &st.kind));
        }
        let _ = writeln!(
            out,
            "        {};",
            terminator(body, tys, &block.terminator.kind)
        );
        let _ = writeln!(out, "    }}");
    }
    out.push_str("}\n");
    out
}

pub fn statement(body: &Body, tys: &TyInterner, s: &StatementKind) -> String {
    match s {
        StatementKind::Assign(p, rv) => {
            format!("{} = {}", place(body, tys, p), rvalue(body, tys, rv))
        }
        StatementKind::StorageLive(l) => format!("StorageLive({l})"),
        StatementKind::StorageDead(l) => format!("StorageDead({l})"),
        StatementKind::SetDiscriminant(p, v) => {
            format!(
                "discriminant({}) = {}",
                place(body, tys, p),
                variant_name(body, tys, p, v.0)
            )
        }
        StatementKind::BorrowFlag(FlagOp::Acquire {
            place: p,
            kind,
            loan,
        }) => {
            format!(
                "flag_acquire({}{}, L{loan})",
                ref_sigil(*kind),
                place(body, tys, p)
            )
        }
        StatementKind::BorrowFlag(FlagOp::Release { loan }) => format!("flag_release(L{loan})"),
        StatementKind::BorrowFlag(FlagOp::Check { place: p, kind }) => {
            format!("flag_check({}{})", ref_sigil(*kind), place(body, tys, p))
        }
        StatementKind::Nop => "nop".to_string(),
    }
}

fn ref_sigil(kind: BorrowKind) -> &'static str {
    match kind {
        BorrowKind::Shared => "&",
        BorrowKind::Mut => "&mut ",
    }
}

pub fn terminator(body: &Body, tys: &TyInterner, t: &TerminatorKind) -> String {
    match t {
        TerminatorKind::Goto { target } => format!("goto -> {target}"),
        TerminatorKind::SwitchInt { discr, targets } => {
            let mut arms: Vec<String> = targets
                .values
                .iter()
                .map(|(v, b)| format!("{v}: {b}"))
                .collect();
            arms.push(format!("otherwise: {}", targets.otherwise));
            format!(
                "switchInt({}) -> [{}]",
                operand(body, tys, discr),
                arms.join(", ")
            )
        }
        // `Abort`, the only unwind action, is not printed.
        TerminatorKind::Call {
            func,
            args,
            destination,
            target,
            unwind: UnwindAction::Abort,
        } => {
            let callee = match func {
                Operand::Const(Const {
                    kind: ConstKind::FnDef(inst),
                    ..
                }) => inst.name.clone(),
                other => operand(body, tys, other),
            };
            let args: Vec<String> = args.iter().map(|a| operand(body, tys, a)).collect();
            let next = match target {
                Some(b) => b.to_string(),
                None => "!".to_string(),
            };
            format!(
                "{} = {}({}) -> {}",
                place(body, tys, destination),
                callee,
                args.join(", "),
                next
            )
        }
        TerminatorKind::Drop {
            place: p,
            target,
            unwind: UnwindAction::Abort,
        } => {
            format!("drop({}) -> {target}", place(body, tys, p))
        }
        TerminatorKind::Return => "return".to_string(),
        TerminatorKind::Abort { reason } => format!("abort({reason:?})"),
        TerminatorKind::Unreachable => "unreachable".to_string(),
    }
}

pub fn place(body: &Body, tys: &TyInterner, p: &Place) -> String {
    let mut s = p.local.to_string();
    let mut cur = Place::local(p.local);
    for elem in &p.projection {
        s = match elem {
            ProjElem::Field(f, _) => format!("{s}.{}", f.0),
            ProjElem::Downcast(v) => {
                format!("({s} as {})", variant_name(body, tys, &cur, v.0))
            }
            ProjElem::Deref => format!("(*{s})"),
            ProjElem::Index(l) => format!("{s}[{l}]"),
            ProjElem::ConstIndex(i) => format!("{s}[{i}]"),
        };
        cur = cur.project(*elem);
    }
    s
}

fn variant_name(body: &Body, tys: &TyInterner, base: &Place, v: u32) -> String {
    if let Ok(PlaceTy { ty, .. }) = place_ty(body, tys, base) {
        if let TyKind::Adt(a) | TyKind::Shared(a) = tys.kind(ty) {
            if let Some(var) = tys.adt(a).variants.get(v as usize) {
                return var.name.clone();
            }
        }
    }
    format!("variant#{v}")
}

pub fn operand(body: &Body, tys: &TyInterner, o: &Operand) -> String {
    match o {
        Operand::Copy(p) => format!("copy {}", place(body, tys, p)),
        Operand::Move(p) => format!("move {}", place(body, tys, p)),
        Operand::Const(c) => format!("const {}", constant(tys, c)),
    }
}

pub fn constant(tys: &TyInterner, c: &Const) -> String {
    match &c.kind {
        ConstKind::Scalar(v) => match tys.kind(c.ty) {
            TyKind::Bool => (*v != 0).to_string(),
            TyKind::Char => match char::from_u32(*v as u32) {
                Some(ch) => format!("{ch:?}"),
                None => format!("char#{v}"),
            },
            TyKind::Int(i) => {
                if i.signed() {
                    let bits = i.bits();
                    let shift = 128 - bits;
                    let signed = ((*v as i128) << shift) >> shift;
                    format!("{signed}_{}", i.name())
                } else {
                    format!("{v}_{}", i.name())
                }
            }
            _ => format!("{v}_{}", tys.display(c.ty)),
        },
        ConstKind::Float(bits) => format!("{:?}_{}", f64::from_bits(*bits), tys.display(c.ty)),
        ConstKind::Str(s) => format!("{s:?}"),
        ConstKind::Unit => "()".to_string(),
        ConstKind::FnDef(inst) => inst.name.clone(),
        ConstKind::ZeroSized => format!("<ZST {}>", tys.display(c.ty)),
    }
}

pub fn rvalue(body: &Body, tys: &TyInterner, rv: &Rvalue) -> String {
    let ops = |v: &[Operand]| -> String {
        v.iter()
            .map(|o| operand(body, tys, o))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match rv {
        Rvalue::Use(o) => operand(body, tys, o),
        Rvalue::Ref(BorrowKind::Shared, p) => format!("&{}", place(body, tys, p)),
        Rvalue::Ref(BorrowKind::Mut, p) => format!("&mut {}", place(body, tys, p)),
        Rvalue::Retain(p) => format!("retain({})", place(body, tys, p)),
        Rvalue::BinaryOp(op, a, b) => format!(
            "{}({}, {})",
            op.name(),
            operand(body, tys, a),
            operand(body, tys, b)
        ),
        Rvalue::CheckedBinaryOp(op, a, b) => format!(
            "Checked{}({}, {})",
            op.name(),
            operand(body, tys, a),
            operand(body, tys, b)
        ),
        Rvalue::UnaryOp(op, a) => format!("{op:?}({})", operand(body, tys, a)),
        Rvalue::Cast(k, o, t) => {
            format!("{} as {} ({k:?})", operand(body, tys, o), tys.display(*t))
        }
        Rvalue::Discriminant(p) => format!("discriminant({})", place(body, tys, p)),
        Rvalue::Len(p) => format!("Len({})", place(body, tys, p)),
        Rvalue::NullaryOp(NullOp::SizeOf, t) => format!("SizeOf({})", tys.display(*t)),
        Rvalue::NullaryOp(NullOp::AlignOf, t) => format!("AlignOf({})", tys.display(*t)),
        Rvalue::Aggregate(kind, v) => match kind {
            AggregateKind::Tuple => format!("({})", ops(v)),
            AggregateKind::Array(_) => format!("[{}]", ops(v)),
            AggregateKind::Adt { ty, variant } | AggregateKind::Shared { ty, variant } => {
                let shared = matches!(kind, AggregateKind::Shared { .. });
                let (name, vname) = match tys.kind(*ty) {
                    TyKind::Adt(a) | TyKind::Shared(a) => {
                        let adt = tys.adt(a);
                        let vname = if adt.is_enum {
                            adt.variants
                                .get(variant.index())
                                .map(|v| format!(".{}", v.name))
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        (adt.name.clone(), vname)
                    }
                    _ => (tys.display(*ty), String::new()),
                };
                let prefix = if shared { "shared " } else { "" };
                format!("{prefix}{name}{vname} {{ {} }}", ops(v))
            }
            AggregateKind::Closure { ty } => format!("{} [{}]", tys.display(*ty), ops(v)),
        },
    }
}
