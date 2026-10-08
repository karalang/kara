//! MIR: the control-flow-graph IR between typed HIR and LLVM
//! (redesign M1; design in `docs/spikes/mir-types.md`).
//!
//! This module holds the data types, a block-by-block builder, place
//! typing, the textual form and the structural validator. The HIR-to-MIR
//! lowering, drop elaboration, the borrow checker and the MIR interpreter
//! build on it. `ty` is a placeholder interner until the typed HIR's
//! interned types land; MIR only needs the queries it exposes.

pub mod borrowck;
pub mod build;
pub mod effects;
pub mod elaborate;
pub mod flags;
pub mod interp;
pub mod lower;
pub mod movecheck;
pub mod parse;
pub mod place_ty;
pub mod pretty;
pub mod syntax;
pub mod ty;
pub mod validate;

pub use borrowck::check_borrows;
pub use build::BodyBuilder;
pub use elaborate::elaborate_drops;
pub use flags::insert_borrow_flags;
pub use movecheck::check_moves;
pub use parse::{parse_module, pretty_module, MirModule};
pub use pretty::pretty_body;
pub use syntax::*;
pub use ty::{AdtDef, AdtId, FnKind, IntTy, Ty, TyInterner, TyKind, VariantDef};
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
                unwind: UnwindAction::Abort,
            },
        );
        b.terminate(
            bb2,
            TerminatorKind::Drop {
                place: r.into(),
                target: bb3,
                unwind: UnwindAction::Abort,
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

    /// `u8 as char`, `char as` an integer and `bool as` an integer: the
    /// validator checks each kind against its operand and target, and the
    /// interpreter runs them.
    #[test]
    fn mir_char_and_bool_casts() {
        let src = |body: &str| {
            format!(
                "
fn main() -> () {{
    let mut _0: ();
    let _1: char;
    let _2: u32;
    let _3: i64;
    let _4: u8;
    let _5: ();
    bb0: {{
{body}        _5 = println(copy _1, copy _2, copy _3, copy _4) -> bb1;
    }}
    bb1: {{
        _0 = const ();
        return;
    }}
}}
"
            )
        };
        let m = parse_module(&src("        _1 = const 97_u8 as char (IntToChar);
        _2 = copy _1 as u32 (CharToInt);
        _3 = const true as i64 (BoolToInt);
        _4 = const 'z' as u8 (CharToInt);
"))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(validate(&m.bodies[0], &m.tys), Vec::<String>::new());
        let prog = interp::Program::from_module(&m);
        let r = interp::run(&prog, &m.tys, "main", vec![]);
        // `println` writes its arguments with no separator.
        assert_eq!(r.output, "a971122\n", "{:?}", r.outcome);

        let bad = parse_module(&src("        _1 = const 97_u32 as char (IntToChar);
        _2 = const 'a' as u32 (BoolToInt);
        _3 = const 1_i64;
        _4 = const 1_u8;
"))
        .unwrap_or_else(|e| panic!("{e}"));
        let errs = validate(&bad.bodies[0], &bad.tys);
        assert!(
            errors_mention(&errs, "IntToChar cast from u32 to char"),
            "{errs:?}"
        );
        assert!(
            errors_mention(&errs, "BoolToInt cast from char to u32"),
            "{errs:?}"
        );
    }

    /// `par` region lines read and print back, and the validator checks
    /// their blocks exist.
    #[test]
    fn mir_par_regions_round_trip() {
        let src = |regions: &str| {
            format!(
                "
fn main() -> () {{
    let mut _0: ();
    let _1: i64;
{regions}    bb0: {{
        _1 = const 2_i64;
        goto -> bb1;
    }}
    bb1: {{
        goto -> bb2;
    }}
    bb2: {{
        goto -> bb3;
    }}
    bb3: {{
        _0 = const ();
        return;
    }}
}}
"
            )
        };
        let ok = src("    par#0 block [bb1] [bb2]\n    par#1 for [bb2 bb3] limit copy _1\n    par#2 for [bb3]\n");
        let m = parse_module(&ok).unwrap_or_else(|e| panic!("{e}"));
        let body = &m.bodies[0];
        assert_eq!(validate(body, &m.tys), Vec::<String>::new());
        assert_eq!(body.par_regions.len(), 3);
        assert_eq!(body.par_regions[0].kind, ParKind::Block);
        assert_eq!(body.par_regions[0].branches.len(), 2);
        assert!(matches!(
            body.par_regions[1].kind,
            ParKind::For { limit: Some(_) }
        ));
        assert_eq!(body.par_regions[2].kind, ParKind::For { limit: None });
        let printed = pretty_body(body, &m.tys);
        for line in [
            "par#0 block [bb1] [bb2]",
            "par#1 for [bb2 bb3] limit copy _1",
            "par#2 for [bb3]",
        ] {
            assert!(printed.contains(line), "{printed}");
        }
        let m2 = parse_module(&pretty_module(&m)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(pretty_body(&m2.bodies[0], &m2.tys), printed);

        let bad = parse_module(&src("    par#0 block [bb9]\n    par#1 for\n"))
            .unwrap_or_else(|e| panic!("{e}"));
        let errs = validate(&bad.bodies[0], &bad.tys);
        // An empty `par {}` has a region with no branches, which is valid.
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errors_mention(&errs, "bb9 does not exist"), "{errs:?}");
        let skipped = parse_module(&src("    par#1 block [bb1]\n"));
        assert!(skipped.is_err());
    }

    /// Erased function values: the `Erase` cast, calls through `ref`,
    /// `mut ref` and by value, and what the validator, the borrow check
    /// and drop elaboration make of them.
    #[test]
    fn mir_erased_fn_values() {
        let src = |locals: &str, body: &str| {
            format!(
                "
fn main() -> () {{
    let mut _0: ();
    let _1: i64;
    let _2: ref i64;
    let _3: closure#1(ref i64);
    let _4: Fn(i64) -> i64;
    let _5: ref Fn(i64) -> i64;
    let _6: i64;
{locals}    bb0: {{
        _1 = const 1_i64;
        _2 = &_1;
        _3 = closure#1(ref i64) [copy _2];
        _4 = move _3 as Fn(i64) -> i64 (Erase);
{body}    }}
    bb1: {{
        _0 = const ();
        drop(_4) -> bb2;
    }}
    bb2: {{
        return;
    }}
}}
"
            )
        };
        let ok = src(
            "",
            "        _5 = &_4;\n        _6 = copy _5(const 2_i64) -> bb1;\n",
        );
        let m = parse_module(&ok).unwrap_or_else(|e| panic!("{e}"));
        let body = &m.bodies[0];
        assert_eq!(validate(body, &m.tys), Vec::<String>::new());
        let printed = pretty_body(body, &m.tys);
        assert!(
            printed.contains("_4 = move _3 as Fn(i64) -> i64 (Erase);"),
            "{printed}"
        );
        assert!(printed.contains("let _5: ref Fn(i64) -> i64;"), "{printed}");
        let mut m2 = parse_module(&pretty_module(&m)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(pretty_body(&m2.bodies[0], &m2.tys), printed);
        // The erased value is move-only and owns its captures: its drop is
        // kept, as a whole.
        assert!(!m.tys.is_copy(body.locals[4].ty));
        assert!(m.tys.needs_drop(body.locals[4].ty));
        let b = &mut m2.bodies[0];
        elaborate_drops(b, &mut m2.tys).unwrap();
        assert_eq!(validate(b, &m2.tys), Vec::<String>::new());
        assert!(pretty_body(b, &m2.tys).contains("drop(_4)"));

        // It holds the closure's borrow of `_1`.
        let tail = ok.replace(
            "        _0 = const ();\n        drop(_4)",
            "        _1 = const 3_i64;\n        _0 = const ();\n        drop(_4)",
        );
        let m = parse_module(&tail).unwrap_or_else(|e| panic!("{e}"));
        let errs = check_borrows(&m.bodies[0], &m.tys, &|_| false).unwrap_err();
        assert!(
            errors_mention(&errs, "write of _1 while it is borrowed"),
            "{errs:?}"
        );

        // A `MutFn` cannot be called through a `ref`; an `OnceFn` is called
        // by moving it, and a kind widens but never narrows.
        let locals = "    let _7: MutFn(i64) -> i64;\n    let _8: ref MutFn(i64) -> i64;\n    let _9: OnceFn(i64) -> i64;\n    let _10: Fn(i64) -> i64;\n";
        let mut errs = Vec::new();
        for body in [
            "        _7 = move _4 as MutFn(i64) -> i64 (Erase);\n        _8 = &_7;\n        _6 = copy _8(const 2_i64) -> bb1;\n",
            "        _9 = move _4 as OnceFn(i64) -> i64 (Erase);\n        _10 = move _9 as Fn(i64) -> i64 (Erase);\n        _6 = copy _10(const 2_i64, const 3_i64) -> bb1;\n",
        ] {
            let m = parse_module(&src(locals, body)).unwrap_or_else(|e| panic!("{e}"));
            errs.extend(validate(&m.bodies[0], &m.tys));
        }
        for want in [
            "a MutFn value is called through a `ref`",
            "Erase cast from OnceFn(i64) -> i64 to Fn(i64) -> i64",
            "a call by value through an `OnceFn` must move it",
            "a Fn(i64) -> i64 takes 1 arguments, given 2",
        ] {
            assert!(errors_mention(&errs, want), "{want}: {errs:?}");
        }
    }

    /// `weak` handles: the `Downgrade` and `Upgrade` casts, what the
    /// validator accepts, and that a weak value is move-only, needs drop
    /// and is dropped whole.
    #[test]
    fn mir_weak_handles() {
        let src = |body: &str| {
            format!(
                "
struct Node {{ id: i64 }}
enum Option[shared Node] {{ None, Some(shared Node) }}

fn main(_1: shared Node) -> () {{
    let mut _0: ();
    let _2: ref shared Node;
    let _3: weak Node;
    let _4: ref weak Node;
    let _5: weak Node;
    let _6: Option[shared Node];
    let _7: i64;
    let _8: ref Option[shared Node];
    bb0: {{
        _2 = &_1;
        _3 = copy _2 as weak Node (Downgrade);
        _4 = &_3;
{body}        _0 = const ();
        drop(_6) -> bb1;
    }}
    bb1: {{
        drop(_5) -> bb2;
    }}
    bb2: {{
        drop(_3) -> bb3;
    }}
    bb3: {{
        drop(_1) -> bb4;
    }}
    bb4: {{
        return;
    }}
}}
"
            )
        };
        let ok = src(
            "        _5 = copy _4 as weak Node (Downgrade);\n        _6 = copy _4 as Option[shared Node] (Upgrade);\n",
        );
        // Downgrade of an `Option[shared T]`: `None` gives an empty weak,
        // which is how `parent: None` is written into a weak slot.
        let from_option = src(
            "        _6 = copy _4 as Option[shared Node] (Upgrade);\n        _8 = &_6;\n        _5 = copy _8 as weak Node (Downgrade);\n",
        );
        let m = parse_module(&from_option).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(validate(&m.bodies[0], &m.tys), Vec::<String>::new());
        let m = parse_module(&ok).unwrap_or_else(|e| panic!("{e}"));
        let body = &m.bodies[0];
        assert_eq!(validate(body, &m.tys), Vec::<String>::new());
        let printed = pretty_body(body, &m.tys);
        for line in [
            "let _3: weak Node;",
            "_3 = copy _2 as weak Node (Downgrade);",
            "_6 = copy _4 as Option[shared Node] (Upgrade);",
        ] {
            assert!(printed.contains(line), "{line}: {printed}");
        }
        let mut m2 = parse_module(&pretty_module(&m)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(pretty_body(&m2.bodies[0], &m2.tys), printed);
        let w = body.locals[3].ty;
        assert!(!m.tys.is_copy(w));
        assert!(m.tys.needs_drop(w));
        let b = &mut m2.bodies[0];
        elaborate_drops(b, &mut m2.tys).unwrap();
        assert_eq!(validate(b, &m2.tys), Vec::<String>::new());
        let elaborated = pretty_body(b, &m2.tys);
        assert!(elaborated.contains("drop(_3)"), "{elaborated}");
        assert!(elaborated.contains("drop(_5)"), "{elaborated}");
        // A weak value holds no borrow: writing the handle it came from
        // while it lives is fine.
        assert!(check_borrows(&m.bodies[0], &m.tys, &|_| false).is_ok());

        let mut errs = Vec::new();
        for body in [
            // Upgrade reads through a `ref weak`, not a `ref shared`.
            "        _5 = copy _4 as weak Node (Downgrade);\n        _6 = copy _2 as Option[shared Node] (Upgrade);\n",
            // Downgrade takes a reference, and yields a weak value.
            "        _5 = move _3 as weak Node (Downgrade);\n        _6 = copy _4 as Option[shared Node] (Upgrade);\n",
            "        _5 = copy _4 as weak Node (Downgrade);\n        _7 = copy _4 as i64 (Upgrade);\n        _6 = copy _4 as Option[shared Node] (Upgrade);\n",
            // Downgrade of an Option yields a weak value, not an Option.
            "        _5 = copy _4 as weak Node (Downgrade);\n        _6 = copy _4 as Option[shared Node] (Upgrade);\n        _8 = &_6;\n        _6 = copy _8 as Option[shared Node] (Downgrade);\n",
        ] {
            let m = parse_module(&src(body)).unwrap_or_else(|e| panic!("{e}"));
            errs.extend(validate(&m.bodies[0], &m.tys));
        }
        for want in [
            "Upgrade cast from ref shared Node to Option[shared Node]",
            "Downgrade cast from weak Node to weak Node",
            "Upgrade cast from ref weak Node to i64",
            "Downgrade cast from ref Option[shared Node] to Option[shared Node]",
        ] {
            assert!(errors_mention(&errs, want), "{want}: {errs:?}");
        }
    }

    /// The interpreter's weak count: `Upgrade` yields `Some` while a
    /// strong handle lives and `None` after, the value is dropped at strong
    /// count zero, and its slot is freed when the last weak handle goes.
    #[test]
    fn mir_interp_reuses_freed_slots() {
        // The second allocation reuses the slot the first one freed, so the
        // interpreter's memory follows live allocations.
        let text = "
struct Node { id: i64 }

fn main() -> () {
    let mut _0: ();
    let _1: shared Node;
    let _2: shared Node;
    bb0: {
        _1 = shared Node { const 1_i64 };
        drop(_1) -> bb1;
    }
    bb1: {
        _2 = shared Node { const 2_i64 };
        drop(_2) -> bb2;
    }
    bb2: {
        _0 = const ();
        return;
    }
}
";
        let m = parse_module(text).unwrap_or_else(|e| panic!("{e}"));
        let prog = interp::Program::from_module(&m);
        let r = interp::run(&prog, &m.tys, "main", vec![]);
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        let trace = r.trace();
        let allocs: Vec<&str> = trace.lines().filter(|l| l.starts_with("alloc")).collect();
        let frees: Vec<&str> = trace.lines().filter(|l| l.starts_with("free")).collect();
        assert_eq!(allocs.len(), 2, "{trace}");
        assert!(allocs.iter().all(|l| l.starts_with("alloc a0")), "{trace}");
        assert_eq!(frees, ["free a0", "free a0"], "{trace}");
    }

    #[test]
    fn mir_interp_weak_counts() {
        let text = "
struct Node { id: i64 }
enum Option[shared Node] { None, Some(shared Node) }

fn main() -> () {
    let mut _0: ();
    let _1: shared Node;
    let _2: ref shared Node;
    let _3: weak Node;
    let _4: ref weak Node;
    let _5: Option[shared Node];
    let _6: Option[shared Node];
    let _7: isize;
    let _8: isize;
    let _9: ();
    bb0: {
        _1 = shared Node { const 1_i64 };
        _2 = &_1;
        _3 = copy _2 as weak Node (Downgrade);
        _4 = &_3;
        _5 = copy _4 as Option[shared Node] (Upgrade);
        _7 = discriminant(_5);
        _9 = println(copy _7) -> bb1;
    }
    bb1: {
        drop(_5) -> bb2;
    }
    bb2: {
        drop(_1) -> bb3;
    }
    bb3: {
        _6 = copy _4 as Option[shared Node] (Upgrade);
        _8 = discriminant(_6);
        _9 = println(copy _8) -> bb4;
    }
    bb4: {
        drop(_6) -> bb5;
    }
    bb5: {
        drop(_3) -> bb6;
    }
    bb6: {
        _0 = const ();
        return;
    }
}
";
        let m = parse_module(text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(validate(&m.bodies[0], &m.tys), Vec::<String>::new());
        let prog = interp::Program::from_module(&m);
        let r = interp::run(&prog, &m.tys, "main", vec![]);
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        // `Some` is variant 1, `None` variant 0.
        assert_eq!(r.output, "1\n0\n");
        let trace = r.trace();
        let frees: Vec<&str> = trace.lines().filter(|l| l.starts_with("free")).collect();
        assert_eq!(frees, ["free a0"], "{trace}");
        // The slot outlives the strong handles: it is freed after `_3`'s drop.
        let free_at = trace.find("free a0").unwrap();
        let last_release = trace.rfind("release a0 0").unwrap();
        assert!(free_at > last_release, "{trace}");
    }

    /// `f16`, `bf16`, `i128` and `isize` are MIR types, and a cast into
    /// a narrow float rounds to its precision.
    #[test]
    fn mir_narrow_floats_and_wide_ints() {
        let text = "
fn main() -> () {
    let mut _0: ();
    let _1: bf16;
    let _2: f16;
    let _3: f64;
    let _4: i128;
    let _5: isize;
    let _6: ();
    bb0: {
        _1 = const 257_i64 as bf16 (IntToFloat);
        _2 = const 2049_i64 as f16 (IntToFloat);
        _3 = copy _2 as f64 (FloatToFloat);
        _4 = const 9223372036854775807_i64 as i128 (IntToInt);
        _5 = copy _4 as isize (IntToInt);
        _6 = println(copy _1, copy _3, copy _5) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}
";
        let m = parse_module(text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(validate(&m.bodies[0], &m.tys), Vec::<String>::new());
        let printed = pretty_body(&m.bodies[0], &m.tys);
        for line in [
            "let _1: bf16;",
            "let _2: f16;",
            "let _4: i128;",
            "let _5: isize;",
        ] {
            assert!(printed.contains(line), "{line}: {printed}");
        }
        let prog = interp::Program::from_module(&m);
        let r = interp::run(&prog, &m.tys, "main", vec![]);
        // 257 has no bf16 (8 significant bits): it rounds to even, 256;
        // 2049 has no f16 (11 bits) and rounds to 2048.
        assert_eq!(r.output, "25620489223372036854775807\n", "{:?}", r.outcome);
        assert_eq!(ty::FloatTy::BF16.round(1.1), 1.1015625);
        assert_eq!(ty::FloatTy::F16.round(65520.0), f64::INFINITY);
    }
}
