//! B-2026-11-15-32: a closure handed to `flat_map` that returns a lazy
//! adaptor over a closure reading one of its non-`Copy` captures
//! (`(0..2).flat_map(|m| (0..3).map(|c| m + clears[c]))`) passed `karac
//! check`, then stopped the interpreter and double-freed when compiled. A
//! returned closure takes its captures by move (core semantics §9.3), the
//! adaptor holding it included, so the outer closure moves `clears` and can
//! run once (§9.6): check now refuses it, with `let clears = ref clears;`
//! as the fix, and refuses the `let f = ..; f(0); f(1)` shape as a second
//! call of an `OnceFn`.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(tag: &str, src: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("karac-retadaptor-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f.to_str().unwrap().to_string())
}

fn text(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

const FLAT_MAP: &str = "fn main() {
    let clears: Vec[i64] = [1, 2, 4].to_vec();
    let next: Vec[i64] = (0..2).flat_map(|m| (0..3).map(|c| m + clears[c])).collect();
    println(f\"{next}\");
}
";

#[test]
fn a_flat_map_closure_returning_an_adaptor_over_a_capture_is_refused_and_fixed_with_ref() {
    let (dir, p) = scratch("flatmap", FLAT_MAP);
    let check = karac(&["check", &p]);
    let t = text(&check);
    assert!(!check.status.success(), "{t}");
    assert!(t.contains("this closure can run only once"), "{t}");
    assert!(t.contains("`let clears = ref clears;`"), "{t}");
    assert_eq!(t.matches("error[").count(), 1, "{t}");
    let fix = karac(&["fix", &p]);
    assert!(fix.status.success(), "{}", text(&fix));
    let after = std::fs::read_to_string(&p).unwrap();
    assert!(
        after.contains("    let clears = ref clears;\n    let next: Vec[i64]"),
        "{after}"
    );
    let check = karac(&["check", &p]);
    assert!(check.status.success(), "{after}\n{}", text(&check));
    let run = karac(&["run", "--interp", &p]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "[1, 2, 4, 2, 3, 5]\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_let_bound_closure_returning_an_adaptor_over_a_capture_is_an_once_fn() {
    let src = "fn main() {
    let clears: Vec[i64] = [1, 2, 4].to_vec();
    let f = |m: i64| (0..3).map(|c| m + clears[c]);
    let a: Vec[i64] = f(0).collect();
    let b: Vec[i64] = f(1).collect();
    println(f\"{a} {b}\");
}
";
    let (dir, p) = scratch("letf", src);
    let check = karac(&["check", &p]);
    let t = text(&check);
    assert!(!check.status.success(), "{t}");
    assert!(t.contains("f moved by the first call"), "{t}");
    assert!(t.contains("an OnceFn is called once"), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_let_bound_closure_returning_an_adaptor_called_once_still_runs() {
    let src = "fn main() {
    let clears: Vec[i64] = [1, 2, 4].to_vec();
    let f = |m: i64| (0..3).map(|c| m + clears[c]);
    let a: Vec[i64] = f(1).collect();
    println(f\"{a}\");
}
";
    let (dir, p) = scratch("once", src);
    let check = karac(&["check", &p]);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["run", "--interp", &p]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "[2, 3, 5]\n");
    let _ = std::fs::remove_dir_all(&dir);
}
