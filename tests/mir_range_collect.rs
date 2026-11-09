//! `(a..b).collect()` and `(a..=b).collect()` into a `Vec` of the bounds'
//! integer type lower to `Vec.new()` and a counting loop of pushes, which
//! `presize` reserves for (B-2026-11-08-61): kata 484's `(1..=n).collect()`
//! ran 12 instructions an element through `RangeInclusive.next`. The pins:
//! the values at the type's edges (an empty range, `..=` ending at the
//! largest value, a negative start) on the interpreter and the built
//! binary, and the built fill reserving up front instead of calling the
//! library's `collect`.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const EDGES: &str = r#"fn fill(n: i64) -> Vec[i64] {
    let perm: Vec[i64] = (1..=n).collect();
    perm
}
fn main() {
    let a: Vec[u8] = (250u8..=255u8).collect();
    println(f"{a.len()} {a[0]} {a[5]}");
    let b: Vec[i64] = (5..2).collect();
    let c: Vec[i64] = (3..=3).collect();
    let d: Vec[i32] = (-3..3).collect();
    println(f"{b.len()} {c.len()} {c[0]} {d.len()} {d[0]} {d[5]}");
    let n = 4;
    let e: Vec[i64] = (n..n * 2).collect();
    println(f"{e}");
    let big = 9223372036854775807;
    let f: Vec[i64] = (big - 2..=big).collect();
    println(f"{f}");
    let g: Vec[i64] = (big..=big - 1).collect();
    let h: Vec[u8] = (255u8..=255u8).collect();
    let z: Vec[i64] = (4..=1).collect();
    println(f"{g.len()} {h} {z.len()}");
    let p = fill(1000);
    println(f"{p.len()} {p[0]} {p[999]}");
}
"#;

const WANT: &str = "6 250 255\n0 1 3 6 -3 2\n[4, 5, 6, 7]\n\
[9223372036854775805, 9223372036854775806, 9223372036854775807]\n0 [255] 0\n1000 1 1000\n";

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

#[test]
fn mir_range_collect_fills_a_vec_by_counting() {
    let d = std::env::temp_dir().join(format!("karac-range-collect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, EDGES).unwrap();
    let file = file.to_str().unwrap();
    let interp = karac().args(["__mir-run", file]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        WANT,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), WANT);
    let ir = std::fs::read_to_string(&ll).unwrap();
    assert!(!ir.contains(".collect["), "the library's collect is called");
    let fill: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.fill(")))
        .take_while(|l| *l != "}")
        .collect();
    assert!(
        fill.iter().any(|l| l.contains("presize")),
        "fill is not reserved for:\n{}",
        fill.join("\n")
    );
    let _ = std::fs::remove_dir_all(&d);
}
