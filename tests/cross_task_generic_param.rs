//! B-2026-11-02-15 (core semantics §6.3): an unmarked function value does
//! not reach a `+ CrossTask` slot through a generic parameter that
//! inference solved as the marked type. `collect()` takes each item as a
//! push would, a generic function's result reaches what its arguments
//! reach, and a generic callee may write its arguments through a `mut`
//! argument of a marked type.

use std::path::PathBuf;
use std::process::{Command, Output};

fn check(tag: &str, src: &str) -> Output {
    let dir =
        std::env::temp_dir().join(format!("karac-generic-param-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("main.kara");
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
fn id[T](x: own T) -> T { x }
fn race2(f: Fn() -> i64 + CrossTask) -> i64 {
    let g = TaskGroup.new();
    let a = g.spawn(|| f());
    let b = g.spawn(|| f());
    a.join() + b.join()
}
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

fn passes(tag: &str, body: &str) {
    let out = check(tag, &format!("{PRELUDE}{body}"));
    assert!(
        out.status.success(),
        "{tag}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn collect_of_unmarked_fn_items_into_a_marked_vec_is_refused() {
    refused(
        "collect",
        "fn main() {
    let v: Vec[Fn() -> i64 + CrossTask] = (0..1).map(|i| mkf(S { n: i })).collect();
    println(race2(v[0]));
}
",
        "a value coerced into a `let` whose type is `+ CrossTask`",
    );
    refused(
        "collect_local",
        "fn main() {
    let v: Vec[Fn() -> i64 + CrossTask] = (0..1)
        .map(|i| {
            let t = S { n: i };
            let c: Fn() -> i64 = || { t.n = t.n + 1; t.n };
            c
        })
        .collect();
    println(race2(v[0]));
}
",
        "a value coerced into a `let` whose type is `+ CrossTask`",
    );
}

#[test]
fn collect_of_marked_fn_items_into_a_marked_vec_passes() {
    passes(
        "collect_marked",
        "fn main() {
    let v: Vec[Fn() -> i64 + CrossTask] = (0..2)
        .map(|i| {
            let c: Fn() -> i64 + CrossTask = || i * 2;
            c
        })
        .collect();
    let w: Vec[Fn() -> i64 + CrossTask] = v.into_iter().map(|f| f).collect();
    println(race2(w[1]));
}
",
    );
}

#[test]
fn a_generic_identity_does_not_mark_its_argument() {
    refused(
        "id_call",
        "fn main() {
    let f: Fn() -> i64 + CrossTask = id(mkf(S { n: 0 }));
    println(f());
}
",
        "a value coerced into a `let` whose type is `+ CrossTask`",
    );
    let out = check(
        "id_leak",
        &format!(
            "{PRELUDE}fn main() {{
    let s = S {{ n: 0 }};
    let f: Fn() -> i64 + CrossTask = id(|| {{ s.n = s.n + 1; s.n }});
    println(f());
}}
"
        ),
    );
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success() && err.contains("captures `s`"),
        "{err}"
    );
    passes(
        "id_marked",
        "fn main() {
    let f: Fn() -> i64 + CrossTask = id(|| 5);
    let g: Fn() -> i64 + CrossTask = || 7;
    let h: Fn() -> i64 + CrossTask = id(g);
    println(race2(f) + race2(h));
}
",
    );
}

#[test]
fn mem_replace_and_swap_into_a_marked_place_check_what_they_write() {
    refused(
        "replace",
        "fn main() {
    let mut f: Fn() -> i64 + CrossTask = || 1;
    let old = mem.replace(mut f, mkf(S { n: 0 }));
    println(old());
}
",
        "a value coerced into a place whose type is `+ CrossTask`",
    );
    refused(
        "swap",
        "fn main() {
    let mut f: Fn() -> i64 + CrossTask = || 1;
    let mut g: Fn() -> i64 = mkf(S { n: 0 });
    mem.swap(mut f, mut g);
    println(race2(f));
}
",
        "a value coerced into a place whose type is `+ CrossTask`",
    );
    passes(
        "replace_marked",
        "fn main() {
    let mut f: Fn() -> i64 + CrossTask = || 1;
    let old = mem.replace(mut f, || 2);
    let mut g: Fn() -> i64 + CrossTask = || 3;
    mem.swap(mut f, mut g);
    println(old() + f() + g());
}
",
    );
}
