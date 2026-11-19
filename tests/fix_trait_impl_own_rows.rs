//! B-2026-11-18-66: `karac fix` wrote `own` on a trait impl method's
//! parameter that the trait borrows, E0288 (an impl may not be stronger than
//! its trait) took it off again, and the file ended in whichever state the
//! pass count left it. The `own` now goes on the trait's parameter too, as
//! one fix, or is not offered when the trait's declaration cannot take it.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// The file after one `karac fix`, after a second, and the check after.
fn fix_twice(tag: &str, src: &str) -> (String, String, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-fix-trait-own-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let _ = karac(&["fix"], &path);
    let once = std::fs::read_to_string(&path).unwrap();
    let _ = karac(&["fix"], &path);
    let twice = std::fs::read_to_string(&path).unwrap();
    let after = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    let _ = std::fs::remove_file(&path);
    (once, twice, after)
}

#[test]
fn own_on_a_generic_trait_impl_param_is_written_on_the_trait_too() {
    let src = "trait Wrapper { fn dup[A](ref self, x: A) -> A; }\n\
               struct C { v: i64 }\n\
               impl Wrapper for C { fn dup[A](ref self, x: A) -> A { x } }\n\
               fn main() { let c = C { v: 0 }; println(f\"{c.dup(99)}\"); }\n";
    let (once, twice, after) = fix_twice("generic", src);
    assert!(
        once.contains("trait Wrapper { fn dup[A](ref self, x: own A) -> A; }"),
        "{once}"
    );
    assert!(
        once.contains("fn dup[A](ref self, x: own A) -> A { x }"),
        "{once}"
    );
    assert_eq!(once, twice);
    assert!(after.contains("All checks passed"), "{after}");
}

#[test]
fn own_on_a_mut_ref_self_trait_impl_param_is_written_on_the_trait_too() {
    let src = "trait Store { fn put(mut ref self, k: String); fn count(ref self) -> i64; }\n\
               struct Mem { keys: Vec[String] }\n\
               impl Store for Mem {\n\
               \x20   fn put(mut ref self, k: String) { self.keys.push(k); }\n\
               \x20   fn count(ref self) -> i64 { self.keys.len() }\n\
               }\n\
               fn main() { let mut m = Mem { keys: [] }; m.put(\"a\"); println(m.count()); }\n";
    let (once, twice, after) = fix_twice("store", src);
    assert!(
        once.contains("fn put(mut ref self, k: own String);"),
        "{once}"
    );
    assert!(
        once.contains("fn put(mut ref self, k: own String) { self.keys.push(k); }"),
        "{once}"
    );
    assert_eq!(once, twice);
    assert!(after.contains("All checks passed"), "{after}");
}

#[test]
fn two_impls_giving_a_trait_param_away_write_its_own_once() {
    let src = "trait Sink { fn take(mut ref self, k: String); }\n\
               struct A { keys: Vec[String] }\n\
               struct B { last: Option[String] }\n\
               impl Sink for A { fn take(mut ref self, k: String) { self.keys.push(k); } }\n\
               impl Sink for B { fn take(mut ref self, k: String) { self.last = Some(k); } }\n\
               fn main() {\n\
               \x20   let mut a = A { keys: [] };\n\
               \x20   let mut b = B { last: None };\n\
               \x20   a.take(\"x\");\n\
               \x20   b.take(\"y\");\n\
               \x20   println(a.keys.len());\n\
               }\n";
    let (once, twice, after) = fix_twice("two-impls", src);
    assert!(
        once.contains("trait Sink { fn take(mut ref self, k: own String); }"),
        "{once}"
    );
    assert_eq!(once.matches("k: own String").count(), 3, "{once}");
    assert_eq!(once, twice);
    assert!(after.contains("All checks passed"), "{after}");
}

#[test]
fn a_trait_param_written_ref_takes_no_own_fix_on_its_impl() {
    let src = "trait Store { fn put(mut ref self, k: ref String); }\n\
               struct Mem { keys: Vec[String] }\n\
               impl Store for Mem { fn put(mut ref self, k: String) { self.keys.push(k); } }\n\
               fn main() { let mut m = Mem { keys: [] }; m.put(\"a\"); println(m.keys.len()); }\n";
    let (once, twice, after) = fix_twice("written-ref", src);
    assert_eq!(once, src);
    assert_eq!(twice, src);
    assert!(after.contains("cannot give away parameter `k`"), "{after}");
}
