//! B-2026-10-02-59 / B-2026-09-27-63 -- a `shared enum` is an RC handle to
//! the move checker and to `.clone()`, exactly as a `shared struct` is.

use super::*;

const SHARED_ENUM_HANDLE_SRC: &str = r#"shared struct S { id: i64 }
impl S { fn get(self) -> i64 { return self.id; } }
shared enum Sh { A(i64), B }
impl Sh { fn read(self) -> i64 { match self { Sh.A(r) => { return r; } Sh.B => { return 0; } } } }
shared enum L { Nil, C(i64, L) }
fn sg(s: S) -> i64 { return s.id; }
fn rd(s: Sh) -> i64 { match s { Sh.A(r) => { return r; } Sh.B => { return 0; } } }
fn g(l: L) -> i64 { match l { L.Nil => { return 0; } L.C(v, rest) => { return v + g(rest); } } }
fn mk(n: i64) -> L { if n == 0 { return L.Nil; } return L.C(n, mk(n - 1)); }
fn main() {
    let d = S { id: 3 }; println(f"{sg(d)} {sg(d)}");
    let e: Sh = Sh.A(4); println(f"{e.read()} {e.read()}");
    let f: Sh = Sh.A(5); println(f"{rd(f)} {rd(f)}");
    let h: Sh = Sh.B; let h2 = h; println(f"{rd(h)} {rd(h2)}");
    let a = mk(3);
    println(f"{g(a) + g(a)}");
    let b = a.clone();
    println(f"{g(b)} {g(a.clone())} {g(a)}");
}
"#;

/// B-2026-10-02-59 / B-2026-09-27-63 — the typechecker lowers a shared STRUCT
/// name to `Type::Shared` but leaves a shared ENUM `Type::Named`, so the move
/// checker's copy predicate and `.clone()` resolution, which both recognised
/// `Type::Shared` only, treated the enum as a move type: passing one binding
/// by value twice warned E0500 and `.clone()` was E0236. Both kinds are
/// reference-counted handles (design.md § `shared struct` / `shared enum`).
#[test]
fn test_shared_enum_is_an_rc_handle_to_the_checkers() {
    let parsed = karac::parse(SHARED_ENUM_HANDLE_SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    assert!(resolved.errors.is_empty(), "{:?}", resolved.errors);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let terrs: Vec<String> = typed.errors.iter().map(|e| e.to_string()).collect();
    assert!(terrs.is_empty(), "type errors: {terrs:?}");
    let owned = karac::ownershipcheck(&parsed.program, &typed);
    let oerrs: Vec<String> = owned.errors.iter().map(|e| e.to_string()).collect();
    assert!(oerrs.is_empty(), "ownership diagnostics: {oerrs:?}");
    assert_eq!(
        run(SHARED_ENUM_HANDLE_SRC),
        "3 3\n4 4\n5 5\n0 0\n12\n6 6 6\n"
    );
}

/// B-2026-09-27-63 — the move-checker half alone, with no `.clone()` in the
/// program, so a control without the fix fails on the E0500 warnings rather
/// than on the E0236 type errors the test above stops at first.
#[test]
fn test_shared_enum_reuse_after_by_value_pass_draws_no_move_warning() {
    let src = r#"shared enum Sh { A(i64), B }
impl Sh { fn read(self) -> i64 { match self { Sh.A(r) => { return r; } Sh.B => { return 0; } } } }
fn rd(s: Sh) -> i64 { match s { Sh.A(r) => { return r; } Sh.B => { return 0; } } }
fn main() {
    let e: Sh = Sh.A(4); println(f"{e.read()} {e.read()}");
    let f: Sh = Sh.A(5); println(f"{rd(f)} {rd(f)}");
    let h: Sh = Sh.B; let h2 = h; println(f"{rd(h)} {rd(h2)}");
}
"#;
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty());
    let owned = karac::ownershipcheck(&parsed.program, &typed);
    let oerrs: Vec<String> = owned.errors.iter().map(|e| e.to_string()).collect();
    assert!(oerrs.is_empty(), "ownership diagnostics: {oerrs:?}");
    assert_eq!(run(src), "4 4\n5 5\n0 0\n");
}
