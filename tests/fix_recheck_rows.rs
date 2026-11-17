//! A machine fix must leave a file that checks clean: a fix that trades one
//! refusal for another is not machine-applicable (CLAUDE.md, Writing Kāra).
//! Each case is a fix the catalog probe (2026-11-16) found failing its
//! re-check. Where no mechanical edit is right, the diagnostic stays and
//! `karac fix` leaves the file alone.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-fix-recheck-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn check_stderr(path: &PathBuf) -> (bool, String) {
    let out = karac().arg("check").arg(path).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `check` refuses with `expect`; `fix` must not edit the file.
fn refused_and_left_alone(tag: &str, src: &str, expect: &str) {
    let (dir, path) = fixture(tag, src);
    let (ok, err) = check_stderr(&path);
    assert!(!ok, "{tag}: check must reject");
    assert!(err.contains(expect), "{tag}: expected `{expect}` in: {err}");
    karac().arg("fix").arg(&path).output().unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, src, "{tag}: fix must not edit the file");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `check` refuses with `expect`; one `fix` pass writes `edit` and the file
/// then checks clean.
fn refused_then_fixed(tag: &str, src: &str, expect: &str, edit: &str) {
    let (dir, path) = fixture(tag, src);
    let (ok, err) = check_stderr(&path);
    assert!(!ok, "{tag}: check must reject");
    assert!(err.contains(expect), "{tag}: expected `{expect}` in: {err}");
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    let (ok, err) = check_stderr(&path);
    assert!(
        ok,
        "{tag}: check must pass after fix; source:\n{fixed}\n{err}"
    );
    assert!(
        fixed.contains(edit),
        "{tag}: fix must write `{edit}`: {fixed}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B-2026-11-16-291: dropping the `ref` from `-> ref Holder` leaves a value
/// that still borrows a local, so E0213 carries no fix when the returned
/// type holds a borrow.
#[test]
fn fresh_ref_return_holding_a_borrow_gets_no_fix() {
    refused_and_left_alone(
        "e0213-holder",
        "struct Holder { r: ref String }\n\
         fn bad() -> ref Holder {\n\
         let local = String.from(\"x\");\n\
         Holder { r: local }\n\
         }\n\
         fn main() { let h = bad(); println(h.r.len()); }\n",
        "the value holds a borrow itself",
    );
}

/// B-2026-11-16-291: the borrow can sit one struct further in, or in a tuple.
#[test]
fn fresh_ref_return_holding_a_nested_borrow_gets_no_fix() {
    refused_and_left_alone(
        "e0213-nested",
        "struct In { r: ref String }\n\
         struct Out { i: In }\n\
         fn bad(s: String) -> ref Out {\n\
         Out { i: In { r: s } }\n\
         }\n\
         fn main() { let s = \"a\".to_string(); let o = bad(s); println(o.i.r.len()); }\n",
        "the value holds a borrow itself",
    );
}

/// B-2026-11-16-291 control: an owned value still gets the `ref`-dropping fix.
#[test]
fn fresh_ref_return_of_an_owned_value_is_still_fixed() {
    refused_then_fixed(
        "e0213-owned",
        "struct P { x: i64 }\n\
         fn mk() -> ref P {\n\
         P { x: 1 }\n\
         }\n\
         fn main() { println(mk().x); }\n",
        "Return the value itself: drop the `ref`",
        "fn mk() -> P",
    );
}

/// B-2026-11-16-293: weakening `mut ref self` to the trait's `self` while the
/// body writes `self.n` only moves the refusal, so E0288 carries no fix.
#[test]
fn stronger_receiver_written_by_the_body_gets_no_fix() {
    refused_and_left_alone(
        "e0288-self-write",
        "trait T { fn look(self) -> i64; }\n\
         struct S { n: i64 }\n\
         impl T for S { fn look(mut ref self) -> i64 { self.n += 1; self.n } }\n\
         fn main() { let mut s = S { n: 1 }; println(s.look()); }\n",
        "The body writes through `self`",
    );
}

/// B-2026-11-16-293: a mutating builtin on a field counts as a write.
#[test]
fn stronger_receiver_pushed_through_gets_no_fix() {
    refused_and_left_alone(
        "e0288-self-push",
        "trait T { fn look(self) -> i64; }\n\
         struct S { v: Vec[i64] }\n\
         impl T for S { fn look(mut ref self) -> i64 { self.v.push(1); self.v.len() } }\n\
         fn main() { let mut s = S { v: [] }; println(s.look()); }\n",
        "The body writes through `self`",
    );
}

/// B-2026-11-16-293: the same for a `mut ref` parameter the body writes.
#[test]
fn stronger_parameter_written_by_the_body_gets_no_fix() {
    refused_and_left_alone(
        "e0288-param-write",
        "trait T { fn put(self, v: Vec[i64]) -> i64; }\n\
         struct S { n: i64 }\n\
         impl T for S { fn put(self, v: mut ref Vec[i64]) -> i64 { v.push(1); v.len() } }\n\
         fn main() { let s = S { n: 1 }; let mut v: Vec[i64] = []; println(s.put(mut v)); }\n",
        "takes 'v' as `mut ref`",
    );
}

/// B-2026-11-16-293 control: a receiver the body only reads is still fixed.
#[test]
fn stronger_receiver_only_read_is_still_fixed() {
    refused_then_fixed(
        "e0288-self-read",
        "trait T { fn look(self) -> i64; }\n\
         struct S { n: i64 }\n\
         impl T for S { fn look(mut ref self) -> i64 { self.n } }\n\
         fn main() { let s = S { n: 1 }; println(s.look()); }\n",
        "Declare `self`",
        "fn look(self) -> i64",
    );
}
