//! A join of literals as an element of a list or tuple literal is a literal
//! (design.md §8, "A join of literals is a literal"): the slot types it.
//! `let v: Vec[i32] = [1, if b { 2 } else { 3 }];` and the tuple and
//! `match` forms were refused as narrowing an `i64` (B-2026-11-11-38, kata
//! 1118's month lengths). Every branch is range-checked against the slot.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn runs(tag: &str, src: &str, expected: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-literal-join-elem-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected,
            "{tag} {lane:?}; stderr:\n{stderr}"
        );
        assert!(out.status.success(), "{tag} {lane:?}; stderr:\n{stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_join_of_literals_in_an_annotated_list_or_tuple_takes_the_slot() {
    runs("repros", REPROS, "[1, 2] [2] [2] 2\n");
}

#[test]
fn a_join_element_takes_each_width_and_kind() {
    runs(
        "widths",
        WIDTHS,
        "456\n[0.5, 1.5]\n[31, 29, 31, 30]\n250 -300\n2\n",
    );
}

#[test]
fn a_join_element_out_of_range_for_its_slot_is_refused() {
    let path = std::env::temp_dir().join(format!(
        "karac-literal-join-elem-range-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "fn main() {\n    let b = true;\n    let v: Vec[u8] = [if b { 300 } else { 0 }];\n    println(v);\n}\n",
    )
    .unwrap();
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("integer literal 300 out of range for 'u8'"),
        "{stderr}"
    );
}

const REPROS: &str = r#"fn main() {
    let b = true;
    let v: Vec[i32] = [1, if b { 2 } else { 3 }];
    let w: Vec[i32] = [if b { 2 } else { 3 }];
    let x: Vec[i32] = [match b { true => 2, false => 3 }];
    let t: (i32, i32) = (1, if b { 2 } else { 3 });
    println(f"{v} {w} {x} {t.1}");
}
"#;

const WIDTHS: &str = r#"fn total(v: Vec[u8]) -> i64 {
    let mut s = 0;
    for x in v {
        s += x;
    }
    s
}

fn days(leap: bool) -> Vec[i32] {
    [31, if leap { 29 } else { 28 }, 31, 30]
}

fn main() {
    let b = true;
    let v: Vec[u8] = [200, if b { 255 } else { 0 }, match b { true => 1, false => 2 }];
    println(total(v));
    let f: Vec[f32] = [0.5, if b { 1.5 } else { 2.5 }];
    println(f);
    println(days(true));
    let t: (u8, i16) = (if b { 250 } else { 1 }, { -300 });
    println(f"{t.0} {t.1}");
    let a: Array[i32, 2] = [1, if b { 2 } else { 3 }];
    println(a[1]);
}
"#;
