//! B-2026-11-02-18 (core semantics §6.3), the review thread's structural
//! rule: a value is coerced, and its environment checked, wherever it flows
//! into a slot whose type holds `+ CrossTask` once the call's type
//! parameters are solved. That is every call argument against its
//! parameter's solved type (a generic impl's `Bx.new(x)` too), and every
//! closure's result against its solved return type (`unwrap_or_else`,
//! `get_or_insert_with`, `fold`). A callee with no body here (`mem.take`)
//! returns what its arguments reach.

use std::process::{Command, Output};

fn check(tag: &str, src: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("karac-solved-param-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const PRELUDE: &str = "shared struct S { mut n: i64 }
fn mkf(s: own S) -> Fn() -> i64 { || { s.n = s.n + 1; s.n } }
fn race2(f: Fn() -> i64 + CrossTask) -> i64 {
    let g = TaskGroup.new();
    let a = g.spawn(|| f());
    let b = g.spawn(|| f());
    a.join() + b.join()
}
struct Bx[T] { x: T }
impl[T] Bx[T] { fn new(x: own T) -> Bx[T] { Bx { x: x } } }
";

fn refused(tag: &str, body: &str, slot: &str) {
    let out = check(tag, &format!("{PRELUDE}{body}"));
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{tag}: check passed\n{err}");
    assert!(
        err.contains("E_NOT_CROSS_TASK") && err.contains(slot),
        "{tag}:\n{err}"
    );
}

#[test]
fn a_generic_impls_associated_fn_checks_its_argument_as_solved() {
    refused(
        "bx_new",
        "fn main() {
    let b: Bx[Fn() -> i64 + CrossTask] = Bx.new(mkf(S { n: 0 }));
    println(race2(b.x));
}
",
        "a parameter whose type is `+ CrossTask` as solved",
    );
}

#[test]
fn a_closures_result_is_checked_against_its_solved_return_type() {
    refused(
        "fold",
        "fn main() {
    let init: Fn() -> i64 + CrossTask = || 1;
    let f = (0..1).fold(init, |acc, i| mkf(S { n: i }));
    println(race2(f));
}
",
        "a closure's `+ CrossTask` return type",
    );
    refused(
        "unwrap_or_else",
        "fn main() {
    let o: Option[Fn() -> i64 + CrossTask] = None;
    let f: Fn() -> i64 + CrossTask = o.unwrap_or_else(|| mkf(S { n: 0 }));
    println(race2(f));
}
",
        "a closure's `+ CrossTask` return type",
    );
    refused(
        "get_or_insert_with",
        "fn main() {
    let mut o: Option[Fn() -> i64 + CrossTask] = None;
    o.get_or_insert_with(|| mkf(S { n: 0 }));
    println(race2(o.unwrap()));
}
",
        "a closure's `+ CrossTask` return type",
    );
}

#[test]
fn mem_take_from_an_unmarked_place_is_out_of_sight() {
    refused(
        "mem_take",
        "fn main() {
    let mut src: Option[Fn() -> i64] = Some(mkf(S { n: 0 }));
    let o: Option[Fn() -> i64 + CrossTask] = mem.take(mut src);
    println(race2(o.unwrap()));
}
",
        "a value coerced into a `let` whose type is `+ CrossTask`",
    );
}

/// Marked values through the same paths, and closures handed to generic
/// slots whose solved types are not marked, still check: a closure that
/// writes a `shared` handle is fine where its result is a plain value.
#[test]
fn marked_values_and_unmarked_slots_through_the_same_paths_check() {
    let out = check(
        "ok",
        &format!(
            "{PRELUDE}fn main() {{
    let b: Bx[Fn() -> i64 + CrossTask] = Bx.new(|| 3);
    let init: Fn() -> i64 + CrossTask = || 1;
    let f = (0..3).fold(init, |acc, i| {{ let g: Fn() -> i64 + CrossTask = || i; g }});
    let o: Option[Fn() -> i64 + CrossTask] = None;
    let h: Fn() -> i64 + CrossTask = o.unwrap_or_else(|| || 9);
    let mut p: Option[Fn() -> i64 + CrossTask] = None;
    p.get_or_insert_with(|| || 4);
    let s = S {{ n: 0 }};
    let u: Option[i64] = None;
    let k = u.unwrap_or_else(|| {{ s.n = s.n + 1; s.n }});
    let q = Some(5).map(|x| {{ s.n = s.n + x; let m: Fn() -> i64 + CrossTask = || 2; m }});
    let c = Bx.new(mkf(S {{ n: 0 }}));
    println(race2(b.x) + race2(f) + race2(h) + race2(p.unwrap()) + k + race2(q.unwrap()) + (c.x)());
}}
"
        ),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
