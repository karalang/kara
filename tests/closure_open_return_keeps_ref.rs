//! B-2026-11-19-130: a closure literal whose tail is a `ref` of a `Copy`
//! value, passed where the callee leaves the return open (`ap[T](f: Fn() ->
//! T)` with `ap(|| ref v)`, `g.spawn(|| ref v)`), solved `T` as the value
//! type while the body still returned the reference: `karac run` printed the
//! reference's address, and a join into a `ref i64` stopped on invalid MIR.
//! The open slot keeps a written `ref`, as a let-bound closure and a
//! non-`Copy` value already did, so the result is a reference the borrow
//! checker sees.

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
    let dir = std::env::temp_dir().join(format!("karac-closure-open-ref-{}", std::process::id()));
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
        let build = karac(&["build", "-o", exe.to_str().unwrap()], &path);
        assert!(build.status.success(), "{}", text(&build));
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), want, "{}", text(&ran));
    }
}

fn refused(name: &str, src: &str, want: &str) {
    let path = scratch(name, src);
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(text(&check).contains(want), "{}", text(&check));
}

#[test]
fn an_open_return_keeps_a_closures_ref_of_a_copy_value() {
    refused(
        "literal.kara",
        "fn ap[T](f: Fn() -> T) -> T { f() }
fn main() {
    let v = 5;
    let q = ap(|| ref v);
    let s: String = q;
}
",
        "found 'ref i64'",
    );
}

#[test]
fn a_ref_returned_through_an_open_return_reads_the_value() {
    runs(
        "ap.kara",
        "fn ap[T](f: Fn() -> T) -> T { f() }
fn main() {
    let v = 5;
    let q = ap(|| ref v);
    println(*q);
    let w = ap(|| v + 1);
    println(w);
    let xs: Vec[i64] = [1, 2, 3];
    let s: Vec[i64] = xs.iter().map(|x| x).collect();
    println(s.len());
}
",
        "5\n6\n3\n",
    );
    runs(
        "ap_par.kara",
        "fn ap[T](f: Fn() -> T) -> T { f() }
fn main() {
    let v = 5;
    let q = ap(|| ref v);
    println(q);
    let xs: Vec[i64] = [1, 2, 3];
    let s: Vec[i64] = xs.iter().map(|x| x).collect();
    println(s[2]);
    let r = par { ref v, 1 };
    println(r.0);
}
",
        "5\n3\n5\n",
    );
}

#[test]
fn a_spawned_closures_ref_result_is_the_reference() {
    runs(
        "spawn.kara",
        "fn main() {
    let v = 5;
    let g = TaskGroup.new();
    let h = g.spawn(|| ref v);
    let x: i64 = h.join();
    println(x);
}
",
        "5\n",
    );
}

#[test]
fn a_spawned_ref_that_outlives_its_value_is_refused() {
    refused(
        "spawn_escape.kara",
        "fn main() {
    let e = 1;
    let mut p: ref i64 = ref e;
    {
        let v = 5;
        let g = TaskGroup.new();
        let h = g.spawn(|| ref v);
        p = h.join();
    }
    println(*p);
}
",
        "cannot end `v` while `p` still holds a borrow of it",
    );
}

/// A reference the tail only names or computes is still read as the value
/// (core semantics §5.10), so a closure handing back a temporary's value
/// keeps working; a written `ref` tail in a block keeps the reference.
#[test]
fn a_computed_ref_tail_still_reads_the_value() {
    runs(
        "block_tails.kara",
        "fn ap[T](f: Fn() -> T) -> T { f() }
fn main() {
    let v = 5;
    let q = ap(|| { let r = ref v; r });
    println(q);
    let w = ap(|| { let r = ref v; ref v });
    println(w);
    let g = TaskGroup.new();
    let h = g.spawn(|| { let r = ref v; r });
    println(h.join());
}
",
        "5\n5\n5\n",
    );
    runs(
        "concrete_slot.kara",
        "fn ap(f: Fn() -> i64) -> i64 { f() }
fn main() {
    let v = 5;
    let f: Fn() -> i64 = || ref v;
    println(f());
    println(ap(|| ref v));
    let xs: Vec[i64] = [7, 8];
    let ys: Vec[i64] = xs.iter().map(|x| ref x).collect();
    println(ys[1]);
}
",
        "5\n5\n8\n",
    );
}
