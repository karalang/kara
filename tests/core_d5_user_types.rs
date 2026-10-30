//! D5 reads a parameter's mode from the program's own types and impls: a
//! program `struct Mask` is owned, not the SIMD scalar of the same name
//! (B-2026-10-29-47), and each of two operator impls for one type keeps its
//! own parameter modes (B-2026-10-29-46). `karac check` and the MIR
//! interpreter agree.

use std::process::Command;

/// `karac <cmd> <file>` on `src`: whether it succeeded, stdout, stderr.
fn karac(name: &str, cmd: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-d5-user-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `src` checks clean and prints `want` on the MIR interpreter.
fn checks_and_runs(name: &str, src: &str, want: &str) {
    let (ok, _, err) = karac(name, "check", src);
    assert!(ok, "{name}: check refused it:\n{err}");
    let (ok, out, err) = karac(name, "__mir-run", src);
    assert!(ok, "{name}: __mir-run failed:\n{err}");
    assert_eq!(out, want, "{name}");
}

/// The row's repro: `count(m)` twice borrows a program `Mask` both times,
/// as it does the same struct named `Bits`.
#[test]
fn d5_a_program_struct_named_mask_borrows() {
    let src = "struct Mask { on: Vec[bool] }
fn count(m: Mask) -> i64 {
    let mut n = 0;
    for b in m.on { if *b { n += 1; } }
    n
}
fn main() {
    let m = Mask { on: vec![true, false, true] };
    let a = count(m);
    let b = count(m);
    println(f\"{a} {b} {m.on.len()}\");
}
";
    checks_and_runs("mask", src, "2 2 3\n");
    checks_and_runs("bits", &src.replace("Mask", "Bits"), "2 2 3\n");
    // An enum, and an operator's right operand, read the same way.
    checks_and_runs(
        "vector-enum",
        "enum Vector { Two(i64, i64), Named(String) }
fn len(v: Vector) -> i64 {
    match v {
        Vector.Two(a, b) => a + b,
        Vector.Named(s) => s.len() as i64,
    }
}
fn main() {
    let v = Vector.Named(\"abc\".to_string());
    println(f\"{len(v)} {len(v)}\");
}
",
        "3 3\n",
    );
    checks_and_runs(
        "mask-rhs",
        "struct Mask { on: Vec[bool] }
struct Col { v: Vec[i64] }
impl Mul[Mask] for Col {
    type Output = i64;
    fn mul(self, rhs: Mask) -> i64 {
        let mut n = 0;
        for i in 0..self.v.len() { if rhs.on[i] { n += self.v[i]; } }
        n
    }
}
fn main() {
    let c = Col { v: vec![1, 2, 3] };
    let m = Mask { on: vec![true, false, true] };
    println(f\"{c * m} {c * m} {m.on.len()}\");
}
",
        "4 4 3\n",
    );
}

const COL: &str = "struct Col { v: Vec[f64] }
struct Flags { on: Vec[bool] }
";
const ADD_COL: &str = "impl Add for Col {
    type Output = Col;
    fn add(self, rhs: Col) -> Col { Col { v: vec![self.v[0] + rhs.v[0]] } }
}
";
const ADD_FLAGS: &str = "impl Add[Flags] for Col {
    type Output = Col;
    fn add(self, rhs: own Flags) -> Col { Col { v: vec![self.v[0] + rhs.on.len() as f64] } }
}
";

/// Two impls of `Add` for one type each keep their own parameter modes
/// (B-2026-10-29-46): `a + b` borrows `b` through the borrowing impl, in
/// either order of the impls, while `a + f` gives `f` to the owning one.
#[test]
fn d5_two_operator_impls_on_one_type_keep_their_modes() {
    let main = "fn main() {
    let a = Col { v: vec![1.0] };
    let b = Col { v: vec![2.0] };
    let c = a + b;
    let d = a + b;
    let f = Flags { on: vec![true, false] };
    let e = a + f;
    println(f\"{c.v[0]} {d.v[0]} {b.v[0]} {e.v[0]}\");
}
";
    for (name, impls) in [
        ("col-first", format!("{ADD_COL}{ADD_FLAGS}")),
        ("flags-first", format!("{ADD_FLAGS}{ADD_COL}")),
    ] {
        checks_and_runs(name, &format!("{COL}{impls}{main}"), "3.0 3.0 2.0 3.0\n");
        // The owning impl still takes its operand: a second `a + f` is a
        // use of the moved `f`.
        let twice = main.replace("let e = a + f;", "let e = a + f;\n    let g = a + f;");
        let (ok, _, err) = karac(name, "check", &format!("{COL}{impls}{twice}"));
        assert!(!ok && err.contains("'f'"), "{name}: {err}");
    }
}
