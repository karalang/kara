//! A table of scalar constants that `mir::literal_tables` moves into a
//! static (`for step in [1, 9]`, `let flags = [true, false, true]`) is a
//! constant global whose buffer is a constant array, instead of a zeroed
//! global its initializer fills at startup, so LLVM sees the elements
//! (kata 752's two-step loop reloaded the static's header and bounds-checked
//! each step, B-2026-11-09-5). Integers, `bool`s, `char`s and floats read
//! back as written, by index, by `len`, through `iter` and a `clone` that
//! is then grown; a table of strings is still built by its initializer.

#[cfg(feature = "llvm")]
mod common;

const SRC: &str = r#"
fn steps(x: i64) -> i64 {
    let mut s = 0;
    for step in [1, 9] {
        s += (x + step) % 10;
    }
    s
}

fn picks(i: i64) -> String {
    let flags = [true, false, true];
    let marks = ['a', 'é', 'z'];
    let scales = [0.5, 2.0, -1.25];
    let small: Vec[i8] = [-3, 7, 127];
    let names = ["one", "two", "three"];
    let k = i % 3;
    f"{flags[k]} {marks[k]} {scales[k]} {small[k]} {names[k]} {flags.len()}"
}

fn copied() -> i64 {
    let table = [4, 5, 6];
    let mut mine = table.clone();
    mine.push(7);
    let mut sum = 0;
    for x in table.iter() {
        sum += *x;
    }
    sum * 100 + mine.len() + mine[3]
}

fn main() {
    let mut total = 0;
    for x in 0..10 {
        total += steps(x);
    }
    println(f"{total}");
    for i in 0..4 {
        println(picks(i));
    }
    println(f"{copied()}");
}
"#;

#[test]
fn mir_constant_tables_read_as_built() {
    let r = karac::mir::lower::run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        r.output, "90\ntrue a 0.5 -3 one 3\nfalse é 2.0 7 two 3\ntrue z -1.25 127 three 3\ntrue a 0.5 -3 one 3\n1511\n",
        "stderr: {}",
        r.stderr
    );
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_constant_tables_are_constant_globals() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    // Each table of scalar constants is a constant global over a constant
    // buffer, with no initializer run at startup; the string table is not.
    for want in [
        "constant [2 x i64] [i64 1, i64 9]",
        "constant [3 x i64] [i64 4, i64 5, i64 6]",
        "constant [3 x double] [double 5.000000e-01, double 2.000000e+00, double -1.250000e+00]",
        "constant [3 x i32] [i32 97, i32 233, i32 122]",
    ] {
        assert!(ir.contains(want), "no `{want}` in:\n{ir}");
    }
    let constant = ir
        .lines()
        .filter(|l| {
            l.starts_with("@kara.static.table") && l.contains("= constant { ptr, i64, i64 }")
        })
        .count();
    assert_eq!(constant, 6, "{ir}");
    let zeroed = ir
        .lines()
        .filter(|l| {
            l.starts_with("@kara.static.table") && l.contains("= global [24 x i8] zeroinitializer")
        })
        .count();
    assert_eq!(zeroed, 1, "{ir}");
    let dir = std::env::temp_dir().join(format!("karac-const-table-{}", std::process::id()));
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
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "90\ntrue a 0.5 -3 one 3\nfalse é 2.0 7 two 3\ntrue z -1.25 127 three 3\ntrue a 0.5 -3 one 3\n1511\n"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
