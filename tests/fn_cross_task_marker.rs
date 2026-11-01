//! Core semantics §6.3 for erased function types (B-2026-11-01-26): an
//! erased `Fn` / `MutFn` / `OnceFn` type hides the environment behind it, so
//! it is `CrossTask` only when written `Fn(A) -> R + CrossTask`, and each
//! value coerced into that form is checked: its environment must reach only
//! `CrossTask` values, and must be in sight. `karac check` answered yes for
//! any type holding an erased function, so a `shared` handle crossed tasks
//! inside one (a data race, 166280 of 200000 increments). A function value
//! bound from a closure literal in the same body is not erased.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("karac-fn-cross-task-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn check(tag: &str, src: &str) -> (bool, String) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn refused(tag: &str, src: &str, needle: &str) {
    let (ok, out) = check(tag, src);
    assert!(!ok, "{tag}: check accepted it:\n{out}");
    assert!(
        out.contains("E_NOT_CROSS_TASK") && out.contains(needle),
        "{tag}: want E_NOT_CROSS_TASK with {needle:?}:\n{out}"
    );
}

fn accepted_and_runs(tag: &str, src: &str, want: &str) {
    let (ok, out) = check(tag, src);
    assert!(ok, "{tag}: check refused it:\n{out}");
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const S: &str = "shared struct S { mut n: i64 }
fn mkf(s: own S) -> Fn() -> i64 { || { s.n = s.n + 1; s.n } }
";

#[test]
fn an_erased_function_value_does_not_cross_a_task() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "returned",
            "fn main() { let f = mkf(S { n: 0 }); let g = TaskGroup.new(); \
             println(g.spawn(|| f()).join()); }",
            "capture of `f` (type `Fn() -> i64`)",
        ),
        (
            "spawned-directly",
            "fn main() { let f = mkf(S { n: 0 }); let g = TaskGroup.new(); \
             println(g.spawn(f).join()); }",
            "`f` (type `Fn() -> i64`) cannot cross",
        ),
        (
            "param",
            "fn go(f: Fn() -> i64) -> i64 { TaskGroup.new().spawn(|| f()).join() }
fn main() { println(go(mkf(S { n: 0 }))); }",
            "capture of `f`",
        ),
        (
            "struct-field",
            "struct W { f: Fn() -> i64 }
fn main() { let s = S { n: 0 }; let w = W { f: || s.n }; let g = TaskGroup.new(); \
             println(g.spawn(|| (w.f)()).join()); }",
            "capture of `w` (type `W`)",
        ),
        (
            "vec",
            "fn main() { let s = S { n: 0 }; let mut v: Vec[Fn() -> i64] = Vec.new(); \
             v.push(|| s.n); let g = TaskGroup.new(); println(g.spawn(|| v[0]()).join()); }",
            "capture of `v`",
        ),
        (
            "option",
            "fn mko(s: own S) -> Option[Fn() -> i64] { Some(|| s.n) }
fn main() { let o = mko(S { n: 0 }); let g = TaskGroup.new(); \
             println(g.spawn(|| match o { Some(f) => f(), None => 0 }).join()); }",
            "capture of `o`",
        ),
        (
            "map",
            "fn main() { let s = S { n: 0 }; let mut m: Map[i64, Fn() -> i64] = Map.new(); \
             m.insert(1, || s.n); let g = TaskGroup.new(); println(g.spawn(|| m.len()).join()); }",
            "capture of `m`",
        ),
        (
            "closure-returning-closure",
            "fn main() { let mkc = || { let s = S { n: 0 }; let c: Fn() -> i64 = || s.n; c }; \
             let f = mkc(); let g = TaskGroup.new(); println(g.spawn(|| f()).join()); }",
            "capture of `f`",
        ),
        (
            "channel",
            "fn main() { let (tx, rx) = channel(1); let _ = tx.send(mkf(S { n: 0 })); \
             println(rx.recv().unwrap()()); }",
            "value sent across a channel",
        ),
        (
            "sync-field",
            "sync struct Y { f: Fn() -> i64 }
fn main() { let y = Y { f: mkf(S { n: 0 }) }; println((y.f)()); }",
            "field `f` of `sync struct Y`",
        ),
        (
            "isolating-group",
            "fn main() { let f = mkf(S { n: 0 }); let g = TaskGroup.isolating(); \
             println(g.spawn(|| f()).join().unwrap()); }",
            "capture of `f`",
        ),
    ];
    for (tag, body, needle) in cases {
        refused(tag, &format!("{S}{body}"), needle);
    }
}

#[test]
fn a_marked_function_type_crosses_and_its_coercions_are_checked() {
    accepted_and_runs(
        "marked",
        "fn mk(k: i64) -> Fn() -> i64 + CrossTask { || k + 1 }
fn go(f: Fn() -> i64 + CrossTask) -> i64 { TaskGroup.new().spawn(|| f()).join() }
struct W { f: Fn() -> i64 + CrossTask }
fn main() {
    let f = mk(3);
    println(go(f));
    println(go(|| 9));
    let w = W { f: || 5 };
    let g = TaskGroup.new();
    println(g.spawn(|| (w.f)()).join());
}
",
        "4\n9\n5\n",
    );
    // Into a marked return type, parameter, field and element: a `shared`
    // capture is refused, and so is an erased value from out of sight.
    let cases: &[(&str, &str, &str)] = &[
        (
            "return",
            "fn mk2(s: own S) -> Fn() -> i64 + CrossTask { || s.n }
fn main() { println(mk2(S { n: 0 })()); }",
            "coerced into the function's `+ CrossTask` return type",
        ),
        (
            "param-shared",
            "fn go(f: Fn() -> i64 + CrossTask) -> i64 { TaskGroup.new().spawn(|| f()).join() }
fn main() { let s = S { n: 0 }; println(go(|| s.n)); }",
            "captures `s`",
        ),
        (
            "param-out-of-sight",
            "fn go(f: Fn() -> i64 + CrossTask) -> i64 { TaskGroup.new().spawn(|| f()).join() }
fn main() { println(go(mkf(S { n: 1 }))); }",
            "out of sight",
        ),
        (
            "field",
            "struct W { f: Fn() -> i64 + CrossTask }
fn main() { let s = S { n: 0 }; let w = W { f: || s.n }; println((w.f)()); }",
            "a field whose type is `+ CrossTask`",
        ),
        (
            "element",
            "fn main() { let s = S { n: 0 }; let mut v: Vec[Fn() -> i64 + CrossTask] = Vec.new(); \
             v.push(|| s.n); println(v.len()); }",
            "an element whose type is `+ CrossTask`",
        ),
        (
            "let",
            "fn main() { let f: Fn() -> i64 + CrossTask = mkf(S { n: 0 }); println(f()); }",
            "a `let` whose type is `+ CrossTask`",
        ),
    ];
    for (tag, body, needle) in cases {
        refused(tag, &format!("{S}{body}"), needle);
    }
}

#[test]
fn a_function_value_bound_from_a_closure_literal_is_in_sight() {
    accepted_and_runs(
        "literal-local",
        "fn main() { let k = 3; let f = || k + 1; let g = TaskGroup.new(); \
         println(g.spawn(|| f()).join()); }
",
        "4\n",
    );
    // ...and its environment is still followed (B-2026-10-26-13).
    refused(
        "literal-local-shared",
        &format!(
            "{S}fn main() {{ let s = S {{ n: 0 }}; let f = || s.n; let g = TaskGroup.new(); \
             println(g.spawn(|| f()).join()); }}"
        ),
        "captures `s`",
    );
}

#[test]
fn the_fix_adds_the_marker_where_the_type_is_declared() {
    let src = "fn go(f: Fn() -> i64) -> i64 { TaskGroup.new().spawn(|| f()).join() }
fn main() { println(go(|| 2)); }
";
    let (dir, path) = fixture("fix", src);
    let out = karac().arg("fix").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("fn go(f: Fn() -> i64 + CrossTask) -> i64"),
        "fix wrote:\n{fixed}"
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_dir_all(dir);
    // The marker goes before an effect clause, and binds to the nearest
    // function type; the formatter keeps it.
    let src = "fn go(f: Fn(i64) -> Fn() -> i64 + CrossTask with _) -> i64 with _ { f(1)() }
fn main() { println(go(|x| || 1)); }
";
    let (dir, path) = fixture("fmt", src);
    let out = karac().arg("fmt").arg(&path).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("f: Fn(i64) -> Fn() -> i64 + CrossTask with _"),
        "fmt printed:\n{text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
