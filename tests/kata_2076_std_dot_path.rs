//! Kata 2076's forbid arm moved a group's members out with Rust's
//! `std::mem::take`, written with Kāra's dots: `std.mem.take(mut v)`. The
//! `::` form is reported by the parser with the edit that deletes the
//! module path; the dotted one parses as field accesses on an undeclared
//! `std` and said only "undefined name 'std'". It now names the path and
//! deletes the module segments Kāra has no use for, keeping its own `mem`;
//! a local named `std` is left alone.

use std::process::{Command, Stdio};

fn karac(name: &str, cmd: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("kata_2076_std_dot_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, file.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let text = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        text,
    )
}

const SRC: &str = "fn main() {
    let mut v = [1, 2].to_vec();
    let w = std.mem.take(mut v);
    let m = std.cmp.max(3, 4);
    let k = cmp.min(1, 2);
    println(f\"{w.len()} {v.len()} {m} {k}\");
}
";

#[test]
fn kata_2076_dotted_std_path_is_named() {
    let (out, _) = karac("check", "check", SRC);
    assert!(
        out.contains("`std.mem.take` is a Rust path -- Kāra's library needs no `std` module path: write `mem.take`"),
        "{out}"
    );
    assert!(
        out.contains("`std.cmp.max` is a Rust path -- Kāra's library needs no `std.cmp` module path: write `max`"),
        "{out}"
    );
    assert!(
        out.contains(
            "`cmp.min` is a Rust path -- Kāra's library needs no `cmp` module path: write `min`"
        ),
        "{out}"
    );
    assert!(!out.contains("undefined name 'std'"), "{out}");
}

#[test]
fn kata_2076_dotted_std_path_is_fixed() {
    let (_, text) = karac("fix", "fix", SRC);
    assert!(text.contains("let w = mem.take(mut v);"), "{text}");
    assert!(text.contains("let m = max(3, 4);"), "{text}");
    assert!(text.contains("let k = min(1, 2);"), "{text}");
}

#[test]
fn kata_2076_a_local_named_std_is_left_alone() {
    let (out, _) = karac(
        "local",
        "check",
        "fn main() {
    let std = 2.5;
    println(f\"{std.floor()}\");
}
",
    );
    assert!(!out.contains("error"), "{out}");
}
