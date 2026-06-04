//! Phase 6 line 170 slices 1+2 — integration tests for the
//! `is_cross_task_safe` walker.
//!
//! Drives the walker against types extracted from the real
//! `karac::typecheck` pipeline. Covers the direct-hit unsafe cases
//! (Rc, raw pointers, shared struct/enum, OnceCell) plus the
//! transitive cases (Vec[Rc[T]], struct fields, enum variant
//! payloads, Arc[Rc[T]] still bad).
//!
//! Slice 3 of the same entry — boundary-site enforcement at
//! `spawn`/`par {}`/`TaskGroup.spawn`/`Channel.send`/`with_provider` —
//! lands separately and reuses this walker.

use karac::cross_task_safe::{is_cross_task_safe, CrossTaskUnsafeFixIt};
use karac::typechecker::types::{IntSize, Type};
use karac::typechecker::TypeCheckResult;
use karac::{parse, resolve, typecheck};

/// Helper: parse + resolve + typecheck a snippet, return the result so
/// individual tests can probe `struct_info` / `enum_info`.
fn typecheck_snippet(src: &str) -> TypeCheckResult {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "type errors: {:?}", typed.errors);
    typed
}

// ── Safe-leaf cases ─────────────────────────────────────────

#[test]
fn primitives_are_safe() {
    let types = typecheck_snippet("fn nop() {}");
    for ty in [
        Type::Int(IntSize::I64),
        Type::Bool,
        Type::Char,
        Type::Str,
        Type::Unit,
        Type::Never,
    ] {
        assert!(
            is_cross_task_safe(&ty, &types).is_ok(),
            "primitive {:?} should be cross-task-safe",
            ty
        );
    }
}

#[test]
fn vec_of_primitive_is_safe() {
    let types = typecheck_snippet("fn nop() {}");
    let ty = Type::Named {
        name: "Vec".to_string(),
        args: vec![Type::Int(IntSize::I64)],
    };
    assert!(is_cross_task_safe(&ty, &types).is_ok());
}

#[test]
fn arc_of_primitive_is_safe() {
    let types = typecheck_snippet("fn nop() {}");
    let ty = Type::Arc(Box::new(Type::Int(IntSize::I64)));
    assert!(is_cross_task_safe(&ty, &types).is_ok());
}

#[test]
fn unresolved_type_param_is_conservatively_safe() {
    let types = typecheck_snippet("fn nop() {}");
    let ty = Type::TypeParam("T".to_string());
    assert!(is_cross_task_safe(&ty, &types).is_ok());
}

// ── Direct-hit unsafe cases ────────────────────────────────

#[test]
fn rc_at_root_is_unsafe_with_rc_to_arc_fix() {
    let types = typecheck_snippet("fn nop() {}");
    let ty = Type::Rc(Box::new(Type::Int(IntSize::I64)));
    let err = is_cross_task_safe(&ty, &types).unwrap_err();
    assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::RcToArc);
    assert!(err.unsafe_leaf.starts_with("Rc["));
    assert!(
        err.path.is_empty(),
        "root-level unsafe leaf should have empty path; got {:?}",
        err.path
    );
}

#[test]
fn raw_pointer_is_unsafe_for_both_directions() {
    let types = typecheck_snippet("fn nop() {}");
    for is_mut in [false, true] {
        let ty = Type::Pointer {
            is_mut,
            inner: Box::new(Type::Int(IntSize::I64)),
        };
        let err = is_cross_task_safe(&ty, &types).unwrap_err();
        assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::RawPointer);
    }
}

#[test]
fn once_cell_named_form_is_unsafe() {
    let types = typecheck_snippet("fn nop() {}");
    let ty = Type::Named {
        name: "OnceCell".to_string(),
        args: vec![Type::Int(IntSize::I64)],
    };
    let err = is_cross_task_safe(&ty, &types).unwrap_err();
    assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::OnceCellToLock);
}

#[test]
fn shared_struct_via_type_shared_is_unsafe() {
    let types = typecheck_snippet("fn nop() {}");
    // Bypass the typechecker — directly construct a Type::Shared. The
    // walker's contract is type-driven, so it doesn't matter whether
    // the typechecker actually produced a Shared at this site.
    let ty = Type::Shared("Hub".to_string());
    let err = is_cross_task_safe(&ty, &types).unwrap_err();
    assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::SharedToPar);
}

#[test]
fn par_struct_via_type_shared_is_safe() {
    // Phase 6 `par struct` slice B: `par struct` / `par enum` values lower to
    // `Type::Shared` too, but a `par` type is cross-task-safe by definition.
    // The walker distinguishes via `StructInfo.is_par`, so it needs the real
    // typechecked struct_info (not a bare `fn nop` snippet).
    let types = typecheck_snippet("par struct Hub { value: i64 }");
    let ty = Type::Shared("Hub".to_string());
    assert!(
        is_cross_task_safe(&ty, &types).is_ok(),
        "par struct must be cross-task-safe by definition; got {:?}",
        is_cross_task_safe(&ty, &types)
    );
}

#[test]
fn par_enum_via_type_shared_is_safe() {
    let types = typecheck_snippet("par enum Hub { A, B }");
    let ty = Type::Shared("Hub".to_string());
    assert!(
        is_cross_task_safe(&ty, &types).is_ok(),
        "par enum must be cross-task-safe by definition; got {:?}",
        is_cross_task_safe(&ty, &types)
    );
}

// ── Transitive cases ───────────────────────────────────────

#[test]
fn vec_of_rc_carries_path_through_arg() {
    let types = typecheck_snippet("fn nop() {}");
    let ty = Type::Named {
        name: "Vec".to_string(),
        args: vec![Type::Rc(Box::new(Type::Int(IntSize::I64)))],
    };
    let err = is_cross_task_safe(&ty, &types).unwrap_err();
    assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::RcToArc);
    assert_eq!(err.path, vec!["`Vec` arg 0".to_string()]);
}

#[test]
fn arc_of_rc_still_unsafe_via_inner() {
    let types = typecheck_snippet("fn nop() {}");
    let ty = Type::Arc(Box::new(Type::Rc(Box::new(Type::Int(IntSize::I64)))));
    let err = is_cross_task_safe(&ty, &types).unwrap_err();
    assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::RcToArc);
    assert_eq!(err.path, vec!["Arc inner".to_string()]);
}

// Note: `Rc[T]` is not a directly-nameable user type at v1 — the
// typechecker emits `Type::Rc` internally via ownership analysis on
// `shared struct` values, not via explicit user annotation. A test
// that constructs a struct with a literal `Rc[T]` field is therefore
// not runnable through the parser surface. The walker's transitive
// handling of struct fields is exercised in slice 3's boundary-site
// integration tests instead, where real spawn-site capture types reach
// the walker after typechecker-internal Type::Rc resolution.

#[test]
fn user_shared_struct_via_struct_info_is_unsafe() {
    let src = r#"
        shared struct Hub {
            id: i64,
        }
    "#;
    let types = typecheck_snippet(src);
    let ty = Type::Named {
        name: "Hub".to_string(),
        args: vec![],
    };
    let err = is_cross_task_safe(&ty, &types).unwrap_err();
    assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::SharedToPar);
    assert!(err.unsafe_leaf.contains("shared"));
}

// Same reasoning as the `Rc[T]` field test above: `Rc[T]` is not
// directly-nameable in user kara source at v1. Slice 3's integration
// tests will exercise enum-variant transitivity with real spawn-site
// types.

#[test]
fn user_shared_enum_via_enum_info_is_unsafe() {
    let src = r#"
        shared enum Mood {
            Happy,
            Sad,
        }
    "#;
    let types = typecheck_snippet(src);
    let ty = Type::Named {
        name: "Mood".to_string(),
        args: vec![],
    };
    let err = is_cross_task_safe(&ty, &types).unwrap_err();
    assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::SharedToPar);
}

// ── Reference / borrow transparency ────────────────────────

#[test]
fn ref_to_unsafe_inner_is_unsafe() {
    let types = typecheck_snippet("fn nop() {}");
    let ty = Type::Ref(Box::new(Type::Rc(Box::new(Type::Str))));
    let err = is_cross_task_safe(&ty, &types).unwrap_err();
    assert_eq!(err.fix_it, CrossTaskUnsafeFixIt::RcToArc);
}

// ── Fix-it help text ───────────────────────────────────────

#[test]
fn fix_it_help_text_renders_per_variant() {
    assert!(CrossTaskUnsafeFixIt::RcToArc
        .help_text("Rc[Cache]")
        .contains("Arc"));
    assert!(CrossTaskUnsafeFixIt::SharedToPar
        .help_text("Hub")
        .contains("par"));
    assert!(CrossTaskUnsafeFixIt::OnceCellToLock
        .help_text("OnceCell[i64]")
        .contains("OnceLock"));
    assert!(CrossTaskUnsafeFixIt::RawPointer
        .help_text("*const u8")
        .contains("Atomic"));
}
