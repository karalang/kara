//! `filter(..).count()` adds each answer of the predicate rather than
//! branching on it, and `count()` steps without an overflow test (kata
//! 520: a branch per char on `is_ascii_uppercase` mispredicted over
//! mixed-case words, 1.85x Rust's time with the same instructions). The
//! counts must not change: empty input, all kept, none kept, a mix, and a
//! predicate with captures and side effects, which still runs once per item
//! in order.

#[cfg(feature = "llvm")]
mod common;

const SRC: &str = r#"
fn main() {
    let words = ["", "ABC", "abc", "Abc", "aBcD"];
    for w in words {
        let caps = w.chars().filter(|c| c.is_ascii_uppercase()).count();
        print(f"{caps} ");
    }
    let v: Vec[i64] = [5, 1, 4, 1, 5, 9, 2, 6];
    let limit = 4;
    let mut seen: Vec[i64] = Vec.new();
    let big = v.iter().filter(|x| { seen.push(*x); *x > limit }).count();
    println(f"| {big} {seen.len()} {seen[7]} {v.iter().count()} {(0..0).count()}");
}
"#;

const EXPECTED: &str = "0 3 0 1 2 | 4 8 6 8 0\n";

#[test]
fn mir_filter_count_counts_each_kept_item() {
    let r = karac::mir::lower::run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.output, EXPECTED, "stderr: {}", r.stderr);
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_filter_count_counts_each_kept_item() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-filter-count-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
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
        assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
