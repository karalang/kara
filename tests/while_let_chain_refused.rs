//! `while let P = v and c { .. }` is Rust's let-chain in a loop. `if let`
//! refused the shape (B-2026-11-06-39) but `while let` did not: the
//! scrutinee parsed as `v and c` and the checker reported a `bool` operand
//! error and a pattern mismatch (B-2026-11-21-71). It is now one parse error naming
//! it, and `karac fix` moves the rest of the condition into a guard that
//! leaves the loop.

use std::process::Command;

fn karac(args: &[&str], dir: &std::path::Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-whilechain-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

/// Kata 1335's monotonic stack: pop while the top is no harder than `x`.
const STACK: &str = "fn main() {
    let jobs: Vec[i64] = vec![3, 1, 2, 5, 4];
    let mut stack: Vec[i64] = Vec.new();
    for x in jobs.iter() {
        while let Some(top) = stack.last() and *top <= *x {
            stack.pop();
        }
        stack.push(*x);
    }
    println(stack);
}
";

#[test]
fn a_while_let_chain_is_named_and_fixed_with_a_guard() {
    let dir = scratch("fix", STACK);
    let (ok, out) = karac(&["check", "main.kara"], &dir);
    assert!(!ok);
    assert!(out.contains("`while let` cannot be joined"), "{out}");
    assert!(!out.contains("logical operator requires"), "{out}");
    let (_, out) = karac(&["fix", "main.kara"], &dir);
    let fixed = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(
        fixed.contains("while let Some(top) = stack.last() { if not (*top <= *x) { break; }"),
        "{out}\n{fixed}"
    );
    let (ok, out) = karac(&["run", "main.kara"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out, "[5, 4]\n");
}

/// Rust's `Some(&j)` with a chain: the `.cloned()` the `&` asks for goes on
/// the scrutinee, not around `.. and c`, and the two fixes together still
/// end in a program that runs.
#[test]
fn a_while_let_chain_through_some_amp_clones_the_scrutinee_only() {
    let src = STACK
        .replace("Some(top)", "Some(&top)")
        .replace("*top <= *x", "top <= *x");
    let dir = scratch("amp", &src);
    let _ = karac(&["fix", "main.kara"], &dir);
    let _ = karac(&["fix", "main.kara"], &dir);
    let fixed = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(!fixed.contains("and top <= *x).cloned()"), "{fixed}");
    assert!(fixed.contains("stack.last().cloned()"), "{fixed}");
    let (ok, out) = karac(&["run", "main.kara"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}\n{fixed}");
    assert_eq!(out, "[5, 4]\n");
}

#[test]
fn a_bool_pattern_over_an_and_condition_in_a_while_is_not_a_let_chain() {
    let dir = scratch(
        "bool",
        "fn main() {
    let mut n = 0;
    while let true = n < 3 and n != 5 {
        n += 1;
    }
    println(n);
}
",
    );
    let (ok, out) = karac(&["run", "main.kara"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out, "3\n");
}
