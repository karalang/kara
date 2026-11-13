//! B-2026-11-13-31 (core semantics §5.4): a method with a `self` or `mut ref self`
//! receiver returns a borrow of `self` only, whatever the result's type, so
//! a view-typed argument (`Str`, `Option[ref T]`, a tuple holding a `ref`) is
//! no source for it. Before the fix only `ref T` and `Slice[T]` arguments
//! were held to the rule, and `fn pick(self, other: Str) -> Str { other }`
//! let a caller keep the result past the argument and read freed memory.

use std::path::PathBuf;
use std::process::Command;

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-receiver-views-{tag}-{}-{}",
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

fn check(tag: &str, src: &str) -> (bool, String) {
    let (dir, path) = fixture(tag, src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const RULE: &str = "returns a borrow of `self` only";

fn refused(tag: &str, method: &str) {
    let src = format!(
        "struct C {{ s: String }}\n\
         impl C {{ {method} }}\n\
         fn main() {{}}\n"
    );
    let (ok, err) = check(tag, &src);
    assert!(!ok, "{tag}: check must refuse:\n{src}");
    assert!(err.contains(RULE), "{tag}: expected `{RULE}` in: {err}");
}

fn accepted(tag: &str, method: &str) {
    let src = format!(
        "struct C {{ s: String }}\n\
         impl C {{ {method} }}\n\
         fn main() {{}}\n"
    );
    let (ok, err) = check(tag, &src);
    assert!(ok, "{tag}: check must accept:\n{src}\n{err}");
}

#[test]
fn receiver_rule_refuses_a_str_argument_returned() {
    refused("str", "fn pick(self, other: Str) -> Str { other }");
    refused(
        "str-mut",
        "fn pick(mut ref self, other: Str) -> Str { other }",
    );
    refused(
        "str-return-stmt",
        "fn pick(self, other: Str) -> Str { return other; }",
    );
    refused(
        "str-branch",
        "fn pick(self, other: Str, b: bool) -> Str { if b { self.s.as_str() } else { other } }",
    );
    refused(
        "str-of-ref-string",
        "fn pick(self, other: ref String) -> Str { other.as_str() }",
    );
}

#[test]
fn receiver_rule_refuses_an_option_or_tuple_argument_returned() {
    refused(
        "option-ref",
        "fn pick(self, o: Option[ref String]) -> Option[ref String] { o }",
    );
    refused(
        "option-wrapped",
        "fn pick(self, o: Str) -> Option[Str] { Some(o) }",
    );
    refused(
        "tuple-ref",
        "fn pick(self, t: (ref String, i64)) -> ref String { t.0 }",
    );
    refused(
        "result-str",
        "fn pick(self, o: Result[Str, i64]) -> Result[Str, i64] { o }",
    );
    refused(
        "owned-vec-of-str",
        "fn pick(self, o: own Vec[Str]) -> Vec[Str] { o }",
    );
    refused(
        "tuple-built",
        "fn pick(self, o: Str) -> (Str, i64) { (o, 1) }",
    );
}

/// A plain struct with a view field is a view too (`core_is_view`).
#[test]
fn receiver_rule_refuses_a_view_struct_argument_returned() {
    let src = "struct V { s: Str }\n\
        struct C { s: String }\n\
        impl C { fn pick(self, o: own V) -> V { o } }\n\
        fn main() {}\n";
    let (ok, err) = check("view-struct", src);
    assert!(!ok, "check must refuse:\n{src}");
    assert!(err.contains(RULE), "{err}");
}

#[test]
fn receiver_rule_keeps_borrows_of_self_and_owned_results() {
    accepted("self-view", "fn name(self) -> Str { self.s.as_str() }");
    accepted(
        "self-view-with-arg",
        "fn name(self, _other: Str) -> Str { self.s.as_str() }",
    );
    accepted(
        "self-option",
        "fn first(self, _k: Str) -> Option[Str] { Some(self.s.as_str()) }",
    );
    accepted(
        "owned-from-arg",
        "fn joined(self, other: Str) -> String { f\"{self.s}{other}\" }",
    );
    accepted("literal", "fn tag(self, _o: Str) -> Str { \"t\" }");
    // An associated function's result borrows its view parameters.
    accepted("associated", "fn pick(other: Str) -> Str { other }");
}

/// The probe's program: the caller keeps `c.pick(..)` past the `T` whose
/// run-time-built `String` the argument viewed.
#[test]
fn receiver_rule_refuses_the_probe_use_after_free() {
    let src = "struct T { s: String }\n\
        struct C { s: String }\n\
        impl C { fn pick(self, other: Str) -> Str { other } }\n\
        fn main() {\n\
            let c = C { s: \"a\".to_string() };\n\
            let r = { let t = T { s: { let mut b = String.new(); for i in 0..40 { b.push(((97 + i % 26) as u8) as char); } b } }; c.pick(t.s.as_str()) };\n\
            println(f\"r = {r}\");\n\
        }\n";
    let (ok, err) = check("probe", src);
    assert!(!ok, "check must refuse the probe");
    assert!(err.contains(RULE), "{err}");
}
