//! B-2026-11-20-88: a type parameter bounded by a function trait written
//! `+ CrossTask` (`[F: Fn() -> i64 + CrossTask]`, core semantics §6.3) is
//! `CrossTask`, so a `par` branch, a `par for` body or a spawned task may
//! capture a value of it; and each function value a caller gives it is
//! checked as one coerced into `Fn() -> i64 + CrossTask` is, so a closure
//! capturing a `shared` handle is refused at the call, through a `where`
//! clause and through a generic that forwards it. An unmarked function
//! bound is still refused, and its fix writes the marker.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> (Output, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-ct-fn-bound-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    (out, after)
}

const HEAD: &str = "shared struct C { v: i64 }
fn both[F: Fn() -> i64 + CrossTask](f: F) -> i64 {
    let (a, b) = par { f(), f() };
    a + b
}
fn each[F](xs: Vec[i64], f: F) -> Vec[i64] where F: Fn(i64) -> i64 + CrossTask {
    par for x in xs.iter() { f(x) }
}
fn fwd[G: Fn() -> i64 + CrossTask](g: G) -> i64 { both(g) }
fn spawned[F: Fn() -> i64 + CrossTask](f: F) -> i64 {
    TaskGroup.new().spawn(|| f()).join()
}
";

#[test]
fn a_cross_task_fn_bound_lets_a_task_capture_the_parameter() {
    let src = format!(
        "{HEAD}fn main() {{
    let k = 2;
    println(both(|| k));
    println(f\"{{each([1, 2], |x| x * k)}}\");
    println(fwd(|| k + 1));
    println(spawned(|| k * 10));
}}
"
    );
    let (out, _) = karac("pure", &["check"], &src);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (out, _) = karac("pure", &["__mir-run"], &src);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "4\n[2, 4]\n6\n20\n");
}

#[test]
fn a_cross_task_fn_bound_refuses_a_closure_capturing_a_shared_handle() {
    for (call, callee) in [
        ("both(|| c.v)", "both"),
        ("each([1], |x| x + c.v).len()", "each"),
        ("fwd(|| c.v)", "fwd"),
        ("spawned(|| c.v)", "spawned"),
    ] {
        let src = format!(
            "{HEAD}fn main() {{
    let c = C {{ v: 3 }};
    println({call});
}}
"
        );
        let (out, _) = karac("shared", &["check"], &src);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{call} was accepted");
        assert!(
            stderr.contains(&format!(
                "the function passed to `{callee}` runs in another task, and its environment \
                 captures `c` (type `C`)"
            )),
            "{call}: {stderr}"
        );
    }
}

#[test]
fn an_unmarked_fn_bound_is_refused_and_fixed_by_the_marker() {
    let src = "fn both[F: Fn() -> i64](f: F) -> i64 {
    let (a, b) = par { f(), f() };
    a + b
}
fn main() { println(both(|| 2)); }
";
    let (out, _) = karac("unmarked", &["check"], src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "an unmarked bound was accepted");
    assert!(
        stderr.contains("help: write `+ CrossTask` after `F`'s function bound"),
        "{stderr}"
    );
    let (out, after) = karac("unmarked-fix", &["fix"], src);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        after.starts_with("fn both[F: Fn() -> i64 + CrossTask](f: F) -> i64 {"),
        "{after}"
    );
    let (out, _) = karac("unmarked-fixed", &["check"], &after);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
