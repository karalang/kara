//! design.md §9 shapes `karac check` used to accept (B-2026-11-06-27, items
//! 3, 4, 5, 6 and 10): a derive plus a manual impl of the same trait,
//! `#[derive(Drop)]`, an impl whose `Self` is `dyn Trait`, an unknown
//! associated binding in a generic parameter's bound, and `Clone.clone`
//! implemented as `clone(own self)`. Each is now refused where it is written;
//! the neighbouring correct shapes still check and run.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-derive-lists-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

/// `(line, column)` of every `karac check --output=json` error whose message
/// contains `needle`, plus how many errors there were in all.
fn errors_with(tag: &str, src: &str, needle: &str) -> (Vec<(u64, u64)>, usize) {
    let out = karac(tag, &["check", "--output=json"], src);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tag}: {e}"));
    let diags = json["diagnostics"].as_array().expect("diagnostics");
    let errors: Vec<&serde_json::Value> =
        diags.iter().filter(|d| d["severity"] == "error").collect();
    let hits = errors
        .iter()
        .filter(|d| d["message"].as_str().unwrap_or("").contains(needle))
        .map(|d| (d["line"].as_u64().unwrap(), d["column"].as_u64().unwrap()))
        .collect();
    (hits, errors.len())
}

#[test]
fn derive_plus_manual_impl_of_one_trait_is_a_conflicting_impl() {
    let src = "#[derive(PartialEq)]
struct Pt { x: i64 }
impl PartialEq for Pt { fn eq(self, other: Pt) -> bool { true } }
fn main() { println(Pt { x: 1 } == Pt { x: 2 }); }
";
    let (hits, _) = errors_with(
        "dup",
        src,
        "conflicting impl: `Pt` derives `PartialEq` and also implements it here",
    );
    assert_eq!(hits, vec![(3, 1)]);
}

#[test]
fn derive_drop_is_not_derivable() {
    let src = "#[derive(Drop)]
struct R {}
fn main() { let r = R {}; println(1); }
";
    let (hits, _) = errors_with("drop", src, "`Drop` is not derivable");
    assert_eq!(hits, vec![(2, 1)]);
}

#[test]
fn impl_for_a_dyn_type_is_refused() {
    let src = "trait Shape { fn area(self) -> i64; }
trait Extra { fn twice(self) -> i64; }
struct Sq { s: i64 }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }
impl Extra for dyn Shape { fn twice(self) -> i64 { 2 } }
fn main() { let s: dyn Shape = Sq { s: 2 }; println(s.area()); }
";
    let (hits, _) = errors_with("dyn", src, "`impl Extra for dyn ...` is not allowed");
    assert_eq!(hits, vec![(5, 1)]);
}

#[test]
fn unknown_assoc_binding_in_a_generic_bound_is_refused() {
    let src = "fn f[I: Iterator[Elem = i64]](it: own I) -> i64 { 0 }
fn main() { println(f([1, 2].into_iter())); }
";
    let (hits, _) = errors_with(
        "assoc",
        src,
        "`Elem` is not an associated type of trait `Iterator`",
    );
    assert_eq!(hits, vec![(1, 18)]);
}

#[test]
fn clone_taking_own_self_is_stronger_than_the_trait() {
    let src = "struct Pt { v: Vec[i64] }
impl Clone for Pt { fn clone(own self) -> Pt { self } }
fn main() { let a = Pt { v: [1] }; let b = a.clone(); println(b.v.len()); }
";
    let (hits, _) = errors_with(
        "clone",
        src,
        "takes its receiver as `own self`, but 'Clone.clone' declares `self`",
    );
    assert_eq!(hits, vec![(2, 30)]);
}

/// The correct neighbours of every refused shape: a manual impl on a type
/// that does not derive the trait, a derived trait with a manual impl of a
/// different one, a library-expanded derive (`Serialize`, whose impl is
/// spliced in from `serde.kara`), the real `Item` binding in a bound, and
/// `clone(self)`. They check clean and print the same on both backends.
#[test]
fn neighbouring_derive_and_impl_shapes_still_run() {
    let src = "import std.json.{Json};
#[derive(PartialEq, Clone)]
struct A { x: i64 }
struct B { x: i64 }
impl PartialEq for B { fn eq(self, other: B) -> bool { self.x % 10 == other.x % 10 } }
#[derive(PartialEq)]
struct C { v: Vec[i64] }
impl Clone for C { fn clone(self) -> C { C { v: [self.v.len()] } } }
#[derive(Serialize, Deserialize)]
struct D { n: i64 }
fn total[I: Iterator[Item = i64]](it: own I) -> i64 {
    let mut s = 0;
    for x in it { s += x; }
    s
}
fn main() {
    println(A { x: 1 } == A { x: 1 }.clone());
    println(B { x: 3 } == B { x: 13 });
    let c = C { v: [7, 8, 9] };
    println(c.clone().v[0]);
    println(Json.serialize(D { n: 4 }));
    println(total([1, 2, 3].into_iter()));
}
";
    let (_, n) = errors_with("ok", src, "");
    assert_eq!(n, 0, "the correct shapes must check clean");
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for backend in backends {
        let out = karac("ok", &[backend], src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "true\ntrue\n3\n{\"n\":4}\n6\n",
            "{backend}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn run_time_serde_default_is_not_a_constant() {
    let src = "import std.json.{Json};
#[derive(Serialize, Deserialize)]
struct C { #[serde(default: Vec.new())] v: Vec[i64], #[serde(default: 8080)] port: i64 }
fn main() { println(1); }
";
    let (hits, n) = errors_with(
        "serde-default",
        src,
        "`#[serde(default: ..)]` of field `v` must be a compile-time constant",
    );
    assert_eq!(hits, vec![(3, 29)]);
    assert_eq!(n, 1, "`default: 8080` is a constant and stays accepted");
}

/// design.md §9: a derive never adds another. 0.8.x still fills the missing
/// derive in, with a `deprecated` warning whose fix writes it into the list;
/// `#[allow(deprecated)]` on the type silences it, and a complete list has
/// none. The program still runs, so the fill-in itself is unchanged.
#[test]
fn implied_derive_is_a_deprecation_warning_with_a_fix() {
    let src = "#[derive(Eq, Hash)]
struct A { x: i64 }
#[derive(Copy)]
struct B { x: i64 }
#[derive(Ord, Debug)]
enum C { X, Y }
#[derive(Hash)]
#[allow(deprecated)]
struct D { x: i64 }
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash)]
struct E { x: i64 }
fn main() {
    let b = B { x: 1 };
    let c = b;
    println(b.x + c.x);
    println(A { x: 1 } == A { x: 1 });
    println(C.X < C.Y);
}
";
    let out = karac("implied", &["check", "--output=json"], src);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let warned: Vec<(u64, String)> = json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| {
            d["message"]
                .as_str()
                .unwrap_or("")
                .starts_with("implied derive on")
        })
        .map(|d| {
            (
                d["line"].as_u64().unwrap(),
                d["replacement"]["text"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(
        warned,
        vec![
            (1, ", PartialEq".to_string()),
            (3, ", Clone".to_string()),
            (5, ", PartialOrd, Eq, PartialEq".to_string()),
        ]
    );
    let out = karac("implied-run", &["__mir-run"], src);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "2\ntrue\ntrue\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
