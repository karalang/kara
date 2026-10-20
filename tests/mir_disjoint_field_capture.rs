//! B-2026-10-19-48: a closure that uses a struct binding only through its
//! fields captures those fields, not the binding, so the other fields stay
//! usable beside it (core semantics §9.1), on both MIR backends.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn both(name: &str, src: &str, want: &str) {
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "{name}: interpreter");
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir =
        std::env::temp_dir().join(format!("karac-mir-field-cap-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::codegen::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            want,
            "{name}: compiled"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_closure_reading_one_field_leaves_another_writable() {
    both(
        "shared",
        r#"struct P { a: String, b: String }
fn main() {
    let mut p = P { a: "a".to_string(), b: "b".to_string() };
    let c = || p.a.len();
    p.b.push_str("x");
    println(f"{c()} {p.b}");
}
"#,
        "1 bx\n",
    );
}

#[test]
fn a_closure_mutating_one_field_leaves_another_readable() {
    both(
        "mut",
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
struct P { a: Vec[i64], b: R, n: i64 }
fn main() {
    let mut p = P { a: Vec.new(), b: R { id: 7 }, n: 1 };
    let mut push = |x: i64| p.a.push(x + p.n);
    push(1);
    push(2);
    println(f"{p.b.id}");
    println(f"{p.a.len()}");
}
"#,
        "7\n2\nd7\n",
    );
}

#[test]
fn a_closure_using_the_whole_binding_still_borrows_all_of_it() {
    let src = r#"struct P { a: Vec[i64], b: i64 }
fn show(p: ref P) -> i64 { p.b }
fn main() {
    let mut p = P { a: Vec.new(), b: 1 };
    let mut push = |x: i64| { p.a.push(x); show(p) };
    p.b = 3;
    println(f"{push(1)}");
}
"#;
    let err = match karac::mir::lower::build_source(src) {
        Ok(l) => karac::mir::lower::compiled_check(&l).err(),
        Err(e) => Some(e),
    }
    .expect("a whole-binding capture conflicts with the field write");
    assert!(format!("{err:?}").contains("E0516"), "{err:?}");
}
