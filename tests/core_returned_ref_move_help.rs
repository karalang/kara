//! Moving a value while a reference a call returned still points into it
//! is refused, and the help names that reference and the call that
//! returned it. It used to say "drop the slice borrow", though there is no
//! slice (B-2026-10-30-21, README's `name_of` example).

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-retrefhelp-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn moving_under_a_returned_reference_names_the_reference() {
    for (name, decl, call, from) in [
        (
            "function",
            "fn name_of(u: User) -> ref String { ref u.name }",
            "name_of(u)",
            "name_of",
        ),
        (
            "method",
            "impl User { fn nm(ref self) -> ref String { ref self.name } }",
            "u.nm()",
            ".nm()",
        ),
    ] {
        let src = format!(
            "struct User {{ name: String, age: i64 }}
{decl}
fn main() {{
    let u = User {{ name: \"ada\".to_string(), age: 36 }};
    let n = {call};
    let v = u;
    println(n);
    println(v.age);
}}
"
        );
        let (ok, err) = check(name, &src);
        assert!(!ok, "{name}: check accepted a move under a live reference");
        assert!(
            err.contains("cannot move `u` while a borrow into it (a returned reference still points at it) is still live"),
            "{name}: {err}"
        );
        assert!(
            err.contains(&format!(
                "`n`, the reference `{from}` returned, still points into `u`: finish using `n` before moving `u`"
            )),
            "{name}: {err}"
        );
        assert!(!err.contains("slice"), "{name}: {err}");
    }
}
