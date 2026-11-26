//! A task's or `par` branch's result that holds a borrow through a struct
//! field (`Hd { r: ref v }`) borrows `v`, as a `ref` or view result does
//! (core-semantics.md §5.2, §5.4; B-2026-11-19-133): the task reads its
//! `Copy` capture through a borrow instead of copying it in. And a `ref`
//! local takes no plain `Copy` value: `p = x` is E0200 with the `ref x`
//! fix, where it used to pass check and stop every lane with invalid MIR
//! (B-2026-11-19-134).

use karac::{check_source, run_playground};

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

fn refused(src: &str, says: &str) {
    let errs = errors(src);
    assert!(errs.iter().any(|m| m.contains(says)), "{src}\n{errs:#?}");
}

fn runs(src: &str, want: &str) {
    let errs = errors(src);
    assert!(errs.is_empty(), "{src}\n{errs:#?}");
    let r = run_playground(src);
    assert!(r.ok, "{src}\n{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), want, "{src}");
}

const HD: &str = "import std.sync.{TaskGroup}; struct Hd { r: ref i64 }\n";

#[test]
fn joined_ref_field_outliving_its_place_is_refused() {
    refused(
        &format!(
            "{HD}fn main() {{ let e = 7; let mut p: ref i64 = ref e; \
             {{ let v = 5; let g = TaskGroup.new(); let h = g.spawn(|| Hd {{ r: ref v }}); \
             p = h.join().r; }} println(*p); }}\n"
        ),
        "`v` while `p` still holds a borrow of it",
    );
}

#[test]
fn par_ref_field_outliving_its_place_is_refused() {
    refused(
        &format!(
            "{HD}fn main() {{ let e = 7; let mut p: ref i64 = ref e; \
             {{ let v = 5; let (a, b) = par {{ Hd {{ r: ref v }}, 1 }}; p = a.r; }} println(*p); }}\n"
        ),
        "`v` while `p` still holds a borrow of it",
    );
}

#[test]
fn ref_field_results_used_in_scope_run() {
    runs(
        &format!(
            "{HD}fn main() {{ let v = 5; let g = TaskGroup.new(); \
             let h = g.spawn(|| Hd {{ r: ref v }}); let p = h.join().r; println(*p); \
             let (a, b) = par {{ Hd {{ r: ref v }}, 1 }}; println(*a.r + b); }}\n"
        ),
        "5\n6\n",
    );
}

#[test]
fn a_copy_place_into_a_ref_local_is_refused_with_the_borrow_fix() {
    let src = "fn main() { let e = 7; let mut p: ref i64 = ref e; let x: i64 = 5; p = x; println(*p); }\n";
    let d: Vec<_> = check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(
        d.iter()
            .any(|d| d.message.contains("write `p = ref x` to borrow it")
                && d.fixes.iter().any(|f| f.replacement == "ref x")),
        "{d:#?}"
    );
    runs(
        "fn main() { let e = 7; let mut p: ref i64 = ref e; let x: i64 = 5; p = ref x; println(*p); }\n",
        "5\n",
    );
}

#[test]
fn a_copy_temporary_into_a_ref_local_is_refused() {
    refused(
        "fn mk() -> i64 { 5 }\nfn main() { let e = 7; let mut p: ref i64 = ref e; p = mk(); println(*p); }\n",
        "so it cannot hold this `i64` value, which is no borrow",
    );
}
