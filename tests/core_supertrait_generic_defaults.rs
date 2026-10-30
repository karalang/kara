//! A trait method's own generic parameters are solved from its arguments
//! when it is called on `Self` in a default body or on a bounded type
//! parameter, a supertrait's method included (B-2026-10-29-27). `karac
//! check` accepts the program and every backend prints its oracle.

use std::process::{Command, Stdio};

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["check", "__mir-run", "__mir-llvm-run"]
    } else {
        &["check", "__mir-run"]
    }
}

/// The row's repro, grown: a supertrait's `tag[T]` destructured in a
/// subtrait's default, a bounded `show[T: Clone + Display]`, a generic
/// default calling another, and the same calls on `S: Shape`.
#[test]
fn supertrait_generic_default_solves_its_parameter() {
    let src = "trait Named {
    fn id(self) -> i64;
    fn tag[T](self, t: own T) -> (i64, T) { (self.id(), t) }
    fn twice[U](self, u: own U) -> U { let (i, v) = self.tag(u); v }
    fn show[T: Clone + Display](self, t: T) -> String { f\"{self.id()}:{t.clone()}\" }
}
trait Shape: Named {
    fn area(self) -> i64;
    fn both[U](self, u: own U) -> (i64, U) { let (i, u) = self.tag(u); (i + self.area(), u) }
    fn label(self) -> String { self.show(self.area()) }
    fn pick[V](self, a: own V, b: own V) -> V { let (i, x) = self.tag(a); if i > 0 { x } else { b } }
}
struct Sq { s: i64 }
impl Named for Sq { fn id(self) -> i64 { 100 } }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }
fn via[S: Shape](s: S) -> String {
    let (i, w) = s.tag(2.5);
    let (j, b) = s.both(\"y\".to_string());
    f\"{i} {w} {j} {b} {s.label()} {s.pick(7, 8)}\"
}
fn main() {
    let d = Sq { s: 3 };
    let (a, b) = d.tag(\"x\");
    let (c, e) = d.both(true);
    println(f\"{a} {b} {c} {e} {d.twice(5)} {d.label()}\");
    println(via(Sq { s: 2 }));
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-supertrait-generic-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    for b in backends() {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(b)
            .arg(&path)
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        let want = if *b == "check" {
            ""
        } else {
            "100 x 109 true 5 100:9
100 2.5 104 y 100:4 7
"
        };
        assert_eq!(
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).as_ref()
            ),
            (Some(0), want),
            "{b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}
