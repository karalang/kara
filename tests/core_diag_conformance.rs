//! The diagnostics side of core-semantics §3–§7 (moves, borrows, partial
//! moves, drops): each refusal sits on a span, names both places it is
//! about, and carries a machine fix where the doc implies one. Each test
//! here pins one gap the 2026-11-17 conformance pass found. B-2026-11-17-85
//! is the ledger row.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn write(name: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-conf-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

fn check(name: &str, src: &str) -> String {
    let path = write(name, src);
    let (_, err) = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    err
}

/// Applies `karac fix` and returns the fixed text and whether it re-checks clean.
fn fix(name: &str, src: &str) -> (String, bool, String) {
    let path = write(name, src);
    let p = path.to_str().unwrap();
    karac(&["fix", p]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let (ok, err) = karac(&["check", p]);
    let _ = std::fs::remove_file(&path);
    (fixed, ok, err)
}

/// §5.6: a read while a `mut ref` local borrows the place names where the
/// borrow is bound and the use that keeps it live.
#[test]
fn conf_read_while_mut_ref_live_names_both_sites() {
    let err = check(
        "read-live",
        "fn main() {
    let mut v: Vec[i64] = [1, 2];
    let r = mut ref v;
    println(f\"{v.len()}\");
    r.push(3);
}
",
    );
    assert!(
        err.contains(
            "while `mut ref r` borrows `v`: `r` is used after this, at 5:5 (`r` is bound at 3:9"
        ),
        "{err}"
    );
}

/// §5.6: a write while a `ref` local borrows the place, the same way.
#[test]
fn conf_write_while_ref_live_names_both_sites() {
    let err = check(
        "write-live",
        "fn main() {
    let mut v: Vec[String] = [\"a\".to_string()];
    let r = ref v[0];
    v.push(\"b\".to_string());
    println(r);
}
",
    );
    assert!(
        err.contains(
            "while `ref r` borrows `v[..]`: `r` is used after this, at 5:13 (`r` is bound at 3:9"
        ),
        "{err}"
    );
}

/// §3.7: a move out of a field of a bare parameter names the place and the
/// parameter it is reached through.
#[test]
fn conf_move_out_of_bare_param_field_names_the_place() {
    let err = check(
        "move-out",
        "struct User { name: String }
fn name_of(u: User) -> String { u.name }
fn main() {
    let u = User { name: \"x\".to_string() };
    println(name_of(u));
}
",
    );
    assert!(
        err.contains("out of a borrowed place: `u.name` is reached through `u`, a bare parameter"),
        "{err}"
    );
}

/// §4.6: a binding of a `mut ref` scrutinee's part is a `mut ref`, and
/// moving it is §3.7's error with the `.clone()` fix, not a bare mismatch.
#[test]
fn conf_mut_ref_scrutinee_binding_move_says_why() {
    let src = "enum V { Num(i64), Text(String) }
fn keep(s: own String) { println(s); }
fn f(v: mut ref V) {
    match v {
        V.Text(s) => keep(s),
        V.Num(_) => {}
    }
}
fn main() {
    let mut v = V.Text(\"x\".to_string());
    f(mut v);
}
";
    let err = check("mutref-scrut", src);
    assert!(
        err.contains("found 'mut ref String': a borrowed value cannot be moved out of what it borrows, and `s` (bound at 5:16) borrows its value"),
        "{err}"
    );
    let (fixed, ok, after) = fix("mutref-scrut", src);
    assert!(fixed.contains("keep(s.clone())"), "{fixed}");
    assert!(ok, "{after}");
}

/// §4.6 / §3.7: a `let` destructure of a `shared` value moved on names the
/// binding and where it borrows.
#[test]
fn conf_shared_part_moved_out_is_one_error() {
    let err = check(
        "shared-part",
        "shared struct H { xs: Vec[i64], n: i64 }
fn keep(v: own Vec[i64]) -> i64 { v.len() }
fn main() {
    let h = H { xs: [1, 2], n: 3 };
    let H { xs, n } = h;
    println(keep(xs) + n);
}
",
    );
    assert!(
        err.contains("and `xs` (bound at 5:13) borrows its value"),
        "{err}"
    );
}

/// §4.7: an impl that takes `own String` where the trait borrows is fixed by
/// dropping `own`, never by `own ref String`.
#[test]
fn conf_impl_own_param_fix_drops_own() {
    let src = "trait Sink { fn put(self, s: String); }
struct S {}
impl Sink for S {
    fn put(self, s: own String) { println(s); }
}
fn main() {
    let s = S {};
    s.put(\"x\".to_string());
}
";
    let err = check("impl-own", src);
    assert!(
        err.contains("takes 's' by value (`own`), but 'Sink.put' borrows it"),
        "{err}"
    );
    let (fixed, ok, after) = fix("impl-own", src);
    assert!(fixed.contains("fn put(self, s: String)"), "{fixed}");
    assert!(ok, "{after}");
}

/// §4.1: `own ref T` is not a parameter mode.
#[test]
fn conf_own_ref_param_is_refused() {
    let err = check(
        "own-ref",
        "fn show(s: own ref String) { println(s); }
fn main() { show(\"x\".to_string()); }
",
    );
    assert!(
        err.contains("`own` and `ref` together are not a parameter mode"),
        "{err}"
    );
}

/// §7.11: a `Drop` body with the wrong receiver is refused at the receiver,
/// and the fix writes `mut ref self`.
#[test]
fn conf_drop_receiver_is_fixed_at_the_receiver() {
    let src = "struct D { n: i64 }
impl Drop for D { fn drop(self) { println(\"d\"); } }
fn main() { let d = D { n: 1 }; }
";
    let err = check("drop-recv", src);
    assert!(err.contains("2:27"), "{err}");
    let (fixed, ok, after) = fix("drop-recv", src);
    assert!(fixed.contains("fn drop(mut ref self)"), "{fixed}");
    assert!(ok, "{after}");
}

/// §5.4: returning a view of a local the function owns is the result
/// outliving what it borrows, not a drop conflict.
#[test]
fn conf_returned_view_of_owned_local_is_e0509() {
    let err = check(
        "ret-view",
        "fn mk(s: Str) -> Str {
    let t = s.to_string();
    t.as_str()
}
fn main() { println(mk(\"x\")); }
",
    );
    assert!(
        err.contains("the result borrows `t`, which this function owns"),
        "{err}"
    );
    assert!(!err.contains("cannot drop"), "{err}");
}

/// §5.4: a method with a `self` receiver that returns its argument names the
/// argument.
#[test]
fn conf_receiver_rule_names_the_lending_parameter() {
    let err = check(
        "recv-lends",
        "struct Pool { tag: String }
impl Pool {
    fn pick(self, k: ref String) -> ref String { k }
}
fn main() {
    let pool = Pool { tag: \"t\".to_string() };
    let k = \"q\".to_string();
    println(pool.pick(k));
}
",
    );
    assert!(
        err.contains("reached through the parameter `k`, not through `self`"),
        "{err}"
    );
}
