//! The gaps porting apps/service/ldap_server found (B-2026-11-26-60 to
//! -66), each pinned on the interpreter and, where it runs, compiled:
//!
//! - -60: Rust's `F.Eq(..)` pattern gets a fix to Kāra's `F.Eq { .. }`.
//! - -61: `for (t, vs) in ref self.attrs` returning `Some(vs)` was an
//!   E0509 false positive: the loop borrows the caller's collection.
//! - -62: a list literal in a tuple literal's `Slice` slot, inside a call
//!   argument, is lent for the call as a direct argument is; at a `let`
//!   the temporary would dangle, and borrowck refuses it (E0516).
//! - -63: `String.to_ascii_lowercase` / `to_ascii_uppercase`, Rust's.
//! - -64: `.map_err(|_| e)` on an `Option` gets a fix to `.ok_or_else(|| e)`
//!   (the edit-distance one was `map_or`, which takes two arguments).
//! - -65: `assert_eq!(a, b, "fmt {}", x)` gets a fix to one f-string message.
//! - -66: `Some` / `Ok` / `Err` named as a value passed the checker and had
//!   no body in MIR; they are refused as any variant constructor is, and
//!   `karac fix` writes the closure, for those and for a program's own.

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-ldap-gaps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn run(tag: &str, mode: &str, src: &str) -> String {
    let (dir, _) = scratch(&format!("{tag}-{mode}"), src);
    let out = karac(&dir, &[mode, "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `karac fix` to a fixpoint, then the fixed text and its run.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let (dir, file) = scratch(&format!("{tag}-fix"), src);
    let out = karac(&dir, &["fix", "main.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let run = karac(&dir, &["__mir-run", "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "fix: {}\n{text}\nrun: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&run.stderr)
    );
    (text, String::from_utf8_lossy(&run.stdout).into_owned())
}

const LENDS: &str = r#"
struct Spec { name: Str, vals: Slice[Str] }

fn attrs(pairs: Slice[(Str, Slice[Str])]) -> String {
    let mut out = String.new();
    for (t, vs) in pairs {
        out.push_str(f"{t}={vs.len()};");
    }
    out
}

fn deep(p: ((Str, Slice[Str]), i64)) -> i64 {
    (p.0).1.len() + p.1
}

fn spec(s: Spec) -> String {
    f"{s.name}:{s.vals.len()}"
}

fn nums(p: (i64, Slice[i64])) -> i64 {
    let mut t = p.0;
    for x in p.1 {
        t += x;
    }
    t
}

fn main() {
    println(attrs([("objectClass", ["top", "domain"]), ("dc", ["kara"]), ("empty", [])]));
    println(deep((("a", ["x", "y"]), 1)));
    println(spec(Spec { name: "s", vals: ["p", "q", "r"] }));
    println(nums((10, [1, 2, 3])));
    let v = [4, 5];
    println(nums((0, v)));
}
"#;

const LENDS_WANT: &str = "objectClass=2;dc=1;empty=0;\n3\ns:3\n16\n9\n";

#[test]
fn interp_list_literal_in_a_tuple_slice_slot_is_lent_for_the_call() {
    assert_eq!(run("lends", "__mir-run", LENDS), LENDS_WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_list_literal_in_a_tuple_slice_slot_is_lent_for_the_call() {
    assert_eq!(run("lends", "__mir-llvm-run", LENDS), LENDS_WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn asan_list_literal_in_a_tuple_slice_slot_is_lent_for_the_call() {
    assert_eq!(run("lends", "__mir-llvm-asan", LENDS), LENDS_WANT);
}

#[test]
fn check_list_literal_in_a_tuple_slice_slot_at_a_let_would_dangle() {
    let src = r#"
fn main() {
    let t: (Str, Slice[Str]) = ("a", ["x"]);
    println(t.1.len());
}
"#;
    let (dir, _) = scratch("let", src);
    let check = karac(&dir, &["check", "main.kara"]);
    let mir = karac(&dir, &["__mir-run", "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&check.stdout).to_string()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success() && text.contains("E0516"), "{text}");
    let mtext = String::from_utf8_lossy(&mir.stderr);
    assert!(!mir.status.success() && mtext.contains("E0516"), "{mtext}");
}

const ASCII: &str = r#"
fn main() {
    let s = "objectClass=Top É ß".to_string();
    println(s.to_ascii_lowercase());
    println(s.to_ascii_uppercase());
    let v: Str = "uidNumber";
    println(v.to_ascii_lowercase().len());
    println(s);
}
"#;

const ASCII_WANT: &str = "objectclass=top É ß\nOBJECTCLASS=TOP É ß\n9\nobjectClass=Top É ß\n";

#[test]
fn interp_string_ascii_case_is_rusts() {
    assert_eq!(run("ascii", "__mir-run", ASCII), ASCII_WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_string_ascii_case_is_rusts() {
    assert_eq!(run("ascii", "__mir-llvm-run", ASCII), ASCII_WANT);
}

const FOR_REF: &str = r#"
struct Entry { attrs: Vec[(String, Vec[String])] }

impl Entry {
    fn get(self, name: Str) -> Option[ref Vec[String]] {
        for (t, vs) in ref self.attrs {
            if t == name {
                return Some(vs);
            }
        }
        None
    }
}

fn main() {
    let e = Entry { attrs: [("a".to_string(), ["x".to_string(), "y".to_string()].to_vec())].to_vec() };
    match e.get("a") {
        Some(vs) => println(vs.len()),
        None => println("none"),
    }
    println(e.get("b").is_none());
}
"#;

#[test]
fn interp_for_over_a_ref_returns_an_element_borrow() {
    assert_eq!(run("forref", "__mir-run", FOR_REF), "2\ntrue\n");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_for_over_a_ref_returns_an_element_borrow() {
    assert_eq!(run("forref", "__mir-llvm-run", FOR_REF), "2\ntrue\n");
}

#[test]
fn fix_rust_rest_parens_pattern() {
    let src = r#"
enum F { Eq(String, String), P(String) }
fn kind(f: F) -> i64 {
    match f {
        F.Eq(..) => 1,
        _ => 3,
    }
}
fn main() {
    println(kind(F.Eq("a".to_string(), "b".to_string())));
    println(kind(F.P("a".to_string())));
}
"#;
    let (text, out) = fixed("rest", src);
    assert!(text.contains("F.Eq { .. } => 1"), "{text}");
    assert_eq!(out, "1\n3\n");
}

#[test]
fn fix_map_err_on_an_option_to_ok_or_else() {
    let src = r#"
fn hexbyte(hex: Str) -> Result[u8, String] {
    let v = u8.from_str_radix(hex, 16).map_err(|_| "bad escape".to_string())?;
    Ok(v)
}
fn main() {
    println(hexbyte("2a").unwrap());
    println(hexbyte("zz").unwrap_err());
}
"#;
    let (text, out) = fixed("maperr", src);
    assert!(
        text.contains(r#".ok_or_else(|| "bad escape".to_string())?"#),
        "{text}"
    );
    assert_eq!(out, "42\nbad escape\n");
}

#[test]
fn fix_assert_macros_with_format_arguments() {
    let src = r#"
fn main() {
    let s = "abc";
    let rest = 3;
    assert_eq!(rest, s.len(), "trailing text in '{}'", s);
    assert!(rest > 0, "rest {} of {}", rest, s.len());
    assert_ne!(rest, 4, "plain");
    assert!(rest == 3, "inline {rest}");
    println("ok");
}
"#;
    let (text, out) = fixed("assert", src);
    for want in [
        r#"assert_eq(rest, s.len(), f"trailing text in '{s}'");"#,
        r#"assert(rest > 0, f"rest {rest} of {s.len()}");"#,
        r#"assert_ne(rest, 4, "plain");"#,
        r#"assert(rest == 3, f"inline {rest}");"#,
    ] {
        assert!(text.contains(want), "lacks {want}:\n{text}");
    }
    assert_eq!(out, "ok\n");
}

#[test]
fn fix_constructor_named_as_a_value_to_a_closure() {
    let src = r#"
enum Tag { Name(i64), Other }
fn wrap(r: own Result[i64, String]) -> Result[Option[i64], String] {
    r.map(Some)
}
fn main() {
    println(wrap(Ok(3)).unwrap().unwrap());
    let w: Option[Result[i64, String]] = Some(4).map(Ok);
    println(w.unwrap().unwrap());
    let e: Result[i64, Option[i64]] = Err(5).map_err(Some);
    println(e.unwrap_err().unwrap());
    match Some(6).map(Tag.Name) {
        Some(Tag.Name(n)) => println(n),
        _ => println("other"),
    }
}
"#;
    let (dir, _) = scratch("ctor-check", src);
    let check = karac(&dir, &["check", "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    let msg = String::from_utf8_lossy(&check.stderr).to_string()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(
        !check.status.success() && msg.contains("enum variant constructor 'Some'"),
        "{msg}"
    );
    let (text, out) = fixed("ctor", src);
    for want in [
        "r.map(|a0| Some(a0))",
        "Some(4).map(|a0| Ok(a0))",
        "Err(5).map_err(|a0| Some(a0))",
        "Some(6).map(|a0| Tag.Name(a0))",
    ] {
        assert!(text.contains(want), "lacks {want}:\n{text}");
    }
    assert_eq!(out, "3\n4\n5\n6\n");
}
