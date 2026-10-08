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

#[test]
fn keep_meaning_writes_own_on_every_bare_non_copy_position() {
    // `all` is for placeholder bodies: what the body does says nothing.
    let src = "\
struct S { n: i64 }
impl S {
    fn take(self, s: String, n: i64) -> i64 { n }
    fn look(ref self, s: String) -> i64 { 0 }
}
fn main() { println(S { n: 1 }.take(\"a\", 2)); }
";
    let dir = std::env::temp_dir().join(format!("karac_d5_all_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_D5_MIGRATE", "all")
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
    assert!(
        text.contains("fn take(own self, s: own String, n: i64)"),
        "{text}"
    );
    assert!(text.contains("fn look(ref self, s: own String)"), "{text}");
}

/// A returned `Option[R]` or `Result[R, E]` parameter is a move when `R`
/// is not `Copy`; one over Copy parts is not. The migration used to read
/// every `Option[..]` parameter as `Copy`, since it dropped the arguments.
#[test]
fn a_returned_option_of_a_drop_type_gets_own() {
    let src = "\
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\"); } }
fn pass(x: Option[R]) -> Option[R] { x }
fn res(x: Result[R, String]) -> Result[R, String] { x }
fn num(x: Option[i64]) -> Option[i64] { x }
fn pair(x: (i64, i64)) -> (i64, i64) { x }
fn main() { let _a = pass(None); let _b = res(Err(\"e\")); println(num(None).is_none()); println(pair((1, 2)).0); }
";
    let out = migrate(src);
    assert!(out.contains("fn pass(x: own Option[R])"), "{out}");
    assert!(out.contains("fn res(x: own Result[R, String])"), "{out}");
    assert!(out.contains("fn num(x: Option[i64])"), "{out}");
    assert!(out.contains("fn pair(x: (i64, i64))"), "{out}");
}

/// A receiver a built-in collection's method consumes (`into_iter`) is a
/// move; one `iter` borrows is not.
#[test]
fn a_parameter_consumed_by_into_iter_gets_own() {
    let src = "\
struct S { s: String }
fn f(rows: Vec[S]) -> i64 { let mut n = 0; for r in rows.into_iter() { n = n + r.s.len(); } n }
fn r(rows: Vec[S]) -> i64 { let mut n = 0; for x in rows.iter() { n = n + x.s.len(); } n }
fn main() { println(f(Vec.new())); println(r(Vec.new())); }
";
    let out = migrate(src);
    assert!(out.contains("fn f(rows: own Vec[S])"), "{out}");
    assert!(out.contains("fn r(rows: Vec[S])"), "{out}");
}

/// A parameter whose value runs a `Drop` body when it dies keeps `own` even
/// when the body only reads it: as a borrow it would be dropped at the
/// caller's scope end instead, and the program would print differently.
/// A type with no `Drop` body anywhere inside still becomes a borrow.
#[test]
fn a_parameter_that_runs_a_drop_body_keeps_its_drop_in_the_callee() {
    let src = "\
struct W { id: i64 }
impl Drop for W { fn drop(mut ref self) { println(f\"d{self.id}\"); } }
struct Plain { s: String }
struct Holder { w: W }
fn consume(x: W) { println(\"in\"); }
fn look(x: Plain) -> i64 { x.s.len() }
fn hold(h: Holder) -> i64 { h.w.id }
fn many(v: Vec[W]) -> i64 { v.len() }
impl W { fn show(self) { println(f\"{self.id}\"); } fn peek(ref self) -> i64 { self.id } }
fn main() { consume(W { id: 1 }); println(look(Plain { s: \"a\" })); }
";
    let out = migrate(src);
    assert!(out.contains("fn consume(x: own W)"), "{out}");
    assert!(out.contains("fn look(x: Plain)"), "{out}");
    assert!(out.contains("fn hold(h: own Holder)"), "{out}");
    assert!(out.contains("fn many(v: own Vec[W])"), "{out}");
    assert!(out.contains("fn show(own self)"), "{out}");
    assert!(out.contains("fn peek(ref self)"), "{out}");
}

/// The shapes Thread B's D5 corpus run found still moving out of a borrow:
/// a parameter handed back as a `match` arm's value, a payload moved out by
/// `if let`, a `Result` (never `Copy`, core semantics §1.1), a parameter a
/// closure moves out, and one captured by a closure that is returned. A
/// closure that only reads a parameter and stays in the function leaves it
/// borrowed, and so does an `if let` over a `Copy` value.
#[test]
fn match_arms_if_let_results_and_closures_that_take_a_parameter_get_own() {
    let src = "\
enum E { S(String), N }
struct W { s: String }
fn pick(a: String, b: String) -> String { match a > b { true => a, false => b } }
fn take(s: E) -> String { if let E.S(x) = s { return x; } \"z\".to_string() }
fn small(o: Option[i64]) -> i64 { if let Some(x) = o { x } else { 0 } }
fn idr(r: Result[Option[i64], i64]) -> Result[Option[i64], i64] { r }
fn back(w: W) -> W { let g = || w; g() }
fn make(p: String, base: i64) -> Fn(i64) -> i64 { |n| p.len() + base + n }
fn local(p: String) -> i64 { let g = |n: i64| p.len() + n; g(1) }
fn main() {
    println(pick(\"x\".to_string(), \"y\".to_string()));
    println(take(E.N));
    println(small(None));
    println(idr(Ok(None)).is_ok());
    println(back(W { s: \"w\".to_string() }).s);
    println(make(\"ab\".to_string(), 1)(2));
    println(local(\"abc\".to_string()));
}
";
    let out = migrate(src);
    assert!(
        out.contains("fn pick(a: own String, b: own String)"),
        "{out}"
    );
    assert!(out.contains("fn take(s: own E)"), "{out}");
    assert!(out.contains("fn small(o: Option[i64])"), "{out}");
    assert!(
        out.contains("fn idr(r: own Result[Option[i64], i64])"),
        "{out}"
    );
    assert!(out.contains("fn back(w: own W)"), "{out}");
    assert!(out.contains("fn make(p: own String, base: i64)"), "{out}");
    assert!(out.contains("fn local(p: String)"), "{out}");
}

/// A parameter written into a collection literal moves into it, so it keeps
/// `own` (`[x]`, `Vec[...]`, a map literal's key and value).
#[test]
fn a_parameter_moved_into_a_collection_literal_gets_own() {
    let src = "\
fn dup(x: String) -> Vec[String] { [x] }
fn local_use(x: String) -> i64 { let v = [x]; v.len() }
fn width(x: String) -> i64 { x.len() }
fn main() { println(dup(\"a\".to_string()).len()); println(local_use(\"b\".to_string())); println(width(\"c\".to_string())); }
";
    let out = migrate(src);
    assert!(out.contains("fn dup(x: own String)"), "{out}");
    assert!(out.contains("fn local_use(x: own String)"), "{out}");
    assert!(out.contains("fn width(x: String)"), "{out}");
}
