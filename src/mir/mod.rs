//! MIR: the control-flow-graph IR between typed HIR and LLVM
//! (redesign M1; design in `docs/spikes/mir-types.md`).
//!
//! This module holds the data types, a block-by-block builder, place
//! typing, the textual form and the structural validator. The HIR-to-MIR
//! lowering, drop elaboration, the borrow checker and the MIR interpreter
//! build on it. `ty` is a placeholder interner until the typed HIR's
//! interned types land; MIR only needs the queries it exposes.

pub mod build;
pub mod elaborate;
pub mod interp;
pub mod movecheck;
pub mod parse;
pub mod place_ty;
pub mod pretty;
pub mod syntax;
pub mod ty;
pub mod validate;

pub use build::BodyBuilder;
pub use elaborate::elaborate_drops;
pub use movecheck::check_moves;
pub use parse::{parse_module, pretty_module, MirModule};
pub use pretty::pretty_body;
pub use syntax::*;
pub use ty::{AdtDef, AdtId, IntTy, Ty, TyInterner, TyKind, VariantDef};
pub use validate::validate;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::DefId;

    fn inst(name: &str) -> InstanceId {
        InstanceId {
            def: DefId(0),
            args: Vec::new(),
            name: name.to_string(),
        }
    }

    /// `struct R { id: i64 }` with a Drop body, and
    /// `enum E { A(R), B }`.
    fn types() -> (TyInterner, Ty, Ty, Ty) {
        let tys = TyInterner::new();
        let i64t = tys.int(IntTy::I64);
        let r = tys.add_adt(AdtDef {
            def: DefId(1),
            name: "R".into(),
            is_enum: false,
            variants: vec![VariantDef {
                name: "R".into(),
                fields: vec![("id".into(), i64t)],
            }],
            has_drop_impl: true,
            is_copy: false,
        });
        let rt = tys.intern(TyKind::Adt(r));
        let e = tys.add_adt(AdtDef {
            def: DefId(2),
            name: "E".into(),
            is_enum: true,
            variants: vec![
                VariantDef {
                    name: "A".into(),
                    fields: vec![("0".into(), rt)],
                },
                VariantDef {
                    name: "B".into(),
                    fields: vec![],
                },
            ],
            has_drop_impl: false,
            is_copy: false,
        });
        let et = tys.intern(TyKind::Adt(e));
        (tys, i64t, rt, et)
    }

    /// `fn eat(r: R) { if c { consume(r) } }`, hand-lowered: the shape
    /// drop elaboration has to handle (a conditionally moved parameter).
    fn eat(tys: &mut TyInterner, rt: Ty) -> Body {
        let unit = tys.unit();
        let boolt = tys.bool();
        let mut b = BodyBuilder::new(inst("eat"), unit);
        let r = b.arg("r", rt);
        let c = b.arg("c", boolt);
        let tmp = b.temp(unit);
        let bb0 = b.new_block();
        let bb1 = b.new_block();
        let bb2 = b.new_block();
        let bb3 = b.new_block();
        b.terminate(
            bb0,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(c.into()),
                targets: SwitchTargets::if_else(bb1, bb2),
            },
        );
        b.terminate(
            bb1,
            TerminatorKind::Call {
                func: Operand::Const(Const {
                    ty: unit,
                    kind: ConstKind::FnDef(inst("consume")),
                }),
                args: vec![Operand::Move(r.into())],
                destination: tmp.into(),
                target: Some(bb3),
            },
        );
        b.terminate(
            bb2,
            TerminatorKind::Drop {
                place: r.into(),
                target: bb3,
            },
        );
        b.assign(
            bb3,
            Local::RETURN_PLACE,
            Rvalue::Use(Operand::Const(Const {
                ty: unit,
                kind: ConstKind::Unit,
            })),
        );
        b.terminate(bb3, TerminatorKind::Return);
        b.finish().unwrap()
    }

    #[test]
    fn mir_eat_pretty_prints_and_validates() {
        let (mut tys, _, rt, _) = types();
        let body = eat(&mut tys, rt);
        assert_eq!(validate(&body, &tys), Vec::<String>::new());
        let expected = "\
fn eat(_1: R, _2: bool) -> () {
    let mut _0: ();
    let mut _3: ();
    bb0: {
        switchInt(copy _2) -> [0: bb2, otherwise: bb1];
    }
    bb1: {
        _3 = consume(move _1) -> bb3;
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
        assert_eq!(pretty_body(&body, &tys), expected);
    }

    #[test]
    fn mir_pretty_prints_projections_and_constants() {
        let (tys, i64t, rt, et) = types();
        let mut b = BodyBuilder::new(inst("f"), i64t);
        let e = b.arg("e", et);
        let bb0 = b.new_block();
        let id = Place::from(e)
            .project(ProjElem::Downcast(VariantIdx(0)))
            .field(0, rt)
            .field(0, i64t);
        b.assign(
            bb0,
            Local::RETURN_PLACE,
            Rvalue::BinaryOp(
                BinOp::Add,
                Operand::Copy(id),
                Operand::Const(Const {
                    ty: i64t,
                    kind: ConstKind::Scalar((-5i64) as u128),
                }),
            ),
        );
        b.terminate(bb0, TerminatorKind::Return);
        let body = b.finish().unwrap();
        assert_eq!(validate(&body, &tys), Vec::<String>::new());
        let text = pretty_body(&body, &tys);
        assert!(
            text.contains("_0 = Add(copy (_1 as A).0.0, const -5_i64);"),
            "{text}"
        );
        // Interning is stable: the same kind gives the same handle.
        assert_eq!(tys.int(IntTy::I64), i64t);
    }

    fn one_block(
        tys: &mut TyInterner,
        args: &[(&str, Ty)],
        ret: Ty,
        f: impl FnOnce(&mut BodyBuilder, &[Local], BasicBlock),
    ) -> Body {
        let mut b = BodyBuilder::new(inst("t"), ret);
        let locals: Vec<Local> = args.iter().map(|(n, t)| b.arg(n, *t)).collect();
        let bb0 = b.new_block();
        f(&mut b, &locals, bb0);
        let _ = tys;
        b.finish().unwrap()
    }

    fn errors_mention(errs: &[String], needle: &str) -> bool {
        errs.iter().any(|e| e.contains(needle))
    }

    #[test]
    fn mir_validator_rejects_copy_of_non_copy() {
        let (mut tys, _, rt, _) = types();
        let body = one_block(&mut tys, &[("r", rt)], rt, |b, a, bb| {
            b.assign(
                bb,
                Local::RETURN_PLACE,
                Rvalue::Use(Operand::Copy(a[0].into())),
            );
            b.terminate(bb, TerminatorKind::Return);
        });
        let errs = validate(&body, &tys);
        assert!(errors_mention(&errs, "is not Copy"), "{errs:?}");
    }

    #[test]
    fn mir_validator_rejects_move_out_of_index_and_deref() {
        let (mut tys, _, rt, _) = types();
        let arr = tys.intern(TyKind::Array(rt, 2));
        let rref = tys.intern(TyKind::Ref(rt));
        let body = one_block(&mut tys, &[("a", arr), ("p", rref)], rt, |b, a, bb| {
            b.assign(
                bb,
                Local::RETURN_PLACE,
                Rvalue::Use(Operand::Move(
                    Place::from(a[0]).project(ProjElem::ConstIndex(1)),
                )),
            );
            b.assign(
                bb,
                Local::RETURN_PLACE,
                Rvalue::Use(Operand::Move(Place::from(a[1]).project(ProjElem::Deref))),
            );
            b.terminate(bb, TerminatorKind::Return);
        });
        let errs = validate(&body, &tys);
        assert_eq!(
            errs.iter()
                .filter(|e| e.contains("behind a reference or an index"))
                .count(),
            2,
            "{errs:?}"
        );
    }

    #[test]
    fn mir_validator_rejects_move_out_of_shared() {
        let (mut tys, i64t, _, _) = types();
        let s = tys.add_adt(AdtDef {
            def: DefId(3),
            name: "Node".into(),
            is_enum: false,
            variants: vec![VariantDef {
                name: "Node".into(),
                fields: vec![("v".into(), i64t)],
            }],
            has_drop_impl: false,
            is_copy: false,
        });
        let st = tys.intern(TyKind::Shared(s));
        let body = one_block(&mut tys, &[("n", st)], i64t, |b, a, bb| {
            b.assign(
                bb,
                Local::RETURN_PLACE,
                Rvalue::Use(Operand::Move(Place::from(a[0]).field(0, i64t))),
            );
            b.terminate(bb, TerminatorKind::Return);
        });
        let errs = validate(&body, &tys);
        assert!(errors_mention(&errs, "inside a shared value"), "{errs:?}");
    }

    #[test]
    fn mir_validator_rejects_bad_jump_and_bad_downcast() {
        let (mut tys, i64t, rt, _) = types();
        let body = one_block(&mut tys, &[("r", rt)], i64t, |b, a, bb| {
            b.assign(
                bb,
                Local::RETURN_PLACE,
                Rvalue::Use(Operand::Copy(
                    Place::from(a[0])
                        .project(ProjElem::Downcast(VariantIdx(0)))
                        .field(0, i64t),
                )),
            );
            b.terminate(
                bb,
                TerminatorKind::Goto {
                    target: BasicBlock(7),
                },
            );
        });
        let errs = validate(&body, &tys);
        assert!(errors_mention(&errs, "downcast of non-enum R"), "{errs:?}");
        assert!(errors_mention(&errs, "jump to missing bb7"), "{errs:?}");
    }

    #[test]
    fn mir_validator_rejects_wrong_field_type_and_enum_field_without_downcast() {
        let (mut tys, i64t, rt, et) = types();
        let boolt = tys.bool();
        let body = one_block(&mut tys, &[("r", rt), ("e", et)], i64t, |b, a, bb| {
            b.assign(
                bb,
                Local::RETURN_PLACE,
                Rvalue::Use(Operand::Copy(Place::from(a[0]).field(0, boolt))),
            );
            b.assign(
                bb,
                Local::RETURN_PLACE,
                Rvalue::Use(Operand::Copy(Place::from(a[1]).field(0, i64t))),
            );
            b.terminate(bb, TerminatorKind::Return);
        });
        let errs = validate(&body, &tys);
        assert!(
            errors_mention(&errs, "but the projection says bool"),
            "{errs:?}"
        );
        assert!(errors_mention(&errs, "without a downcast"), "{errs:?}");
    }

    #[test]
    fn mir_builder_refuses_unterminated_block() {
        let tys = TyInterner::new();
        let unit = tys.unit();
        let mut b = BodyBuilder::new(inst("t"), unit);
        b.new_block();
        assert_eq!(b.finish().unwrap_err(), "bb0 has no terminator");
    }
}
