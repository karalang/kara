//! `self` in a generic `impl[R] Wrap[R]` is typed by the bare name `Wrap`
//! (the checker erases a generic impl target's arguments), so handing it to
//! `fn helper[R](w: ref Wrap[R])` was refused "expected 'Wrap[R]', found
//! 'Wrap'", and a method call on it left the callee's `R` unsolved, which
//! the MIR builder refused as a call "with no resolved callee" inside a
//! library `impl[R] BufReader[R]` calling the native `read` on `self`
//! (B-2026-11-11-62). As a generic call's argument and as a method's
//! receiver it is now the impl's `Wrap[R]`.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn runs_to(tag: &str, src: &str, want: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-generic-impl-self-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} {lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// The row's repro.
#[test]
fn generic_impl_self_passes_to_a_generic_helper() {
    runs_to(
        "wrap",
        "struct Wrap[R] { x: Option[R] }\n\
         impl[R] Wrap[R] {\n    \
             fn tag(ref self) -> i64 { 1 }\n    \
             fn twice(ref self) -> i64 { helper(self) + self.tag() }\n\
         }\n\
         fn helper[R](w: ref Wrap[R]) -> i64 { w.tag() }\n\
         fn main() { let w: Wrap[i64] = Wrap { x: None }; println(f\"{w.twice()}\"); }\n",
        "2\n",
    );
}

/// Two parameters, a `mut ref` helper that writes through `self`, a helper
/// whose parameters are named apart from the impl's, and one returning a
/// borrow of a field typed by the impl's parameter, at two instances.
#[test]
fn generic_impl_self_reaches_helpers_at_each_instance() {
    runs_to(
        "pair",
        "struct Pair[K, V] { k: K, v: V, n: i64 }\n\
         impl[K, V] Pair[K, V] {\n    \
             fn size(ref self) -> i64 { self.n }\n    \
             fn twice(ref self) -> i64 { helper(self) + count2(self) }\n    \
             fn grow(mut ref self) { bump(mut self); }\n    \
             fn first(ref self) -> ref K { key_of(self) }\n\
         }\n\
         fn helper[K, V](p: ref Pair[K, V]) -> i64 { p.size() * 10 }\n\
         fn count2[A, B](p: ref Pair[A, B]) -> i64 { p.n + 1 }\n\
         fn bump[K, V](p: mut ref Pair[K, V]) { p.n += 5; }\n\
         fn key_of[K, V](p: ref Pair[K, V]) -> ref K { ref p.k }\n\
         fn main() {\n    \
             let mut p: Pair[String, i64] = Pair { k: \"a\".to_string(), v: 3, n: 2 };\n    \
             println(f\"{p.twice()}\");\n    \
             p.grow();\n    \
             println(f\"{p.twice()} {p.first()}\");\n    \
             let mut q: Pair[i32, String] = Pair { k: 7, v: \"x\".to_string(), n: 1 };\n    \
             q.grow();\n    \
             println(f\"{q.twice()} {q.first()}\");\n\
         }\n",
        "23\n78 a\n67 7\n",
    );
}
