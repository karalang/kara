//! A closure bound by `let f = |..| ..` that only reads a capture borrows it
//! (core-semantics §9.3), and its body runs where `f` is called. So moving
//! the capture after `f`'s last use is fine. The ownership pass paired the
//! body's read with the later move as a reuse, and refused the program
//! (B-2026-10-29-37). A call, or a use of `f`, after the move is still
//! refused, and so is moving `f` before it.

use std::process::Command;

fn check(name: &str, body: &str) -> (bool, String) {
    let src = format!(
        "fn apply(g: Fn(i64) -> i64) -> i64 {{ g(2) }}
fn main() {{
    let name: String = \"abc\".to_string();
    let f = |u: i64| name.len() as i64 + u;
    {body}
}}
"
    );
    let dir = std::env::temp_dir().join(format!("karac-clread-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, &src).unwrap();
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
fn a_capture_moved_after_the_closure_s_last_call_is_accepted() {
    for (name, body, want) in [
        (
            "call",
            "println(f(1)); let taken = name; println(taken);",
            "4\nabc\n",
        ),
        (
            "lent",
            "println(apply(f)); let taken = name; println(taken);",
            "5\nabc\n",
        ),
        (
            "branch",
            "if name.len() > 1 { println(f(1)); } let taken = name; println(taken);",
            "4\nabc\n",
        ),
        (
            "loop",
            "for i in 0..2 { println(f(i)); } let taken = name; println(taken);",
            "3\n4\nabc\n",
        ),
    ] {
        let (ok, err) = check(name, body);
        assert!(ok, "{name}: {err}");
        let src = format!(
            "fn apply(g: Fn(i64) -> i64) -> i64 {{ g(2) }}
fn main() {{
    let name: String = \"abc\".to_string();
    let f = |u: i64| name.len() as i64 + u;
    {body}
}}
"
        );
        assert_eq!(
            karac::mir::lower::run_source(&src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}

#[test]
fn a_closure_used_after_its_capture_moves_is_still_refused() {
    for (name, body) in [
        (
            "call_after",
            "println(f(1)); let taken = name; println(f(2)); println(taken);",
        ),
        (
            "lent_after",
            "let taken = name; println(apply(f)); println(taken);",
        ),
        (
            "moved_closure",
            "let g = f; let taken = name; println(g(1)); println(taken);",
        ),
        (
            "in_a_loop",
            "let mut i = 0; while i < 2 { println(f(i)); let t = name; println(t); i += 1; }",
        ),
    ] {
        let (ok, err) = check(name, body);
        assert!(!ok, "{name}: check must refuse");
        assert!(err.contains("name"), "{name}: {err}");
    }
}
