//! An associated function called on a type written with its arguments
//! (`Vec[String].from_iter(it)`, design.md §8) whose own type parameters
//! the arguments decide (B-2026-10-29-40): the check solved only the
//! impl's, so the build found the method's `I` undecided. Outputs are
//! worked out by hand.
//!
//! With `--features llvm` each program is also compiled, at -O0 and -O2,
//! and must print the same.

#[cfg(feature = "llvm")]
mod common;

use karac::mir::interp::Outcome;

/// The interpreter prints `want` and finishes with nothing leaked, and so
/// does the program compiled.
fn runs(src: &str, want: &str) {
    let run = karac::mir::lower::run_source(src).expect("MIR");
    assert!(
        matches!(run.outcome, Outcome::Returned(_)),
        "{:?}\n{}",
        run.outcome,
        run.output
    );
    assert_eq!(run.output, want);
    #[cfg(feature = "llvm")]
    for opt in [0, 2] {
        if let Some((ok, out)) = compiled::build_and_run(src, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[cfg(feature = "llvm")]
mod compiled {
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What the program prints compiled at `opt`, and whether it exited
    /// cleanly; `None` when there is no runtime archive to link.
    pub fn build_and_run(src: &str, opt: u8) -> Option<(bool, String)> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let lowered = karac::mir::lower::build_source(src).expect("MIR");
        karac::mir::lower::compiled_check(&lowered).expect("checks");
        let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("karac-qualified-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let (obj, exe) = (dir.join("main.o"), dir.join("main"));
        karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
        let linked = super::common::link_or_skip(karac::backend::driver::link_executable(
            &obj.to_string_lossy(),
            &exe.to_string_lossy(),
        ));
        let out = linked.map(|()| Command::new(&exe).output().expect("run"));
        let _ = std::fs::remove_dir_all(&dir);
        let out = out?;
        Some((
            out.status.success(),
            String::from_utf8(out.stdout).expect("utf-8"),
        ))
    }
}

/// `Vec[String].from_iter`, `Set[i64].from_iter` and a program's own
/// `Bag[i64].of[I: Iterator[Item = T]]` take their iterator's type from it.
#[test]
fn qualified_call_solves_its_own_parameters() {
    runs(
        r#"struct Bag[T] {
    items: Vec[T],
}
impl[T] Bag[T] {
    fn of[I: Iterator[Item = T]](it: own I) -> Bag[T] {
        let mut items: Vec[T] = Vec.new();
        for x in it {
            items.push(x);
        }
        Bag { items: items }
    }
}
fn main() {
    let lab = [1, 2, 3].to_vec();
    let v = Vec[String].from_iter(lab.iter().map(|x| x.to_string()));
    println(v.join("-"));
    let s = Set[i64].from_iter(lab.iter().map(|x| x * 2));
    println(s.len());
    let b = Bag[i64].of(lab.iter().map(|x| x + 10));
    println(b.items.len());
    println(b.items[2]);
}
"#,
        "1-2-3\n3\n3\n13\n",
    );
}

/// The method's bound still holds against the pinned argument.
#[test]
fn qualified_call_checks_its_bound_against_the_pinned_argument() {
    let src = "fn main() {\n    let lab = [1, 2, 3].to_vec();\n    let v = Vec[String].from_iter(lab.iter().map(|x| x * 2));\n    println(v.len());\n}\n";
    let err = match karac::mir::lower::build_source(src) {
        Ok(_) => panic!("built"),
        Err(e) => e,
    };
    assert!(err.contains("`I.Item = String` is not satisfied"), "{err}");
}
