//! `KARAC_D5_MIGRATE=1 karac fix` (D5, borrow-by-default parameters,
//! `docs/core-semantics.md` §4.1): `own` goes on exactly the bare parameters
//! and receivers whose bodies need ownership, and nothing else changes.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn migrate(src: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "karac_d5_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_D5_MIGRATE", "1")
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    text
}

#[test]
fn a_moved_parameter_gets_own_and_a_read_one_stays_bare() {
    let src = "\
fn keep(s: String) -> String { s }
fn show(s: String) { println(s); }
fn count(n: i64, s: String) -> i64 { n + s.len() }
fn store(v: Vec[String], s: String) -> (Vec[String], String) { (v, s) }
fn main() { println(keep(\"a\")); show(\"b\"); println(count(1, \"c\")); let (v, s) = store(Vec.new(), \"d\"); println(f\"{v.len()} {s}\"); }
";
    let out = migrate(src);
    assert!(out.contains("fn keep(s: own String)"), "{out}");
    assert!(out.contains("fn show(s: String)"), "{out}");
    assert!(out.contains("fn count(n: i64, s: String)"), "{out}");
    assert!(
        out.contains("fn store(v: own Vec[String], s: own String)"),
        "{out}"
    );
}

#[test]
fn passing_to_a_borrowing_parameter_is_not_a_move_but_passing_to_an_own_one_is() {
    // `relay` hands `s` to `keep`, which needs `own`, so `relay` needs it too.
    // `relay_read` hands it to `show`, which stays a borrow, so it does not.
    let src = "\
fn keep(s: String) -> String { s }
fn show(s: String) { println(s); }
fn relay(s: String) -> String { keep(s) }
fn relay_read(s: String) { show(s); }
fn main() { println(relay(\"a\")); relay_read(\"b\"); }
";
    let out = migrate(src);
    assert!(out.contains("fn relay(s: own String)"), "{out}");
    assert!(out.contains("fn relay_read(s: String)"), "{out}");
}

#[test]
fn a_consuming_receiver_and_a_unique_closure_get_own() {
    let src = "\
struct Node { name: String, n: i64 }
impl Node {
    fn into_name(self) -> String { self.name }
    fn n(self) -> i64 { self.n }
}
fn apply(f: MutFn(i64) -> i64, x: i64) -> i64 { f(x) }
fn main() {
    let a = Node { name: \"a\", n: 1 };
    println(a.n());
    println(a.into_name());
    let mut t = 0;
    println(apply(|x| { t = t + x; t }, 3));
}
";
    let out = migrate(src);
    assert!(out.contains("fn into_name(own self)"), "{out}");
    assert!(out.contains("fn n(self)"), "{out}");
    assert!(
        out.contains("fn apply(f: own MutFn(i64) -> i64, x: i64)"),
        "{out}"
    );
}

#[test]
fn every_impl_of_a_trait_method_keeps_the_trait_s_signature() {
    // Only `Sq.take` moves out of `self`, but the trait and `Ci` must agree.
    let src = "\
trait Shape { fn take(self) -> String; fn area(self) -> i64; }
struct Sq { side: i64, tag: String }
impl Shape for Sq {
    fn take(self) -> String { self.tag }
    fn area(self) -> i64 { self.side * self.side }
}
struct Ci { r: i64 }
impl Shape for Ci {
    fn take(self) -> String { \"ci\" }
    fn area(self) -> i64 { self.r }
}
fn main() { println(Sq { side: 2, tag: \"sq\" }.take()); println(Ci { r: 3 }.area()); }
";
    let out = migrate(src);
    assert_eq!(out.matches("fn take(own self)").count(), 3, "{out}");
    assert_eq!(out.matches("fn area(self)").count(), 3, "{out}");
}

#[test]
fn a_migrated_program_is_a_fixed_point() {
    let src = "\
fn keep(s: String) -> String { s }
fn main() { println(keep(\"a\")); }
";
    let once = migrate(src);
    assert_eq!(migrate(&once), once);
}
