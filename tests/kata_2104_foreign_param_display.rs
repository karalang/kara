//! Kata 2104's scan arm, mutated to carry `(bounds.0.min(x), x)` as its
//! state, was refused with "expected 'Option[((i64, i64), OutV)]'":
//! `OutV` is the item parameter of `scan`'s own signature, which nothing
//! at the call had bound because the closure's return did not check. A
//! parameter of the signature a slot came from, out of scope where the
//! user stands, now reads `_`. A parameter of the user's own generic
//! function, in scope, still reads by its name.

use std::process::{Command, Stdio};

fn check(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success());
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn kata_2104_scan_item_parameter_reads_as_blank() {
    let stderr = check(
        "k2104-outv",
        "fn main() {
    let v = [1, 2, 3].to_vec();
    let s: i64 = v.iter().scan((0, 0), |b, x| Some(((b.0.min(x), x), 1))).sum();
    let t: i64 = v.iter().scan(0, |a, x| Some(a + x)).sum();
    println(f\"{s} {t}\");
}
",
    );
    assert!(!stderr.contains("OutV"), "{stderr}");
    assert!(
        stderr.contains("expected 'Option[((i64, i64), _)]'"),
        "{stderr}"
    );
    assert!(
        stderr.contains("expected 'Option[(i64, _)]', found 'Option[i64]'"),
        "{stderr}"
    );
}

#[test]
fn kata_2104_own_type_parameter_keeps_its_name() {
    let stderr = check(
        "k2104-own-param",
        "fn first[T](v: Vec[T]) -> Option[T] {
    let n: Option[i64] = Some(1);
    n
}
fn main() {
    println(first([1].to_vec()).is_some());
}
",
    );
    assert!(stderr.contains("expected 'Option[T]'"), "{stderr}");
}
