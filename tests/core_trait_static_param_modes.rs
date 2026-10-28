//! A static method called through a type parameter (`T.make(s)`) takes the
//! parameter modes its trait declares (B-2026-10-27-32). Before, the call
//! named no impl, so it had no modes and every argument was read as a move:
//! a `mut ref S` passed on in a loop was "moved inside a loop", and the
//! serde `Deserialize` impls raised the same error in the library.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-trait-static-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `src` passes check and prints `expect` on both run lanes.
fn prints(tag: &str, src: &str, expect: &str) {
    let path = write(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{tag}: check: {}", text(&out.stderr));
    for lane in [&["run"][..], &["run", "--interp"]] {
        let out = karac(lane, &path);
        assert!(
            out.status.success(),
            "{tag} {lane:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), expect, "{tag} {lane:?}");
    }
}

#[test]
fn trait_static_mut_ref_arg_in_a_loop() {
    prints("loop", "trait Src {\n    fn next(mut ref self) -> i64;\n}\nstruct Counter { n: i64 }\nimpl Src for Counter {\n    fn next(mut ref self) -> i64 { self.n += 1; self.n }\n}\ntrait Make {\n    fn make[S: Src](s: mut ref S) -> Self;\n}\nstruct W { v: i64 }\nimpl Make for W {\n    fn make[S: Src](s: mut ref S) -> W { W { v: s.next() } }\n}\nfn many[T: Make, S: Src](s: mut ref S) -> Vec[T] {\n    let mut out = Vec.new();\n    for _ in 0..3 {\n        out.push(T.make(s));\n    }\n    out\n}\nfn main() {\n    let mut c = Counter { n: 0 };\n    let ws: Vec[W] = many(mut c);\n    println(ws.len());\n    println(c.n);\n}\n", "3\n3\n");
}

#[test]
fn trait_static_mut_ref_arg_then_reused() {
    prints("reuse", "trait Src {\n    fn next(mut ref self) -> i64;\n}\nstruct Counter { n: i64 }\nimpl Src for Counter {\n    fn next(mut ref self) -> i64 { self.n += 1; self.n }\n}\ntrait Make {\n    fn make[S: Src](s: mut ref S) -> Self;\n}\nstruct W { v: i64 }\nimpl Make for W {\n    fn make[S: Src](s: mut ref S) -> W { W { v: s.next() } }\n}\nfn pair[A: Make, B: Make, S: Src](s: mut ref S) -> (A, B) {\n    s.next();\n    let a = A.make(s);\n    let b = B.make(s);\n    s.next();\n    (a, b)\n}\nfn main() {\n    let mut c = Counter { n: 0 };\n    let p: (W, W) = pair(mut c);\n    println(p.0.v);\n    println(p.1.v);\n    println(c.n);\n}\n", "2\n3\n4\n");
}

/// An `own` parameter of the trait's method still moves its argument.
#[test]
fn trait_static_own_arg_still_moves() {
    let path = write("own", "trait Eat {\n    fn eat(s: own String) -> Self;\n}\nstruct W { n: i64 }\nimpl Eat for W {\n    fn eat(s: own String) -> W { W { n: s.len() } }\n}\nfn twice[T: Eat](s: own String) -> T {\n    let a = T.eat(s);\n    println(s.len());\n    a\n}\nfn main() {\n    let w: W = twice(\"ab\".to_string());\n    println(w.n);\n}\n");
    let out = karac(&["check"], &path);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "check must refuse:\n{err}");
    assert!(
        err.contains("main.kara:10:13: value 's' moved here, used again here (moved at line 9:19)"),
        "{err}"
    );
}
