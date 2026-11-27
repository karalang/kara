//! Two refusals that named the right cause in the wrong place, and a tuple
//! push that kept the default `i64`.
//!
//! - E0208 on `s + t` with `s` a `String` and `t` a `Str` blames the right
//!   operand, so its caret is there (B-2026-11-26-84).
//! - A fill of a value computed from a literal binding, `let g =
//!   Vec.filled(n, f[0] + size)`, refused whole (`f = g` into a
//!   `Vec[i32]`), names the binding and offers its annotation, as a read of
//!   one element does (B-2026-11-26-86).
//! - A bare number beside a fill, a `Some(..)` or a list literal in a tuple
//!   pushed into a `Vec` of tuples takes its slot's type, as at a `let`:
//!   kata 1803's `cases.push((Vec.filled(20000, 20000), 1, 20000))` into a
//!   `Vec[(Vec[i32], i32, i32)]` (B-2026-11-26-86).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-literal-slots-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

fn karac(args: &[&str], f: &std::path::Path) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// `karac check`'s verdict and output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let (ok, out, err) = karac(&["check"], &f);
    let _ = std::fs::remove_dir_all(&dir);
    (ok, format!("{out}{err}"))
}

/// The file `karac fix` writes for `src`.
fn fixed(src: &str) -> String {
    let (dir, f) = file(src);
    let _ = karac(&["fix"], &f);
    let out = std::fs::read_to_string(&f).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn runs_everywhere(src: &str, want: &str) {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        let (dir, f) = file(src);
        let (ok, out, err) = karac(&[cmd], &f);
        let _ = std::fs::remove_dir_all(&dir);
        if cmd != "__mir-run"
            && common::link_or_skip(if ok { Ok(()) } else { Err(err.clone()) }).is_none()
        {
            continue;
        }
        assert!(ok, "{cmd}: {err}\n{src}");
        assert_eq!(out, want, "{cmd}\n{src}");
    }
}

#[test]
fn string_plus_str_points_at_the_right_operand() {
    let src = "fn main() {\n    let a = \"x\".to_string();\n    let s = \"abc\".to_string();\n    \
               let t = a + s[1..];\n    let k = 1;\n    \
               let u = s[..k].to_string() + \"1\" + s[k + 1..];\n    println(t);\n    println(u);\n}\n";
    let (ok, out) = check(src);
    assert!(!ok, "{out}");
    assert!(
        out.contains("4:17: '+' on 'String' requires a 'String' right operand, found 'Str'"),
        "{out}"
    );
    assert!(
        out.contains("6:40: '+' on 'String' requires a 'String' right operand, found 'Str'"),
        "{out}"
    );
    let after = fixed(src);
    assert!(
        after.contains("let t = a + s[1..].to_string();")
            && after.contains("let u = s[..k].to_string() + \"1\" + s[k + 1..].to_string();"),
        "{after}"
    );
    runs_everywhere(&after, "xbc\na1c\n");
}

#[test]
fn a_fill_of_a_value_from_a_literal_binding_names_the_binding() {
    let src = "fn main() {\n    let mut f: Vec[i32] = Vec.filled(4, 0);\n    let size = 2;\n    \
               let g = Vec.filled(4, f[0] + size);\n    f = g;\n    println(f[0]);\n}\n";
    let (ok, out) = check(src);
    assert!(!ok, "{out}");
    assert!(
        out.contains("5:9: expected 'Vec[i32]', found 'Vec[i64]': the elements of `g` are 'i64' because `size`")
            && out.contains("declare it `size: i32`"),
        "{out}"
    );
    let after = fixed(src);
    assert!(after.contains("let size: i32 = 2;"), "{after}");
    runs_everywhere(&after, "2\n");
}

#[test]
fn a_bare_number_in_a_pushed_tuple_takes_its_slot() {
    // Kata 1803's form, unsuffixed.
    runs_everywhere(
        "fn main() {\n    let mut cases: Vec[(Vec[i32], i32, i32)] = Vec.new();\n    \
         cases.push((Vec.filled(3, 20000), 1, 20000));\n    \
         for (v, a, b) in cases.iter() { println(f\"{v[0] + a + b} {v.len()}\"); }\n}\n",
        "40001 3\n",
    );
    runs_everywhere(
        "fn main() {\n    let mut a: Vec[(Option[i32], i32)] = Vec.new();\n    a.push((Some(1), 2));\n    \
         let mut b: Vec[(Vec[i32], i32)] = Vec.new();\n    b.push(([1, 2], 3));\n    \
         let mut c: Vec[(Vec[u8], f32)] = Vec.new();\n    c.push((Vec.filled(2, 7), 1.5));\n    \
         println(f\"{a[0].1} {b[0].1 + b[0].0[1]} {c[0].0[1]} {c[0].1}\");\n}\n",
        "2 5 7 1.5\n",
    );
    // A number the slot cannot hold is still refused.
    let (ok, out) = check(
        "fn main() { let mut a: Vec[(Vec[u8], u8)] = Vec.new(); a.push((Vec.filled(2, 7), 300)); println(a.len()); }\n",
    );
    assert!(!ok, "{out}");
}
