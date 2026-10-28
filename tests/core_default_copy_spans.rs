//! A trait default body copied into several impls keeps the trait's spans in
//! every copy, so the span-keyed expression types hold only the last impl's.
//! The ownership checks read each copy's own node types there
//! (B-2026-10-28-9): `v[0]` moves a `String` out of its `Vec` in the
//! `Pick[String]` copy whichever impl comes last.

use std::process::Command;

const TRAIT: &str = "trait Pick[T] {
    fn all(ref self) -> Vec[T];
    fn first(ref self) -> T {
        let v = self.all();
        v[0]
    }
}
";

const WORDS: &str = "struct Words {}
impl Pick[String] for Words {
    fn all(ref self) -> Vec[String] { return [\"a\", \"b\"]; }
}
";

const NUMS: &str = "struct Nums {}
impl Pick[i64] for Nums {
    fn all(ref self) -> Vec[i64] { return [1, 2]; }
}
";

const BYTES: &str = "struct Bytes {}
impl Pick[u8] for Bytes {
    fn all(ref self) -> Vec[u8] { return [1, 2]; }
}
";

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-copyspans-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_refused(name: &str, src: &str) {
    let (ok, err) = check(name, src);
    assert!(!ok, "check must refuse:\n{src}");
    assert!(
        err.contains("main.kara:5:9: cannot move a non-`Copy` value out of a collection element"),
        "expected the move out of `v[0]` in:\n{err}"
    );
}

#[test]
fn default_copy_spans_string_impl_before_copy_impl_refused() {
    let main = "fn main() {\n    println(Nums {}.first());\n    println(Words {}.first());\n}\n";
    assert_refused("wn", &format!("{TRAIT}\n{WORDS}\n{NUMS}\n{main}"));
}

#[test]
fn default_copy_spans_string_impl_after_copy_impl_refused() {
    let main = "fn main() {\n    println(Nums {}.first());\n    println(Words {}.first());\n}\n";
    assert_refused("nw", &format!("{TRAIT}\n{NUMS}\n{WORDS}\n{main}"));
}

/// Two `Copy` instantiations of the same body stay accepted.
#[test]
fn default_copy_spans_copy_impls_accepted() {
    let main = "fn main() {\n    println(Nums {}.first());\n    println(Bytes {}.first());\n}\n";
    let (ok, err) = check("nb", &format!("{TRAIT}\n{NUMS}\n{BYTES}\n{main}"));
    assert!(ok, "check must accept:\n{err}");
}
