//! B-2026-10-19-51: a call through a generic bound whose trait method
//! takes `own self` moves the receiver even into an impl that borrows it;
//! the moved value is the call's temporary and drops at the end of the
//! statement (core semantics §4.7, §7.4), not at the end of the generic
//! function. A `self` trait method, and a direct call to the borrowing
//! impl, still borrow.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
trait Show { fn show(own self) -> i64; fn peek(self) -> i64; }
impl Show for R { fn show(self) -> i64 { self.id } fn peek(self) -> i64 { self.id + 10 } }
fn via[T: Show](x: own T) -> i64 {
    let p = x.peek();
    println(f"peek {p}");
    let n = x.show() + 1;
    println("got");
    n
}
fn direct() {
    let r = R { id: 7 };
    let n = r.show();
    println(f"direct {n}");
}
fn main() {
    let n = via(R { id: 1 });
    println(f"{n}");
    direct();
}
"#;

const WANT: &str = "peek 11\nd1\ngot\n2\ndirect 7\nd7\n";

#[test]
fn a_generic_own_self_call_drops_its_receiver_at_the_statement_end() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-mir-generic-own-{}", std::process::id()));
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
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8(out.stdout).unwrap(), WANT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
