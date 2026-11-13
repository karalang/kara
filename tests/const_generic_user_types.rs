//! B-2026-11-13-80: design.md § Const generic parameters gives structs and
//! enums `const` parameters (`struct Matrix[T, const ROWS: i64, const COLS:
//! i64]`), with constant expressions as arguments and one instance per
//! value. Every argument on a user type was refused ("not yet supported in
//! this slice"). A user type's `const` argument is now a value: `Buf[3]` and
//! `Buf[5]` are distinct types, an `impl[const N: i64] Buf[N]` serves each,
//! and `N` is a value in its methods.

use std::process::{Command, Stdio};

/// `karac check` of `src` (success, stderr), and the MIR interpreter's
/// stdout when it checks clean.
fn run(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-const-user-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("karac")
    };
    let check = karac(&["check", "cell.kara"]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let out = if check.status.success() {
        String::from_utf8_lossy(&karac(&["__mir-run", "cell.kara"]).stdout).into_owned()
    } else {
        String::new()
    };
    let _ = std::fs::remove_dir_all(&dir);
    (check.status.success(), err, out)
}

fn runs(name: &str, src: &str, want: &str) {
    let (ok, err, out) = run(name, src);
    assert!(ok, "{name}: {err}");
    assert_eq!(out, want, "{name}");
}

#[test]
fn const_param_instances_each_carry_their_value() {
    // probe-core/gen/g01_const_instances.kara
    runs(
        "instances",
        "struct Buf[const N: i64] { v: i64 }\n\
impl[const N: i64] Buf[N] { fn cap(self) -> i64 { N } fn scaled(self) -> i64 { self.v * N } }\n\
fn main() {\n    let a: Buf[3] = Buf { v: 2 }; let b: Buf[5] = Buf { v: 3 };\n    println(f\"{a.cap()} {b.cap()}\");\n    println(f\"{a.scaled()} {b.scaled()}\");\n}\n",
        "3 5\n6 15\n",
    );
}

#[test]
fn const_param_sizes_a_field_and_takes_constant_expressions() {
    runs(
        "exprs",
        "const SIZE: i64 = 4;\n\
struct Buf[const N: i64] { a: Array[i64, N] }\n\
impl[const N: i64] Buf[N] {\n    fn new() -> Buf[N] { Buf { a: [7; N] } }\n    fn total(self) -> i64 { let mut s = 0; for x in self.a { s += x; } s }\n    fn cap(self) -> i64 { N }\n}\n\
fn cap_of[const N: i64](b: Buf[N]) -> i64 { N + 100 }\n\
fn main() {\n    let a: Buf[2 + 1] = Buf.new();\n    let b: Buf[SIZE] = Buf.new();\n    println(f\"{a.total()} {b.total()} {a.cap()} {b.cap()} {cap_of(a)} {cap_of(b)}\");\n}\n",
        "21 28 3 4 103 104\n",
    );
}

#[test]
fn const_param_on_an_enum_and_beside_a_type_param() {
    runs(
        "enum",
        "enum Slot[const N: i64] { Empty, Full(i64) }\n\
impl[const N: i64] Slot[N] { fn limit(self) -> i64 { match self { Slot.Empty => N, Slot.Full(v) => v * N } } }\n\
fn main() { let a: Slot[3] = Slot.Full(2); let b: Slot[10] = Slot.Empty; println(f\"{a.limit()} {b.limit()}\"); }\n",
        "6 10\n",
    );
    runs(
        "pair",
        "struct Pair[T, const N: i64] { items: Array[T, N] }\n\
impl[T, const N: i64] Pair[T, N] { fn n(self) -> i64 { N } }\n\
fn main() { let p: Pair[String, 2] = Pair { items: [\"a\".to_string(), \"b\".to_string()] }; println(f\"{p.items[0]} {p.n()}\"); }\n",
        "a 2\n",
    );
}

#[test]
fn const_param_instances_are_distinct_types() {
    let (ok, err, _) = run(
        "distinct",
        "struct Buf[const N: i64] { v: i64 }\nfn take(b: Buf[5]) -> i64 { b.v }\nfn main() { let a: Buf[3] = Buf { v: 1 }; println(take(a)); }\n",
    );
    assert!(!ok);
    assert!(err.contains("expected 'Buf[5]', found 'Buf[3]'"), "{err}");
}

#[test]
fn const_param_argument_must_be_constant() {
    let (ok, err, _) = run(
        "runtime",
        "struct Buf[const N: i64] { v: i64 }\nfn main() { let n = 3; let a: Buf[n] = Buf { v: 1 }; }\n",
    );
    assert!(!ok);
    assert!(
        err.contains("const generic argument must be a compile-time constant expression: `n` is a run-time value"),
        "{err}"
    );
    assert!(!err.contains("not yet supported"), "{err}");
}

#[test]
fn const_param_instances_compiled() {
    if !cfg!(feature = "llvm") {
        return;
    }
    let dir = std::env::temp_dir().join(format!("karac-const-user-llvm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cell.kara"),
        "struct Buf[const N: i64] { a: Array[i64, N] }\n\
impl[const N: i64] Buf[N] { fn total(self) -> i64 { let mut s = 0; for x in self.a { s += x; } s * N } }\n\
fn main() { let a: Buf[3] = Buf { a: [1; 3] }; let b: Buf[5] = Buf { a: [2; 5] }; println(f\"{a.total()} {b.total()}\"); }\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-llvm-run", "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("Linker failed") && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive");
        return;
    }
    assert_eq!(String::from_utf8_lossy(&out.stdout), "9 50\n", "{err}");
}
