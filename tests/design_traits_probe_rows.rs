//! §9 trait rows the design-traits probe filed, each checked on every backend.
//!
//! - B-2026-11-06-24: `#[derive(Copy)]` together with `impl Drop` is refused
//!   at the impl, naming both traits (design.md §9 "`Drop` and `Copy` are
//!   exclusive"); the drop body ran once per copy.
//! - B-2026-11-06-25: `#[derive(Default)]` on a generic struct is
//!   `impl[T: Default]`, and a call whose type argument only the result names
//!   is held to that bound; `impl PartialEq[i64] for Meters` makes
//!   `meters == 3`; a manual `Hash` impl feeds a field to its `H: Hasher`.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, cmd: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-design-traits-{tag}-{}-{}.kara",
        cmd.trim_start_matches('_'),
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    }
}

/// `src` checks clean and prints `want` on every backend.
fn runs(tag: &str, src: &str, want: &str) {
    let out = karac(tag, "check", src);
    assert!(
        out.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for b in backends() {
        let out = karac(tag, b, src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} on {b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{tag} on {b}");
    }
}

/// `src` is refused by `karac check` with `err` in its diagnostics.
fn refused(tag: &str, src: &str, err: &str) {
    let out = karac(tag, "check", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag} was accepted");
    assert!(stderr.contains(err), "{tag}: {stderr}");
}

#[test]
fn design_traits_derived_copy_with_drop_is_refused() {
    let src = "#[derive(Copy, Clone)]\nstruct Pt { x: i64 }\nimpl Drop for Pt { fn drop(mut ref self) { println(f\"drop {self.x}\"); } }\nfn take(p: own Pt) { println(f\"took {p.x}\"); }\nfn main() { let a = Pt { x: 1 }; let b = a; take(a); take(b); println(\"end\"); }\n";
    refused(
        "copy-drop",
        src,
        "`Pt` derives `Copy` and implements `Drop`, and a type cannot be both",
    );
    let src = "#[derive(Copy, Clone)]\nenum Tok { A, B }\nimpl Drop for Tok { fn drop(mut ref self) { println(\"drop\"); } }\nfn main() { let t = Tok.A; let u = t; println(\"end\"); }\n";
    refused(
        "copy-drop-enum",
        src,
        "`Tok` derives `Copy` and implements `Drop`",
    );
}

#[test]
fn design_traits_derived_clone_with_drop_still_runs() {
    let src = "#[derive(Clone)]\nstruct Pt { x: i64 }\nimpl Drop for Pt { fn drop(mut ref self) { println(f\"drop {self.x}\"); } }\nfn main() { let a = Pt { x: 1 }; let b = a.clone(); println(f\"{b.x}\"); }\n";
    runs("clone-drop", src, "1\ndrop 1\ndrop 1\n");
}

#[test]
fn design_traits_generic_derive_default_bounds_each_parameter() {
    let src = "#[derive(Default)]\nstruct Pair[T] { a: T, n: i64, v: Vec[T], tag: (T, bool) }\n#[derive(Default)]\nstruct Outer { p: Pair[i64], name: String }\n#[derive(Default)]\nenum Mode[T] { #[default] Idle, Busy(T) }\nfn main() {\n    let q: Pair[i64] = Pair.default();\n    println(f\"{q.a} {q.n} {q.v.len()} {q.tag.1}\");\n    let s: Pair[String] = Pair.default();\n    println(f\"[{s.a}]\");\n    let o: Outer = Outer.default();\n    println(f\"{o.p.a} [{o.name}]\");\n    let m: Mode[i64] = Mode.default();\n    match m { Mode.Idle => println(\"idle\"), Mode.Busy(n) => println(n) }\n}\n";
    runs("default", src, "0 0 0 false\n[]\n0 []\nidle\n");
}

#[test]
fn design_traits_result_named_impl_bound_is_checked() {
    let src = "struct Foo { x: i64 }\n#[derive(Default)]\nstruct Pair[T] { a: T }\nfn main() { let q: Pair[Foo] = Pair.default(); println(q.a.x); }\n";
    refused(
        "default-bound",
        src,
        "trait bound `T: Default` is not satisfied; `Foo` does not implement `Default`",
    );
    let src = "trait Zero { fn zero() -> Self; }\nimpl Zero for i64 { fn zero() -> i64 { 0 } }\nstruct Foo { x: i64 }\nstruct Pair[T] { a: T }\nimpl[T: Zero] Pair[T] { fn make() -> Pair[T] { Pair { a: T.zero() } } }\nfn main() { let r: Pair[i64] = Pair.make(); println(r.a); let q: Pair[Foo] = Pair.make(); println(q.a.x); }\n";
    refused(
        "zero-bound",
        src,
        "trait bound `T: Zero` is not satisfied; `Foo` does not implement `Zero`",
    );
}

#[test]
fn design_traits_partial_eq_takes_its_rhs() {
    let src = "struct Meters { v: i64 }\nimpl PartialEq[i64] for Meters { fn eq(self, o: ref i64) -> bool { self.v == *o } }\nimpl PartialEq for Meters { fn eq(self, o: ref Meters) -> bool { self.v / 10 == o.v / 10 } }\nfn main() {\n    let a = Meters { v: 3 };\n    println(a == 3);\n    println(a != 3);\n    println(Meters { v: 4 } == 3);\n    println(a == Meters { v: 7 });\n    println(a == Meters { v: 17 });\n    let n: i64 = 3;\n    println(a == n);\n}\n";
    runs(
        "partial-eq-rhs",
        src,
        "true\nfalse\nfalse\ntrue\nfalse\ntrue\n",
    );
    // An `impl PartialEq[i64]` alone is not the type's own equality.
    let src = "struct Meters { v: i64 }\nimpl PartialEq[i64] for Meters { fn eq(self, o: ref i64) -> bool { self.v == *o } }\nfn main() { println(Meters { v: 3 } == Meters { v: 3 }); }\n";
    refused(
        "partial-eq-rhs-only",
        src,
        "type 'Meters' does not implement PartialEq",
    );
}

#[test]
fn design_traits_manual_hash_feeds_a_field() {
    let src = "struct Ci { s: String }\nimpl PartialEq for Ci { fn eq(self, o: ref Ci) -> bool { self.s.to_lowercase() == o.s.to_lowercase() } }\nimpl Eq for Ci {}\nimpl Hash for Ci { fn hash[H: Hasher](self, hasher: mut ref H) { self.s.to_lowercase().hash(hasher); } }\nfn main() { let mut s: Set[Ci] = Set.new(); s.insert(Ci { s: \"Ab\" }); s.insert(Ci { s: \"aB\" }); s.insert(Ci { s: \"c\" }); println(s.len()); }\n";
    runs("hash-field", src, "2\n");
    // A string fed to a user `Hasher`, whose `write` borrows its bytes.
    let src = "struct Sum { n: u64 }\nimpl Hasher for Sum {\n    fn write(mut ref self, bytes: ref Slice[u8]) { for b in bytes { self.n = self.n * 31 + *b; } }\n    fn finish(ref self) -> u64 { self.n }\n}\nfn main() { let mut h = Sum { n: 0 }; \"ab\".hash(mut h); println(h.finish()); }\n";
    runs("hash-str", src, "96510\n");
}
