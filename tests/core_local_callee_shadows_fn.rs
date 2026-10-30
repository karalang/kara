//! A local closure or function-typed parameter shadows a function of the
//! same name, so a call through it takes the binding's own parameter
//! modes. `let take = |n: own String| ..` named the stdlib's
//! `take(dest: mut ref T)`, so `take(n)` read as a borrow and a later use
//! of `n` passed `karac check`, while MIR refused it as a use after move
//! (B-2026-10-30-2).

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-shadowfn-{}-{name}", std::process::id()));
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
fn an_owning_local_closure_named_like_a_borrowing_fn_moves() {
    for (name, src) in [
        (
            "read_after",
            "fn main() {
    let take = |n: own String| n.len();
    let n = \"b\".to_string();
    let k = take(n);
    println(f\"{k} {n}\");
}
",
        ),
        (
            "call_twice",
            "fn main() {
    let take = |n: own String| { let m = n; m.len() };
    let n = \"b\".to_string();
    let k = take(n);
    let j = take(n);
    println(f\"{k} {j}\");
}
",
        ),
        (
            "param",
            "fn run(take: Fn(own String) -> i64) {
    let n = \"b\".to_string();
    let k = take(n);
    println(f\"{k} {n}\");
}

fn main() { run(|n: own String| n.len()); }
",
        ),
    ] {
        let (ok, err) = check(name, src);
        assert!(!ok, "{name}: check accepted a use after move");
        assert!(
            err.contains("value 'n' moved here, used again here"),
            "{name}: {err}"
        );
        assert!(
            karac::mir::lower::run_source(src).is_err(),
            "{name}: MIR accepted it too"
        );
    }
}

#[test]
fn a_borrowing_local_closure_named_like_an_owning_fn_borrows() {
    let src = "fn eat(s: own String) -> i64 { s.len() }

fn main() {
    let eat = |n: String| n.len();
    let n = \"b\".to_string();
    let k = eat(n);
    let take = |n: String| n.len();
    let j = take(n);
    println(f\"{k} {j} {n}\");
}
";
    let (ok, err) = check("borrow", src);
    assert!(ok, "{err}");
    assert_eq!(
        karac::mir::lower::run_source(src)
            .expect("MIR interpreter")
            .output,
        "1 1 b\n"
    );
}
