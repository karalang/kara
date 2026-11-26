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

const DYN_SPAWN: &str = "trait Bump {\n\
     fn bump(ref self) -> i64;\n\
 }\n\
 struct D { s: S }\n\
 impl Bump for D {\n\
     fn bump(ref self) -> i64 { self.s.n }\n\
 }\n\
 fn mkd(s: own S) -> dyn Bump {\n\
     D { s: s }\n\
 }\n\
 fn main() {\n\
     let d = mkd(S { n: 0 });\n\
     let g = TaskGroup.new();\n\
     let a = g.spawn(|| d.bump());\n\
     let b = g.spawn(|| d.bump());\n\
     println(a.join() + b.join());\n\
 }\n";

/// B-2026-11-16-292: `+ CrossTask` on `mkd`'s return type would check the
/// `D` it returns, which holds a `shared` handle, so the fix is withheld.
#[test]
fn cross_task_mark_on_a_return_holding_a_shared_handle_gets_no_fix() {
    refused_and_left_alone(
        "cross-task-dyn-shared",
        &format!("import std.sync.{{TaskGroup}}; shared struct S {{ mut n: i64 }}\n{DYN_SPAWN}"),
        "cannot cross a TaskGroup.spawn task boundary",
    );
}

/// B-2026-11-16-292 control: a returned value that may cross tasks keeps
/// the `+ CrossTask` fix.
#[test]
fn cross_task_mark_on_a_plain_return_is_still_fixed() {
    refused_then_fixed(
        "cross-task-dyn-plain",
        &format!("import std.sync.{{TaskGroup}}; struct S {{ n: i64 }}\n{DYN_SPAWN}"),
        "cannot cross a TaskGroup.spawn task boundary",
        "-> dyn Bump + CrossTask",
    );
}

/// B-2026-11-16-294: an unbounded `T` has no `.clone()`, so the index read
/// gets the borrow advice and no `.clone()` fix.
#[test]
fn index_move_of_an_unbounded_parameter_gets_no_clone_fix() {
    refused_and_left_alone(
        "index-move-unbounded",
        "fn f[T](v: ref Vec[T]) -> T { let a = v[0]; return a; }\n\
         fn main() { println(\"x\"); }\n",
        "No `.clone()` is offered: `T` does not have one",
    );
}

/// B-2026-11-16-294 control: under a `Clone` bound the `.clone()` fix stays.
#[test]
fn index_move_of_a_clone_bounded_parameter_is_still_fixed() {
    refused_then_fixed(
        "index-move-clone-bound",
        "fn f[T: Clone](v: ref Vec[T]) -> T { let a = v[0]; return a; }\n\
         fn main() { println(\"x\"); }\n",
        "take an independent copy",
        "v[0].clone()",
    );
}

/// B-2026-11-16-301: `<` on a struct with no ordering gets the
/// `#[derive(PartialOrd)]` edit, as `==` gets `#[derive(PartialEq)]`.
#[test]
fn ordering_without_partial_ord_is_fixed_by_the_derive() {
    refused_then_fixed(
        "partial-ord-derive",
        "struct S { a: i64 }\n\
         fn main() { let x = S { a: 1 }; let y = S { a: 2 }; if x < y { println(\"lt\"); } }\n",
        "add #[derive(PartialOrd)]",
        "PartialOrd",
    );
}

/// B-2026-11-16-301: a field that does not order makes the derive fail, so
/// no edit is attached.
#[test]
fn ordering_over_an_unordered_field_gets_no_derive_fix() {
    refused_and_left_alone(
        "partial-ord-unordered-field",
        "struct In { a: i64 }\n\
         struct S { i: In }\n\
         fn main() { let x = S { i: In { a: 1 } }; let y = S { i: In { a: 2 } }; if x > y { println(\"gt\"); } }\n",
        "does not implement PartialOrd",
    );
}

/// B-2026-11-16-295: the shared-to-sync migration also routes a read of a
/// wrapped field inside the `par` through its lock (`c.items.len()`,
/// `c.count`), so the migrated file checks clean.
#[test]
fn shared_to_sync_migration_locks_reads_inside_the_par() {
    refused_then_fixed(
        "sync-migration-par-reads",
        "shared struct Counter { val: i64, mut count: i64, mut items: Vec[i64] }\n\
         fn use_a(c: Counter) { }\n\
         fn use_b(c: Counter) { }\n\
         fn main() {\n\
         let c = Counter { val: 0, count: 0, items: [] };\n\
         par {\n\
         use_a(c);\n\
         use_b(c);\n\
         println(c.items.len() + c.count);\n\
         }\n\
         }\n",
        "cannot enter a `par` branch",
        "let items_guard = c.items.lock(); items_guard.value.len()",
    );
}

/// B-2026-11-16-290: in value position a built-in function is no
/// suggestion, since it cannot be used as a value: `let q = prnt;` is no
/// longer rewritten to `print`.
#[test]
fn value_position_did_you_mean_skips_builtin_functions() {
    refused_and_left_alone(
        "dym-value-builtin",
        "fn main() { let q = prnt; }\n",
        "undefined name 'prnt'",
    );
}

/// B-2026-11-16-290 control: in call position the built-in is still the
/// suggestion and the fix.
#[test]
fn call_position_did_you_mean_still_offers_builtin_functions() {
    refused_then_fixed(
        "dym-call-builtin",
        "fn main() { prnt(\"x\"); }\n",
        "did you mean 'print'?",
        "print(\"x\")",
    );
}

/// B-2026-11-16-290: `format(..)` is Rust's and Python's; it gets the
/// f-string spelling, not "did you mean 'forget'".
#[test]
fn format_call_names_the_f_string() {
    refused_and_left_alone(
        "dym-format",
        "fn main() { let s = format(\"{}\", 1); println(s); }\n",
        "Kāra has no `format` function, and an f-string formats",
    );
}
