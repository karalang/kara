//! B-2026-10-19-52: a `let ... else` initializer's temporaries drop
//! before the `else` block runs (core semantics §7.4), and at the `;`
//! when the pattern matches, on both MIR backends.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
fn make(id: i64) -> R { R { id: id } }
fn find(r: R) -> Option[i64] { if r.id > 5 { Some(r.id) } else { None } }
fn f(id: i64) {
    let Some(v) = find(make(id)) else {
        println("else");
        return;
    };
    println(f"got{v}");
}
fn main() {
    f(1);
    f(9);
}
"#;

const WANT: &str = "d1\nelse\nd9\ngot9\n";

#[test]
fn let_else_temporaries_drop_before_the_else_block() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-mir-let-else-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8(out.stdout).unwrap(), WANT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
