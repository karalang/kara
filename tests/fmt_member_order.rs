//! B-2026-11-05-32: `karac fmt` puts each `impl` and `trait` body in canonical
//! order (design.md § Canonical formatting): associated types, then functions
//! without `self` (constructors such as `new`), then methods, each group
//! alphabetical, with doc comments, comments and attributes travelling with
//! their item.

use std::process::Command;

fn fmt(src: &str, tag: &str) -> String {
    let path =
        std::env::temp_dir().join(format!("karac-fmt-order-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fmt", path.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

const SRC: &str = "struct P { n: i64 }

impl P {
    /// The value.
    fn zeta(self) -> i64 { self.n }

    // makes one
    #[inline]
    fn new(n: i64) -> P { P { n: n } }

    fn alpha(self) -> i64 { self.n } // first method

    fn default() -> P { P.new(0) }
}

trait Shape {
    fn zz(self) -> i64;
    type Unit;
    /// Built from nothing.
    fn make() -> i64;
    fn aa(self) -> i64;
}

fn main() {
    let p = P.new(2);
    println(p.zeta() + p.alpha() + P.default().n);
}
";

#[test]
fn fmt_orders_constructors_then_methods_with_their_comments() {
    let out = fmt(SRC, "impl");
    let want = "impl P {
    fn default() -> P {
        P.new(0)
    }

    // makes one
    #[inline]
    fn new(n: i64) -> P {
        P {
            n: n,
        }
    }

    fn alpha(self) -> i64 {
        self.n
    } // first method

    /// The value.
    fn zeta(self) -> i64 {
        self.n
    }
}
";
    assert!(out.contains(want), "{out}");
    assert_eq!(fmt(&out, "again"), out, "the order is a fixed point");
}

#[test]
fn fmt_orders_a_trait_body_types_first() {
    let out = fmt(SRC, "trait");
    let want = "trait Shape {
    type Unit;
    /// Built from nothing.
    fn make() -> i64;
    fn aa(self) -> i64;
    fn zz(self) -> i64;
}
";
    assert!(out.contains(want), "{out}");
}

#[test]
fn fmt_leaves_a_comment_after_the_body_where_it_was() {
    let src = "struct L { n: i64 }
impl L {
    fn take(self) -> i64 { self.n }
    fn get(self) -> i64 { self.n }
}
fn main() {
    let l = L { n: 1 };
    if l.take() == 1 { println(1); } else { println(0); }   // first -> 1
    println(l.get());
}
";
    let out = fmt(src, "after");
    assert!(
        out.contains("    fn get(self) -> i64 {\n        self.n\n    }\n\n    fn take"),
        "{out}"
    );
    assert!(
        out.contains("    } // first -> 1\n    println(l.get());"),
        "{out}"
    );
    assert_eq!(fmt(&out, "after-again"), out, "the order is a fixed point");
}

#[test]
fn fmt_keeps_a_comment_inside_a_method_body_in_that_body() {
    let src = "struct W { n: i64 }
impl W {
    fn zed(self) -> i64 {
        let a = self.n; // first line note
        a + 1 // second note
    }
    fn abc(self) -> i64 { self.n }
}
fn main() { println(W { n: 1 }.zed()); }
";
    let out = fmt(src, "inside");
    let want = "    fn abc(self) -> i64 {
        self.n
    }

    fn zed(self) -> i64 {
        let a = self.n; // first line note
        a + 1 // second note
    }
}
";
    assert!(out.contains(want), "{out}");
    assert_eq!(fmt(&out, "inside-again"), out, "the order is a fixed point");
}
