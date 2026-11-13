//! B-2026-11-13-23: after `eat(p)` moves `p` as a whole, `p.a = v` is
//! refused (core semantics §3.4: a wholly moved value is initialized again
//! only as a whole). The help suggested deriving `Clone` on the field's type
//! and cloning `p` at the move, though nothing reads `p`; it now says to
//! assign all of `p` or move only the fields the callee needs, and offers no
//! `karac fix` edit.

use std::process::{Command, Stdio};

fn check(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-part-assign-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const TYPES: &str = "struct D { n: String }\nstruct P { a: D, b: D }\nfn d(s: Str) -> D { D { n: s.to_string() } }\nfn eat(p: own P) { println(f\"eat {p.a.n} {p.b.n}\"); }\n";

#[test]
fn a_field_assigned_after_a_whole_move_is_told_to_assign_the_whole_root() {
    let err = check(
        "field",
        &format!("{TYPES}fn main() {{\n    let mut p = P {{ a: d(\"a\"), b: d(\"b\") }};\n    eat(p);\n    p.a = d(\"x\");\n    println(\"end\");\n}}\n"),
    );
    assert!(
        err.contains("cell.kara:8:5: value 'p' moved here, used again here (moved at line 7:9)"),
        "{err}"
    );
    assert!(
        err.contains(
            "help: 'p' was moved as a whole, and a wholly moved value is initialized again only \
             as a whole (core semantics §3.4): assign all of it (`p = ...`), or move out only \
             the fields the callee needs"
        ),
        "{err}"
    );
    assert!(!err.contains("Clone"), "{err}");
}

#[test]
fn a_moved_root_read_through_a_field_keeps_the_clone_advice() {
    // `p.a.n` reads `p`, so a copy of `p` is what would make it live.
    let err = check(
        "read",
        &format!("{TYPES}fn main() {{\n    let p = P {{ a: d(\"a\"), b: d(\"b\") }};\n    eat(p);\n    println(p.a.n);\n}}\n"),
    );
    assert!(
        err.contains("value 'p' moved here, used again here"),
        "{err}"
    );
    assert!(!err.contains("§3.4"), "{err}");
}
