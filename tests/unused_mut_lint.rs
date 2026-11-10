//! `unused_mut` (design.md § Built-in lints, B-2026-11-09-55): a `let mut`
//! that nothing writes, assigns through or borrows as `mut` is warned, and
//! `karac fix` deletes the `mut`. Every kind of write the immutability rule
//! knows keeps its `mut`, a call to a local `MutFn` among them, and a lint
//! attribute on the function or its `impl` block sets the level.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-unused-mut-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

const WRITES: &str = "struct P { x: i64 }
fn bump(v: mut ref Vec[i64]) { v.push(1); }
fn main() {
    let mut a = 1;
    let mut b = 2;
    b += 1;
    let mut p = P { x: 1 };
    p.x = 5;
    let mut v = Vec[1, 2];
    v.push(3);
    let mut w = Vec[1];
    bump(mut w);
    let mut count = 0;
    let mut inc = || { count += 1; };
    inc();
    let mut it = v.iter();
    let f = it.next();
    let mut s = 0;
    for x in v { s += x; }
    let mut (c, d) = (1, 2);
    println(f\"{a} {b} {p.x} {w.len()} {count} {f} {s} {c} {d}\");
}
";

#[test]
fn an_unwritten_let_mut_is_warned_and_fixed_and_every_write_keeps_its_mut() {
    let path = scratch("writes.kara", WRITES);
    let before = text(&karac(&["check"], &path));
    assert_eq!(before.matches("warning[unused_mut]").count(), 2, "{before}");
    assert!(
        before.contains(
            "`a` is never written, assigned through or borrowed as `mut`, so the `let` needs no `mut`"
        ),
        "{before}"
    );
    assert!(before.contains("`c`, `d` are never written"), "{before}");
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        fixed,
        WRITES
            .replace("let mut a = 1;", "let a = 1;")
            .replace("let mut (c, d)", "let (c, d)")
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert!(!text(&check).contains("unused_mut"), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "1 3 5 2 1 Some(1) 6 1 2\n",
        "{}",
        text(&run)
    );
}

#[test]
fn writes_the_immutability_table_misses_still_keep_their_mut() {
    // A `MutFn` called in a `let`, a `VecDeque` written inside a closure,
    // a copy of a borrowed array written by index, and a marked or unmarked
    // `mut ref` method argument (B-2026-11-09-55's corpus sweep).
    let src = "struct R { n: i64 }
impl R {
    fn fill(ref self, buf: mut ref String) { buf.push_str(\"hi\"); }
}
fn copy_of(cnt: Array[i64, 3]) -> i64 {
    let mut left = cnt;
    left[1] -= 1;
    left[1]
}
fn main() {
    let mut window: VecDeque[i64] = VecDeque.new();
    let mut next = |val: i64| {
        window.push_back(val);
        return window.len();
    };
    let a = next(3);
    let r = R { n: 1 };
    let mut s = String.new();
    r.fill(s);
    let mut t = String.new();
    r.fill(mut t);
    println(f\"{a} {copy_of([1, 2, 3])} {s} {t}\");
}
";
    let path = scratch("table_misses.kara", src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert!(!text(&check).contains("unused_mut"), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "1 1 hi hi\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_marked_method_argument_needs_let_mut() {
    let src = "struct R { n: i64 }
impl R {
    fn fill(ref self, buf: mut ref String) { buf.push_str(\"hi\"); }
}
fn main() {
    let r = R { n: 1 };
    let t = String.new();
    r.fill(mut t);
    println(t);
}
";
    let path = scratch("marked_method_arg.kara", src);
    let out = text(&karac(&["check"], &path));
    assert!(
        out.contains("cannot pass `t` as `mut` — `t` is declared without `mut`"),
        "{out}"
    );
}

#[test]
fn a_lint_attribute_on_the_function_or_impl_sets_the_unused_mut_level() {
    let src = "struct S { n: i64 }
impl S {
    #[allow(unused_mut)]
    fn quiet(self) -> i64 { let mut k = self.n; k }
}
#[allow(unused_mut)]
impl S {
    fn also_quiet(self) -> i64 { let mut k = self.n; k }
}
#[deny(unused_mut)]
fn loud() -> i64 { let mut k = 1; k }
fn main() { let s = S { n: 1 }; println(f\"{s.quiet()} {s.also_quiet()} {loud()}\"); }
";
    let path = scratch("levels.kara", src);
    let out = karac(&["check"], &path);
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert_eq!(t.matches("unused_mut").count(), 1, "{t}");
    assert!(t.contains(":11:24:"), "{t}");
}

#[test]
fn an_inner_block_let_leaves_an_outer_let_mut_writable() {
    let src = "fn main() {
    let mut q = 7;
    if q > 0 { let q = 3; println(f\"{q}\"); }
    q = 8;
    println(f\"{q}\");
}
";
    let path = scratch("inner_block.kara", src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert!(!text(&check).contains("unused_mut"), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "3\n8\n",
        "{}",
        text(&run)
    );
}
