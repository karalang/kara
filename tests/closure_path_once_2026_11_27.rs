//! B-2026-11-27-24: a closure that moves a captured FIELD or tuple path out
//! (`|| take(o.a)`, `|| t.0.0`), or calls an `own self` method on a capture
//! (`|| w.eat()`, `|| so.unwrap()`, `|| xs.into_iter()`), can run only once,
//! as one that moves the whole binding can (design.md § closures, core
//! semantics §9.3, §9.6). It was typed a repeatable `Fn`, so a
//! `Vec[Fn() -> i64]` took it and its second call read freed memory. Each
//! is refused now with E0235. A closure that only reads its paths, one called
//! once, and a `for` over a captured collection (which borrows it) stay
//! accepted and run.

use std::path::PathBuf;
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str, src: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-closure-path-once-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.kara"), src).unwrap();
        Scratch(dir)
    }

    fn karac(&self, cmd: &str) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&self.0)
            .arg(cmd)
            .arg("main.kara")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const ITEMS: &str = "struct B { s: String }
struct A { b: B, k: i64 }
struct P { a: String, n: i64, opt: Option[String], xs: Vec[String] }
struct W { s: String }
impl W {
    fn eat(own self) -> i64 { self.s.len() }
    fn peek(self) -> i64 { self.s.len() }
}
struct H { w: W }
fn take(s: own String) -> i64 { s.len() }
fn mk() -> P { P { a: f\"a{1}\", n: 4, opt: Some(f\"o{2}\"), xs: [f\"x{3}\", \"yy\"] } }
";

#[test]
fn closure_moving_a_path_or_own_receiver_is_once_callable() {
    for (name, lets, closure, consumed) in [
        ("field", "let o = mk();", "|| take(o.a)", "o.a"),
        (
            "nested",
            "let o = A { b: B { s: f\"x{1}\" }, k: 1 };",
            "|| take(o.b.s)",
            "o.b.s",
        ),
        (
            "tuple",
            "let t = ((f\"x{1}\", 2), 3);",
            "|| take(t.0.0)",
            "t.0.0",
        ),
        (
            "let_bound",
            "let o = mk();",
            "|| { let x = o.a; x.len() }",
            "o.a",
        ),
        (
            "branch_only",
            "let o = mk(); let c = 3 > 2;",
            "|| if c { take(o.a) } else { 0 }",
            "o.a",
        ),
        (
            "own_self_whole",
            "let w = W { s: f\"x{1}\" };",
            "|| w.eat()",
            "w",
        ),
        (
            "own_self_path",
            "let h = H { w: W { s: f\"x{1}\" } };",
            "|| h.w.eat()",
            "h.w",
        ),
        (
            "unwrap_path",
            "let o = mk();",
            "|| o.opt.unwrap().len()",
            "o.opt",
        ),
        (
            "unwrap_whole",
            "let so: Option[String] = Some(f\"x{1}\");",
            "|| so.unwrap().len()",
            "so",
        ),
        (
            "into_iter",
            "let xs: Vec[String] = [f\"x{1}\"];",
            "|| { let mut n = 0; for s in xs.into_iter() { n += s.len(); } n }",
            "xs",
        ),
    ] {
        let src = format!(
            "{ITEMS}fn main() {{\n    {lets}\n    let mut v: Vec[Fn() -> i64] = Vec.new();\n    v.push({closure});\n    println(v[0]() + v[0]());\n}}\n"
        );
        let s = Scratch::new(name, &src);
        let (ok, out) = s.karac("check");
        assert!(
            !ok,
            "{name}: check accepted a once-callable closure in an Fn slot:\n{out}"
        );
        let want = format!("consumes captured binding '{consumed}'");
        assert!(
            out.contains("E0235") && out.contains(&want),
            "{name}: want E0235 naming `{consumed}`, got:\n{out}"
        );
    }
}

#[test]
fn closure_reading_paths_stays_repeatable() {
    let src = format!(
        "{ITEMS}fn main() {{
    let o = mk();
    let w = W {{ s: f\"w{{1}}\" }};
    let mut v: Vec[Fn() -> i64] = Vec.new();
    v.push(|| o.a.len() + o.n);
    v.push(|| {{ let mut n = 0; for s in o.xs {{ n += s.len(); }} n }});
    v.push(|| w.peek());
    let m = || o.opt.unwrap().len();
    let mut t = 0;
    for f in v.iter() {{
        t += f() + f();
    }}
    println(f\"{{t}} {{m()}}\");
}}
"
    );
    let s = Scratch::new("reads", &src);
    let (ok, out) = s.karac("check");
    assert!(ok, "check refused closures that only read:\n{out}");
    let (ok, out) = s.karac("__mir-run");
    // (2 + 4) * 2 + (2 + 2) * 2 + 2 * 2 = 24, and `o.opt.unwrap()` once is 2.
    assert!(
        ok && out.contains("24 2"),
        "the program does not run:\n{out}"
    );
}
