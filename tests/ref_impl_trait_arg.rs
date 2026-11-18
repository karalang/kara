//! B-2026-11-18-42 (kata 1618): an argument-position `ref impl Trait` /
//! `mut ref impl Trait` was not desugared to an anonymous generic the way a
//! bare `impl Trait` is
//! (design.md § `impl Trait`: sugar for `[T: Trait]`). Forwarding the
//! parameter to another `ref impl Trait` function was refused with
//! "expected 'ref impl Shape', found 'ref impl Shape'".

use std::process::{Command, Stdio};

const SRC: &str = "trait Shape {
    fn area(ref self) -> i32;
    fn grow(mut ref self);
}

struct Sq {
    s: i32,
}

impl Shape for Sq {
    fn area(ref self) -> i32 { self.s * self.s }
    fn grow(mut ref self) { self.s += 1; }
}

fn once(x: ref impl Shape) -> i32 { x.area() }

fn twice(x: ref impl Shape) -> i32 { once(x) + once(x) }

fn bigger(x: mut ref impl Shape, by: ref impl Shape) -> i32 {
    x.grow();
    x.area() + twice(by)
}

fn main() {
    let mut q = Sq { s: 3 };
    let r = Sq { s: 2 };
    println(f\"{once(q)} {twice(q)}\");
    println(bigger(mut q, r));
    println(q.s);
}
";

const OUT: &str = "9 18\n24\n4\n";

fn run(lane: &str) -> (String, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-ref-impl-arg-{lane}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("r.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([lane, "r.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn ref_impl_trait_arg_runs_in_the_interpreter() {
    let (out, err) = run("__mir-run");
    assert_eq!(out, OUT, "{err}");
}

#[test]
fn ref_impl_trait_arg_checks_clean() {
    let (_, err) = run("check");
    assert!(!err.contains("error"), "{err}");
}
