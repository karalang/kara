//! A closure a function returns may hold a view of what its argument
//! views (core semantics §5.4). Passed on to be stored where no borrow is
//! kept, in an `escaping` parameter's struct, a `shared` value or a `dyn`,
//! it must be refused as a closure written at the call is (§5.7, §9.3):
//! each program below was accepted and then called the closure after what
//! it viewed was freed (B-2026-11-17-92). A function whose returned
//! closures capture no view, or capture a copy, is unaffected, and so is a
//! returned closure used while what it views is live. A closure written
//! at the call and stored in a `shared` value or a `dyn` the call returns
//! was missed the same way.

use std::process::Command;

/// `karac check` on `src`: its success and its output.
fn check(name: &str, src: &str) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-view-factory-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), text)
}

const D: &str = "struct D { n: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"drop {self.n}\"); } }\n";

/// Name and program after [`D`]; each is refused with E0509.
const REFUSED: &[(&str, &str)] = &[
    (
        "factory.escaping_param",
        r#"struct H { f: Fn() -> i64 }
fn mk(v: Slice[D]) -> Fn() -> i64 { || v[0].n }
fn hold(f: escaping Fn() -> i64) -> H { H { f: f } }
fn main() { let h = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; hold(mk(x[0..2])) }; println("after"); println(f"{(h.f)()}"); }
"#,
    ),
    (
        "factory.shared",
        r#"shared struct H { f: Fn() -> i64 }
fn mk(v: Slice[D]) -> Fn() -> i64 { || v[0].n }
fn wrap(f: escaping Fn() -> i64) -> H { H { f: f } }
fn main() { let h = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; wrap(mk(x[0..2])) }; println("after"); println(f"{(h.f)()}"); }
"#,
    ),
    (
        "factory.dyn",
        r#"trait G { fn get(ref self) -> i64; }
struct W { f: Fn() -> i64 }
impl G for W { fn get(ref self) -> i64 { (self.f)() } }
fn mk(v: Slice[D]) -> Fn() -> i64 { || v[0].n }
fn boxit(f: escaping Fn() -> i64) -> dyn G { W { f: f } }
fn main() { let h = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; boxit(mk(x[0..2])) }; println("after"); println(f"{h.get()}"); }
"#,
    ),
    (
        "factory.through_factory",
        r#"struct H { f: Fn() -> i64 }
fn mk(v: Slice[D]) -> Fn() -> i64 { || v[0].n }
fn twice(v: Slice[D]) -> Fn() -> i64 { mk(v) }
fn hold(f: escaping Fn() -> i64) -> H { H { f: f } }
fn main() { let h = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; hold(twice(x[0..2])) }; println("after"); println(f"{(h.f)()}"); }
"#,
    ),
    (
        "literal.shared",
        r#"shared struct H { f: Fn() -> i64 }
fn wrap(f: escaping Fn() -> i64) -> H { H { f: f } }
fn main() { let h = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; let v = x[0..2]; wrap(|| v[0].n) }; println("after"); println(f"{(h.f)()}"); }
"#,
    ),
    (
        "literal.dyn",
        r#"trait G { fn get(ref self) -> i64; }
struct W { f: Fn() -> i64 }
impl G for W { fn get(ref self) -> i64 { (self.f)() } }
fn boxit(f: escaping Fn() -> i64) -> dyn G { W { f: f } }
fn main() { let h = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; let v = x[0..2]; boxit(|| v[0].n) }; println("after"); println(f"{h.get()}"); }
"#,
    ),
];

/// Name and program after [`D`]; each passes the check.
const ACCEPTED: &[(&str, &str)] = &[
    (
        "op.captures_nothing",
        r#"struct H { f: Fn() -> i64 }
fn op(name: ref String) -> Fn() -> i64 { if name == "a" { || 1 } else { || 2 } }
fn hold(f: escaping Fn() -> i64) -> H { H { f: f } }
fn main() { let h = { let s = "a".to_string(); hold(op(s)) }; println(f"{(h.f)()}"); }
"#,
    ),
    (
        "factory.captures_a_copy",
        r#"struct H { f: Fn() -> i64 }
fn mk(v: Slice[D]) -> Fn() -> i64 { let n = v[0].n; || n }
fn hold(f: escaping Fn() -> i64) -> H { H { f: f } }
fn main() { let h = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; hold(mk(x[0..2])) }; println("after"); println(f"{(h.f)()}"); }
"#,
    ),
    (
        "factory.used_in_scope",
        r#"fn mk(v: Slice[D]) -> Fn() -> i64 { || v[0].n }
fn main() { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; let f = mk(x[0..2]); println(f"{f()}"); }
"#,
    ),
];

#[test]
fn view_factory_results_stored_unkept_are_refused() {
    let mut bad = Vec::new();
    for (name, prog) in REFUSED {
        let (ok, text) = check(name, &format!("{D}{prog}"));
        if ok || !text.contains("E0509") {
            bad.push(format!("{name}: accepted or wrong code:\n{text}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

#[test]
fn view_factory_controls_are_accepted() {
    let mut bad = Vec::new();
    for (name, prog) in ACCEPTED {
        let (ok, text) = check(name, &format!("{D}{prog}"));
        if !ok {
            bad.push(format!("{name}: refused:\n{text}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}
