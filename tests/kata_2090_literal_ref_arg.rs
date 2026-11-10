//! Kata 2090's bench asks each array for ten radii, `let radii = [0, 1,
//! 3, ...]` walked with `for k in radii.iter()`, and hands each `k` to
//! `get_averages(nums, k: i64)`. A borrowed number handed to a by-value
//! parameter is read as a copy, but while the array's literals were still
//! open the call refused it, "expected 'i64', found 'ref i64'", where the
//! element's own value would have fixed them. The by-value slot now fixes
//! the borrowed literal as it would the value. `Vec.filled(2, k)` over the
//! same loop was a vector of borrows of the element, which the compiled
//! backends read as a pointer and crashed on; a borrowed `Copy` value now
//! fills it with its value, as `[k, k]` and `v.push(k)` take it. A list of
//! float literals, `let fs = [1.0, 3.0];`, is left open as one of integer
//! literals is, so its elements reach an `f32` parameter.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2090_borrowed_literal_to_a_value_parameter() {
    let src = "fn twice(k: i64) -> i64 {
    k * 2
}

fn narrow(k: i32) -> i32 {
    k + 2147483000
}

fn half(x: f32) -> f32 {
    x / 2.0
}

fn main() {
    let radii = [0, 1, 3];
    let mut s = 0;
    for k in radii.iter() {
        s += twice(k);
    }
    let small = [600, 7];
    let mut t: Vec[i32] = Vec.new();
    for k in small.iter() {
        t.push(narrow(k));
    }
    let mut f = 0;
    for k in radii.iter() {
        for a in Vec.filled(2, k).iter() {
            f += a;
        }
    }
    let fs = [1.0, 3.0];
    let h: Vec[f32] = fs.iter().map(|x| half(x)).collect();
    println(f\"{s} {f} {t} {h}\");
}
";
    let file = write("k2090-lit-ref", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "8 8 [2147483600, 2147483007] [0.5, 1.5]\n");
}

/// An integer literal's borrow is still no float, and a float literal's no
/// integer.
#[test]
fn kata_2090_borrowed_literal_keeps_its_kind() {
    for (name, src) in [
        (
            "k2090-int-to-float",
            "fn half(x: f64) -> f64 {
    x / 2.0
}

fn main() {
    let v = [1, 2];
    for x in v.iter() {
        println(f\"{half(x)}\");
    }
}
",
        ),
        (
            "k2090-float-to-int",
            "fn twice(k: i64) -> i64 {
    k * 2
}

fn main() {
    let v = [1.5, 2.5];
    for x in v.iter() {
        println(f\"{twice(x)}\");
    }
}
",
        ),
    ] {
        let file = write(name, src);
        let check = karac(&["check", &file]);
        let _ = std::fs::remove_file(&file);
        assert!(!check.status.success(), "{name} should be refused");
    }
}
