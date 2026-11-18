//! B-2026-11-18-16: `a += b` (and `-=`, `*=`, `/=`, `%=`) on a
//! `#[derive(Arithmetic)]` distinct type passed `karac check` and then both
//! backends refused it (`Add on Agg([Int(7)]) and Agg([Int(2)])`, `MIR to
//! LLVM: Add on a `FloorNum``), while `a = a + b` ran. design.md §5:
//! compound assignment computes `p op e` by the same rule as the operator.
//! Through a `mut ref` of such a value, `x += d` was refused at check.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-distinct-compound-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["check", "__mir-run", "__mir-llvm-run"]
    } else {
        &["check", "__mir-run"]
    }
}

fn runs(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let want = if *b == "check" { "" } else { want };
        assert_eq!(
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).as_ref()
            ),
            (Some(0), want),
            "{tag} on {b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn compound_assignment_on_an_arithmetic_distinct_type_runs() {
    runs(
        "places",
        "#[derive(Arithmetic, Copy)]
distinct type FloorNum = i64;
#[derive(Arithmetic, Copy)]
distinct type Px = f64;
struct Box { n: FloorNum }
fn main() {
    let mut a = FloorNum(7);
    a += FloorNum(2);
    a -= FloorNum(1);
    println(a.raw());
    let mut p = Px(3.0);
    p *= Px(4.0);
    p /= Px(2.0);
    println(p.raw());
    let mut b = Box { n: FloorNum(5) };
    b.n %= FloorNum(3);
    println(b.n.raw());
    let mut v = [FloorNum(1), FloorNum(2)];
    v[1] += v[0];
    println(v[1].raw());
    a += a;
    println(a.raw());
}
",
        "8\n6.0\n2\n3\n16\n",
    );
}

#[test]
fn compound_assignment_through_a_mut_ref_of_a_distinct_value_runs() {
    runs(
        "mutref",
        "#[derive(Arithmetic, Copy)]
distinct type FloorNum = i64;
fn bump(x: mut ref FloorNum, by: FloorNum) { x += by; x *= FloorNum(2); }
fn plus(x: mut ref FloorNum, y: ref FloorNum) { *x = x + y; }
fn main() {
    let mut a = FloorNum(7);
    bump(mut a, FloorNum(3));
    println(a.raw());
    let one = FloorNum(1);
    plus(mut a, one);
    println(a.raw());
}
",
        "20\n21\n",
    );
}

#[test]
fn compound_assignment_on_a_distinct_type_keeps_the_overflow_check() {
    let src = "#[derive(Arithmetic, Copy)]
distinct type FloorNum = i64;
fn main() { let mut a = FloorNum(9223372036854775807); a += FloorNum(1); println(a.raw()); }
";
    for b in backends().iter().filter(|b| **b != "check") {
        let out = karac("overflow", b, src);
        assert!(!out.status.success(), "{b}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("overflow"),
            "{b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
