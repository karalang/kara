//! B-2026-11-25-48: the library's `where T: Clone` and `where T: PartialEq`
//! bounds on `resize`, `fill`, `Vec.filled`, `extend_from_slice`, the
//! repeat literal `[v; n]` and `dedup` were never checked at the call, so a
//! struct with no `Clone`, an unbounded type parameter and a `Mutex` each
//! got field-by-field copies (two independent mutexes from one), and
//! `dedup` compared a struct with no `PartialEq`. Each is refused now,
//! a generic caller needs the bound itself, and every element type that
//! copies or clones stays accepted.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-where-bounds-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const S: &str = "struct S { s: String }\n";

#[test]
fn where_bounds_unmet_clone_is_refused() {
    for (name, body, want) in [
        (
            "resize",
            "fn main() { let mut v: Vec[S] = Vec.new(); v.resize(2, S { s: \"a\".to_string() }); println(v.len()); }",
            "`Vec.resize` copies its value into each new slot, and 'S' has no `.clone()`: add `#[derive(Clone)]` to it",
        ),
        (
            "filled",
            "fn main() { let v: Vec[S] = Vec.filled(2, S { s: \"a\".to_string() }); println(v.len()); }",
            "`Vec.filled` copies its value into each new slot, and 'S' has no `.clone()`",
        ),
        (
            "filled_inferred",
            "fn main() { let v = Vec.filled(2, S { s: \"a\".to_string() }); println(v.len()); }",
            "`Vec.filled` copies its value into each new slot, and 'S' has no `.clone()`",
        ),
        (
            "fill",
            "fn main() { let mut v = [S { s: \"a\".to_string() }]; v.fill(S { s: \"b\".to_string() }); println(v.len()); }",
            "`fill` copies its value into each new slot, and 'S' has no `.clone()`",
        ),
        (
            "extend_from_slice",
            "fn main() { let v = [S { s: \"a\".to_string() }]; let mut w: Vec[S] = Vec.new(); w.extend_from_slice(v.as_slice()); println(w.len()); }",
            "`extend_from_slice` copies its value into each new slot, and 'S' has no `.clone()`",
        ),
        (
            "array_repeat",
            "fn main() { let a = [S { s: \"a\".to_string() }; 3]; println(a.len()); }",
            "`[v; n]` copies its value into each new slot, and 'S' has no `.clone()`",
        ),
        (
            "mutex",
            "import std.sync.Mutex;\nfn main() { let mut v: Vec[Mutex[i64]] = Vec.new(); v.resize(2, Mutex.new(1)); println(v.len()); }",
            "`Vec.resize` copies its value into each new slot, and 'Mutex[i64]' does not implement `Clone`",
        ),
        (
            "generic_resize",
            "fn dup[T](x: own T) -> Vec[T] { let mut v: Vec[T] = Vec.new(); v.resize(2, x); v }\nfn main() { println(dup(1).len()); }",
            "the type parameter `T` has no `Clone` bound: declare it `T: Clone`",
        ),
        (
            "generic_filled",
            "fn dup[T](x: own T) -> Vec[T] { Vec.filled(2, x) }\nfn main() { println(dup(1).len()); }",
            "the type parameter `T` has no `Clone` bound",
        ),
    ] {
        let (ok, out) = check(name, &format!("{S}{body}\n"));
        assert!(!ok, "{name}: check accepted an unmet `Clone` bound");
        assert!(out.contains(want), "{name}: {out}");
    }
}

#[test]
fn where_bounds_unmet_partial_eq_is_refused() {
    let (ok, out) = check(
        "dedup",
        "struct K { k: i64 }\nfn main() { let mut v = [K { k: 1 }, K { k: 1 }]; v.dedup(); println(v.len()); }\n",
    );
    assert!(!ok, "check accepted `dedup` on a type with no PartialEq");
    assert!(
        out.contains("`dedup` compares elements with `==`, and 'K' does not implement PartialEq"),
        "{out}"
    );
    let (ok, out) = check(
        "dedup_generic",
        "fn f[T](v: mut ref Vec[T]) { v.dedup(); }\nfn main() { let mut v = [1, 1]; f(mut v); println(v.len()); }\n",
    );
    assert!(!ok, "check accepted `dedup` over an unbounded `T`");
    assert!(
        out.contains("the type parameter `T` has no `PartialEq` bound"),
        "{out}"
    );
}

#[test]
fn where_bounds_met_stay_accepted() {
    for (name, src) in [
        (
            "derived_clone",
            "#[derive(Clone)]\nstruct S { s: String }\nfn main() { let mut v: Vec[S] = Vec.new(); v.resize(2, S { s: \"a\".to_string() }); v.fill(S { s: \"b\".to_string() }); let w = Vec.filled(2, S { s: \"c\".to_string() }); v.extend_from_slice(w.as_slice()); let a = [S { s: \"d\".to_string() }; 2]; println(f\"{v.len()} {a.len()}\"); }\n",
        ),
        (
            "copy_struct",
            "#[derive(Copy, Clone)]\nstruct P { x: i64 }\nfn main() { let a = [P { x: 1 }; 3]; let v = Vec.filled(2, P { x: 2 }); println(f\"{a.len()} {v.len()}\"); }\n",
        ),
        (
            "bounded_generic",
            "fn dup[T: Clone](x: own T) -> Vec[T] { let mut v: Vec[T] = Vec.new(); v.resize(2, x.clone()); v.fill(x.clone()); let w = Vec.filled(2, x); v.extend_from_slice(w.as_slice()); v }\nfn main() { println(dup(\"a\".to_string()).len()); }\n",
        ),
        (
            "copy_bounded_generic",
            "fn dup[T: Copy](x: T) -> Vec[T] { Vec.filled(2, x) }\nfn main() { println(dup(1).len()); }\n",
        ),
        (
            "library_values",
            "fn main() { let a = Vec.filled(2, \"s\".to_string()); let b = Vec.filled(2, Vec.filled(2, 0)); let c: Vec[Option[String]] = Vec.filled(2, None); let d = [(1, \"x\".to_string()); 2]; let mut e: Vec[String] = Vec.new(); e.resize(3, \"z\".to_string()); println(f\"{a.len()} {b.len()} {c.len()} {d.len()} {e.len()}\"); }\n",
        ),
        (
            "views",
            "struct S { s: String }\nfn main() { let s = S { s: \"a\".to_string() }; let mut v: Vec[ref S] = Vec.new(); let r = ref s; v.resize(2, r); println(v.len()); }\n",
        ),
        (
            "dedup_eq",
            "#[derive(PartialEq)]\nstruct K { k: i64 }\nfn f[T: PartialEq](v: mut ref Vec[T]) { v.dedup(); }\nfn main() { let mut v = [K { k: 1 }, K { k: 1 }]; v.dedup(); let mut w = [1, 1, 2]; f(mut w); println(f\"{v.len()} {w.len()}\"); }\n",
        ),
    ] {
        let (ok, out) = check(name, src);
        assert!(ok, "{name}: {out}");
    }
}
