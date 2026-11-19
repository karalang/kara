//! B-2026-11-19-137: a borrow conflict on a captured variable inside a
//! closure body names the variable, as the same body in a `for` loop does.
//! The body reads a capture through its environment (`(*(*env).k)`), and
//! the error read "mutable borrow of (*a temporary value) while it is
//! borrowed" because nothing recorded which variable field `k` holds.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn check(src: &str) -> (bool, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-capture-names-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

#[test]
fn a_conflict_on_a_closure_capture_names_the_captured_variable() {
    let (ok, diags) = check(
        r#"fn main() {
    let nums: Vec[i64] = [1, 2, 3];
    let k = 1;
    let mut seen: Map[i64, i64] = Map.new();
    let r = nums.iter().fold((0, 0), |(odd, nice), v| {
        let more = seen.get(odd - k).unwrap_or(0);
        *seen.entry(odd).or_insert(0) += 1;
        (odd + v, nice + more)
    });
    println(f"{r.1}");
}
"#,
    );
    assert!(!ok, "{diags}");
    assert!(
        diags.contains("`seen` while `more` still holds a borrow of it"),
        "{diags}"
    );
    assert!(!diags.contains("a temporary value"), "{diags}");
}
