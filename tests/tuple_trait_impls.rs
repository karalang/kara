//! A trait impl for a tuple type (B-2026-11-21-30). design.md § Coherence:
//! a program implements its own trait for a tuple, which is foreign to
//! every package. Such an impl was accepted and then never found (`no
//! method 'show' on type '(i64, i64)'`); for a while it was refused as not
//! supported. Now a concrete tuple's impl answers method calls, default
//! methods, `Trait.m(x)`, bounds and `dyn` coercions, with its receiver
//! mode and effects checked as any impl's are. An impl over type
//! parameters stays refused (deferred.md § Generic trait impls for tuple
//! types).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn cell(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-tuple-impls-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// Clean under `check` (no warning either), and printing `expected` under
/// every backend.
fn runs_to(tag: &str, src: &str, expected: &str) {
    let dir = cell(tag, src);
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: check: {out}");
    assert!(!out.contains("warning"), "{tag}: {out}");
    for mode in modes() {
        let (ok, out) = karac(&dir, &[mode]);
        assert!(ok, "{tag}: {mode}: {out}");
        assert_eq!(out, expected, "{tag}: {mode}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn refused(tag: &str, src: &str) -> String {
    let dir = cell(tag, src);
    let (ok, out) = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "{tag}: accepted");
    out
}

const SHOW: &str = "trait Show {
    fn show(self) -> String;
    fn twice(self) -> String { f\"{self.show()}{self.show()}\" }
}
impl Show for (i64, i64) { fn show(self) -> String { f\"{self.0}-{self.1}\" } }
impl Show for (String, bool) { fn show(self) -> String { f\"{self.0}?{self.1}\" } }
fn g[T: Show](x: T) -> String { x.show() }
";

#[test]
fn tuple_impl_answers_methods_defaults_ufcs_bounds_and_dyn() {
    runs_to(
        "surface",
        &format!(
            "{SHOW}fn main() {{
    let p = (1, 2);
    println(p.show());
    println(p.twice());
    println(Show.show(p));
    println(g(p));
    println(g((\"t\", false)));
    let q: (String, bool) = (\"s\", true);
    println(q.show());
    let d: dyn Show = (7, 8);
    println(d.show());
}}
"
        ),
        "1-2\n1-21-2\n1-2\n1-2\nt?false\ns?true\n7-8\n",
    );
}

/// Two impls of one trait for tuples that differ only in their element
/// widths: each tuple finds its own, and an open literal tuple takes the
/// default width's.
#[test]
fn tuple_impls_are_keyed_by_their_element_types() {
    runs_to(
        "widths",
        "trait Show { fn show(self) -> String; }
impl Show for (i64, i64) { fn show(self) -> String { f\"wide {self.0}\" } }
impl Show for (i32, i32) { fn show(self) -> String { f\"narrow {self.0}\" } }
fn g[T: Show](x: T) -> String { x.show() }
fn main() {
    let a: (i32, i32) = (3, 4);
    println(a.show());
    println(g(a));
    println((5, 6).show());
    println(g((1, 2)));
}
",
        "narrow 3\nnarrow 3\nwide 5\nwide 1\n",
    );
}

/// `mut ref self` writes the caller's tuple and needs its `let mut`; an
/// `own self` method consumes a tuple holding a `String`.
#[test]
fn tuple_impl_receiver_modes() {
    runs_to(
        "modes",
        "trait Bump { fn bump(mut ref self); }
impl Bump for (i64, i64) { fn bump(mut ref self) { self.0 += 1; } }
trait Take { fn take(own self) -> String; }
impl Take for (String, i64) { fn take(own self) -> String { f\"{self.0}{self.1}\" } }
fn main() {
    let mut r = (5, 6);
    r.bump();
    r.bump();
    println(f\"{r.0} {r.1}\");
    let s: (String, i64) = (\"n\", 4);
    println(s.take());
}
",
        "7 6\nn4\n",
    );
    let out = refused(
        "needs-mut",
        "trait Bump { fn bump(mut ref self); }
impl Bump for (i64, i64) { fn bump(mut ref self) { self.0 += 1; } }
fn main() { let s = (1, 1); s.bump(); }
",
    );
    assert!(out.contains("cannot call `bump` on `s`"), "{out}");
}

/// A tuple impl method's effects reach its callers.
#[test]
fn tuple_impl_effects_reach_the_caller() {
    let out = refused(
        "effects",
        "trait Say { fn say(self); }
impl Say for (i64, i64) { fn say(self) { println(f\"{self.0}\") } }
pub fn quiet(p: (i64, i64)) { p.say(); }
fn main() { quiet((1, 2)); }
",
    );
    assert!(out.contains("E0400"), "{out}");
    assert!(out.contains("writes(Stdout)"), "{out}");
}

#[test]
fn tuple_impl_over_type_parameters_is_refused() {
    let out = refused(
        "generic",
        "trait Show { fn show(self) -> String; }
impl[A, B] Show for (A, B) { fn show(self) -> String { \"p\" } }
fn main() { }
",
    );
    assert!(
        out.contains(
            "a trait impl for a tuple whose elements are type parameters is not supported yet"
        ),
        "{out}"
    );
    let out = refused(
        "inherent",
        "impl (i64, i64) { fn sum(self) -> i64 { self.0 + self.1 } }\nfn main() { }\n",
    );
    assert!(out.contains("a tuple type has no inherent impl"), "{out}");
}

/// A bound a tuple does not meet lists the tuples that do, as tuples.
#[test]
fn tuple_impl_candidates_render_as_tuples() {
    let out = refused(
        "candidates",
        &format!("{SHOW}fn main() {{ let b: (bool, bool) = (true, true); println(g(b)); }}\n"),
    );
    assert!(out.contains("(String, bool), (i64, i64)"), "{out}");
    assert!(!out.contains("Tuple["), "{out}");
}
