//! B-2026-11-19-37: `dyn Trait` in a signature or a field above the trait's
//! declaration names the trait, as items are otherwise order-free.

use std::process::{Command, Stdio};

fn run(tag: &str, src: &str) -> (bool, String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-dyn-below-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).expect("write");
    let karac = |sub: &str| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(sub)
            .arg(&path)
            .stdin(Stdio::null())
            .output()
            .expect("karac")
    };
    let check = karac("check");
    let out = karac("__mir-run");
    let _ = std::fs::remove_file(&path);
    (
        check.status.success(),
        String::from_utf8_lossy(&check.stderr).into_owned(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn dyn_trait_declared_below_its_uses() {
    let src = "struct Scene { shapes: Vec[dyn Shape] }
fn total(sc: Scene) -> i64 { sc.shapes.iter().map(|s| s.area()).sum() }
fn area(s: dyn Shape) -> i64 { s.area() }
trait Shape { fn area(self) -> i64; }
struct Sq { side: i64 }
impl Shape for Sq { fn area(self) -> i64 { self.side * self.side } }
fn main() {
    let sc = Scene { shapes: [Sq { side: 2 }, Sq { side: 3 }] };
    println(f\"{total(sc)} {area(Sq { side: 4 })}\");
}
";
    let (ok, err, out) = run("below", src);
    assert!(ok, "{err}");
    assert_eq!(out, "13 16\n");
}
