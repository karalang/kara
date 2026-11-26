//! A task's result holds what its closure holds (core-semantics.md §5.3,
//! §5.4): `g.spawn(|| v.as_slice())` hands back a handle whose `join()` is
//! a view of `v`, and a `par` branch's `ref v` is a reference to `v`, not
//! into the task's own copy. Before, the handle dropped its closure's
//! origins, so the result outlived the viewed value: heap-use-after-free
//! under ASan (B-2026-11-19-95).

use karac::{check_source, run_playground};

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

/// Refused, and the error names the viewed value `v`, not the group.
fn refused(src: &str, names: &str) {
    let errs = errors(src);
    assert!(
        errs.iter().any(
            |m| m.contains(&format!("`{names}` while `r` still holds a borrow of it"))
                || m.contains(&format!("`{names}` while `p` still holds a borrow of it"))
        ),
        "{src}\n{errs:#?}"
    );
}

fn runs(src: &str, want: &str) {
    let errs = errors(src);
    assert!(errs.is_empty(), "{src}\n{errs:#?}");
    let r = run_playground(src);
    assert!(r.ok, "{src}\n{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), want, "{src}");
}

const OUTLIVES: &str =
    "import std.sync.{TaskGroup};\nfn main() { let e = [0]; let mut r: Slice[i64] = e.as_slice(); \
     { let v = [1, 2, 3, 4, 5, 6, 7, 8]; let g = TaskGroup.new(); \
     let h = g.spawn(|| v.as_slice()); r = h.join(); } println(r[0]); }\n";

#[test]
fn joined_slice_outliving_its_vec_is_refused() {
    refused(OUTLIVES, "v");
}

#[test]
fn joined_str_outliving_its_string_is_refused() {
    refused(
        "import std.sync.{TaskGroup};\nfn main() { let mut r: Str = \"k\"; { let s = \"ab\".to_string(); \
         let g = TaskGroup.new(); let h = g.spawn(|| s.as_str()); r = h.join(); } \
         println(r); }\n",
        "s",
    );
}

#[test]
fn generic_task_body_view_outliving_its_string_is_refused() {
    refused(
        "import std.sync.{TaskGroup};\nfn job[T: CrossTask](x: own T) -> T { x }\n\
         fn main() { let mut r: Str = \"k\"; { let s = \"ab\".to_string(); \
         let g = TaskGroup.new(); let h = g.spawn(|| job(s.as_str())); r = h.join(); } \
         println(r); }\n",
        "s",
    );
}

#[test]
fn par_branch_slice_outliving_its_vec_is_refused() {
    refused(
        "fn main() { let e = [0]; let mut r: Slice[i64] = e.as_slice(); \
         { let v = [1, 2, 3, 4, 5, 6, 7, 8]; let (a, b) = par { v.as_slice(), 1 }; r = a; } \
         println(r[0]); }\n",
        "v",
    );
}

#[test]
fn par_branch_ref_of_a_copy_place_borrows_the_place() {
    let src = "fn main() { let e = 0; let mut p: ref i64 = ref e; \
               { let v = 5; let (a, b) = par { ref v, 1 }; p = a; } println(*p); }\n";
    let errs = errors(src);
    assert!(
        errs.iter()
            .any(|m| m.contains("`v` while `p` still holds a borrow of it")),
        "{src}\n{errs:#?}"
    );
}

#[test]
fn views_joined_while_their_source_lives_run() {
    runs(
        "import std.sync.{TaskGroup};\nfn main() { let v = [1, 2, 3]; let r = { let g = TaskGroup.new(); \
         let h = g.spawn(|| v.as_slice()); h.join() }; println(r[0]); println(v.len()); }\n",
        "1\n3\n",
    );
    runs(
        "import std.sync.{TaskGroup};\nfn main() { let s = \"ab\".to_string(); let g = TaskGroup.new(); \
         let h = g.spawn(|| s.as_str()); let r = h.join(); println(r); }\n",
        "ab\n",
    );
    runs(
        "fn main() { let v = 5; let (a, b) = par { ref v, 1 }; println(*a + b); }\n",
        "6\n",
    );
}

#[test]
fn a_task_with_a_copy_capture_and_a_value_result_still_runs() {
    runs(
        "import std.sync.{TaskGroup, TaskHandle};\nfn main() { let g = TaskGroup.new(); let mut hs: Vec[TaskHandle[i64]] = Vec.new(); \
         let mut i = 0; while i < 3 { let c = i; hs.push(g.spawn(|| c * 10)); i = i + 1; } \
         let mut t = 0; for h in hs.into_iter() { t = t + h.join(); } println(t); }\n",
        "30\n",
    );
}

/// A ref-capturing closure erased to a bare generic `F` carries its
/// origins to the result; the literal form waits on B-2026-11-19-72,
/// which captures it by move today.
#[test]
fn erased_ref_capturing_closure_through_generic_f_is_refused() {
    refused(
        "fn ap[F: Fn() -> Slice[i64]](f: F) -> Slice[i64] { f() }\n\
         fn main() { let e = [0]; let mut r: Slice[i64] = e.as_slice(); \
         { let v = [1, 2, 3]; let f: Fn() -> Slice[i64] = || v.as_slice(); r = ap(f); } \
         println(r[0]); }\n",
        "v",
    );
}
