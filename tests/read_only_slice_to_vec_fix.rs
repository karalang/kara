//! B-2026-11-08-64 — a local bound from a read-only view (`let mut b =
//! s.bytes()`) is a `Slice`, which `let mut` does not make writable. A
//! method that changes a slice in place (`sort`, `swap`, `reverse`, `fill`,
//! `sort_by`, `sort_by_key`, `split_at_mut`) and an assignment through it
//! name the binding, are each reported once, and carry `.to_vec()` on its
//! initializer as their machine fix. A range subscript's view gets no fix:
//! `mut v[a..b]` may be what was meant.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn file(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-slice-to-vec-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn a_view_bound_with_let_mut_is_fixed_with_to_vec() {
    let path = file(
        "bytes",
        "fn main() {\n    let s = \"cab\".to_string();\n    let mut b = s.bytes();\n    b[0] = 120;\n    b.sort();\n    b.swap(0, 1);\n    println(f\"{b[0]} {b[1]} {b[2]}\");\n}\n",
    );
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("`b` is a read-only `Slice`, a view of another value's elements, so `b.sort()` cannot change it")
            && out.contains("`b.swap()` cannot change it")
            && out.contains(
                "`b` is a read-only `Slice[u8]`, a view of another value's elements, so an assignment through `b`"
            ),
        "{out}"
    );
    // The assignment is reported once, not twice.
    assert_eq!(out.matches(":4:5:").count(), 1, "{out}");
    let (_, json) = karac(&["check", "--output=json", p]);
    assert_eq!(json.matches("\".to_vec()\"").count(), 3, "{json}");
    karac(&["fix", p]);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("let mut b = s.bytes().to_vec();\n"),
        "{after}"
    );
    let (ok, out) = karac(&["run", p]);
    assert!(ok && out.ends_with("98 97 120\n"), "{out}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_range_view_names_the_binding_without_a_fix() {
    let path = file(
        "range",
        "fn main() {\n    let v: Vec[i64] = [3, 1];\n    let mut w = v[0..2];\n    w.swap(0, 1);\n    println(f\"{w[0]}\");\n}\n",
    );
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(!ok && out.contains("`w.swap()` cannot change it"), "{out}");
    let (_, json) = karac(&["check", "--output=json", p]);
    assert!(!json.contains("to_vec()\""), "{json}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
