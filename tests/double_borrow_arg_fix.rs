//! B-2026-11-11-70, B-2026-11-11-71 — two Rust spellings with no fix:
//! `xs.contains(&&s[..3])`,
//! a borrow of a borrow as an argument, lexes as one `&&` and was a parse
//! error ("Expected expression, found `&&`") that also hid the rest of the
//! file behind a top-level-statements error; and `s.into_bytes()` was a
//! bare "no method". The first now drops its `&&` as a single `&` is
//! dropped, and the second is fixed to `s.bytes().to_vec()`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_drops_a_double_borrow_and_spells_into_bytes() {
    let src = "const MONTHS: Array[Str, 3] = [\"Jan\", \"Feb\", \"Mar\"];
fn main() {
    let s = \"Feb 11 22:14:15\";
    let known = MONTHS.contains(&&s[..3]);
    let v = MONTHS.iter().filter(|m| m.len() == 3).count();
    let b: Vec[u8] = f\"{v} \".to_string().into_bytes();
    println(f\"{known} {v} {b.len()}\");
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-double-borrow-arg-{}.kara",
        std::process::id()
    ));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", file]);
    let text = String::from_utf8_lossy(&check.stderr).into_owned()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(text.contains("drop the `&&`"), "{text}");
    assert!(!text.contains("top-level statements"), "{text}");
    for _ in 0..2 {
        karac(&["fix", file]);
    }
    let out = std::fs::read_to_string(&path).unwrap();
    for want in ["MONTHS.contains(s[..3])", ".to_string().bytes().to_vec();"] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "true 3 2\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
