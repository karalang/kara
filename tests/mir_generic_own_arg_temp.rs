//! B-2026-11-19-127: a call through a generic bound whose trait method
//! takes a parameter `own` moves a named argument even into an impl that
//! borrows it; the moved value is the call's temporary and drops at the
//! end of the statement (core semantics §4.7, §7.4), not at the end of the
//! generic function. A direct call to the borrowing impl still borrows,
//! and the value is usable after it. The receiver's case is
//! `mir_generic_own_self.rs` (B-2026-10-19-51).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
trait Eat { fn eat(own self, x: own R, y: own R) -> i64; }
struct S { k: i64 }
impl Eat for S { fn eat(self, x: R, y: own R) -> i64 { x.id + y.id + self.k } }
fn named[E: Eat](e: own E, r: own R) -> i64 {
    let v = e.eat(r, R { id: 2 });
    println("after");
    v
}
fn local[E: Eat](e: own E) -> i64 {
    let r = R { id: 6 };
    let v = e.eat(r, R { id: 3 });
    println("after local");
    v
}
fn direct() {
    let r = R { id: 7 };
    let n = S { k: 0 }.eat(r, R { id: 4 });
    println(f"direct {n} {r.id}");
}
fn main() {
    println(f"{named(S { k: 1 }, R { id: 5 })}");
    println(f"{local(S { k: 1 })}");
    direct();
}
"#;

const WANT: &str = "d2\nd5\nafter\n8\nd3\nd6\nafter local\n10\nd4\ndirect 11 7\nd7\n";

#[test]
fn a_generic_own_param_call_drops_a_named_argument_at_the_statement_end() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-mir-own-arg-temp-{}", std::process::id()));
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
