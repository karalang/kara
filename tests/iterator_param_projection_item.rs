//! B-2026-11-19-15: a `for` over an owned parameter `iter: own I` with
//! `I: Iterator` and no `Item = T` written binds `I.Item`, the projection
//! `next()` returns. It bound the iterator's own type `I`, so `t + x` under
//! `where I.Item: Add[Output = I.Item]` was refused with "expected 'I.Item',
//! found 'I'". And an associated function of a projection
//! (`I.Item.default()`, `I.Item.zero()`) is dispatched through the bound a
//! `where I.Item: ..` clause gives it, with `Self` the projection; it typed
//! as an error with no diagnostic and lowering failed with E0516.

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
    let dir = std::env::temp_dir().join(format!(
        "karac-iterator-param-projection-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn runs(name: &str, src: &str, want: &str) {
    let path = scratch(name, src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
    if cfg!(feature = "llvm") {
        let exe = path.with_extension("bin");
        let out = exe.to_str().unwrap();
        let build = karac(&["build", "-o", out], &path);
        assert!(build.status.success(), "{}", text(&build));
        let ran = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), want, "{}", text(&ran));
    }
}

#[test]
fn a_for_over_an_iterator_parameter_binds_its_item() {
    runs(
        "sum.kara",
        "fn sum[I: Iterator](iter: own I, zero: own I.Item) -> I.Item where I.Item: Add[Output = I.Item] {
    let mut t = zero;
    for x in iter {
        t = t + x;
    }
    t
}
fn main() { println(sum([1, 2, 3].into_iter(), 0)); }
",
        "6\n",
    );
}

#[test]
fn associated_functions_of_a_bounded_projection_dispatch() {
    runs(
        "assoc.kara",
        "trait Zero { fn zero() -> Self; }
impl Zero for i64 { fn zero() -> i64 { 0 } }
#[derive(Default, Clone)]
struct P { a: i64 }
impl Zero for P { fn zero() -> P { P { a: 9 } } }
fn first_or_zero[I: Iterator](iter: own I) -> I.Item where I.Item: Zero {
    let mut it = iter;
    match it.next() {
        Some(x) => x,
        None => I.Item.zero(),
    }
}
fn first_or_default[I: Iterator](iter: own I) -> I.Item where I.Item: Default {
    let mut it = iter;
    match it.next() {
        Some(x) => x,
        None => I.Item.default(),
    }
}
fn total[I: Iterator](iter: own I) -> I.Item where I.Item: Zero + Add[Output = I.Item] {
    let mut t = I.Item.zero();
    for x in iter {
        t = t + x;
    }
    t
}
fn fixed[I: Iterator[Item = i64]](iter: own I) -> i64 {
    let mut it = iter;
    match it.next() {
        Some(x) => x,
        None => I.Item.default(),
    }
}
fn main() {
    println(first_or_default([4, 5].into_iter()));
    let e: Vec[i64] = Vec.new();
    println(first_or_zero(e.into_iter()));
    let ps: Vec[P] = Vec.new();
    println(first_or_zero(ps.into_iter()).a);
    let ps2: Vec[P] = Vec.new();
    println(first_or_default(ps2.into_iter()).a);
    let s: Vec[String] = Vec.new();
    println(first_or_default(s.into_iter()).len());
    println(total([1, 2, 3, 4].into_iter()));
    let f: Vec[i64] = Vec.new();
    println(fixed(f.into_iter()));
}
",
        "4\n0\n9\n0\n0\n10\n0\n",
    );
}

#[test]
fn an_associated_function_no_projection_bound_gives_is_refused() {
    let path = scratch(
        "unbounded.kara",
        "fn first[I: Iterator](iter: own I) -> I.Item {
    let mut it = iter;
    match it.next() {
        Some(x) => x,
        None => I.Item.default(),
    }
}
fn main() { println(first([4, 5].into_iter())); }
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains(
            "no associated function 'default' on 'I.Item': no bound on 'I.Item' declares it. \
             Add the trait that does as a bound, `where I.Item: Default`"
        ),
        "{}",
        text(&check)
    );
    assert!(!text(&check).contains("E0516"), "{}", text(&check));
}
