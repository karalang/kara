//! Two `dyn Trait` rules `karac check` did not enforce (design.md § `dyn
//! Trait`). A `mut ref dyn Trait` cannot replace the value it views (`*r =
//! other`), because the new value may have another type and size. A
//! standard-library supertrait is judged for dispatchability like a
//! program's own: `PartialEq.eq` names `Self` outside the receiver, so a
//! `dyn` of a trait that extends it is `E_DYN_NOT_DISPATCHABLE`, while
//! `Display` and `Iterator` stay usable.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dynview-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
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

const SHAPES: &str = "trait Shape { fn area(ref self) -> i64; }
struct Sq { s: i64 }
impl Shape for Sq { fn area(ref self) -> i64 { self.s * self.s } }
struct Ci { r: i64 }
impl Shape for Ci { fn area(ref self) -> i64 { 3 * self.r * self.r } }
";

#[test]
fn a_mut_ref_dyn_cannot_replace_the_value_it_views() {
    let src = format!(
        "{SHAPES}fn swap_in(r: mut ref dyn Shape) {{ *r = Ci {{ r: 2 }}; }}
fn main() {{
    let mut b: dyn Shape = Sq {{ s: 3 }};
    swap_in(mut b);
    println(f\"{{b.area()}}\");
}}
"
    );
    let (ok, err) = check("replace", &src);
    assert!(!ok, "check accepted `*r = other` through a `mut ref dyn`");
    assert!(
        err.contains("E_DYN_REPLACE_THROUGH_VIEW")
            && err.contains("a `mut ref dyn Shape` cannot replace the value it views"),
        "{err}"
    );
}

#[test]
fn assigning_the_owned_dyn_itself_replaces_it() {
    let src = format!(
        "{SHAPES}fn main() {{
    let mut b: dyn Shape = Sq {{ s: 3 }};
    b = Ci {{ r: 2 }};
    println(f\"{{b.area()}}\");
}}
"
    );
    let (ok, err) = check("owned", &src);
    assert!(ok, "{err}");
    assert_eq!(
        karac::mir::lower::run_source(&src)
            .expect("MIR interpreter")
            .output,
        "12\n"
    );
}

#[test]
fn a_standard_library_supertrait_is_judged_for_dispatchability() {
    let src = "trait T: PartialEq { fn n(ref self) -> i64; }
struct A { v: i64 }
impl PartialEq for A { fn eq(ref self, other: ref A) -> bool { self.v == other.v } }
impl T for A { fn n(ref self) -> i64 { self.v } }
fn main() {
    let x: dyn T = A { v: 1 };
    println(f\"{x.n()}\");
}
";
    let (ok, err) = check("partial_eq", src);
    assert!(
        !ok,
        "check accepted a `dyn` of a trait extending `PartialEq`"
    );
    assert!(
        err.contains("E_DYN_NOT_DISPATCHABLE")
            && err.contains("its required method `eq` (from its supertrait `PartialEq`)")
            && err.contains("Drop the supertrait `PartialEq`"),
        "{err}"
    );
}

#[test]
fn dyn_display_and_dyn_iterator_stay_usable() {
    let src = "trait Named: Display { fn name(ref self) -> String; }
struct P { n: String }
impl Display for P { fn to_string(ref self) -> String { self.n.clone() } }
impl Named for P { fn name(ref self) -> String { self.n.clone() } }
fn main() {
    let ps: Vec[dyn Iterator[Item = i64]] = Vec.new();
    let x: dyn Named = P { n: \"a\".to_string() };
    let d: dyn Display = P { n: \"b\".to_string() };
    println(f\"{x.name()} {ps.len()} {d.to_string()}\");
}
";
    let (ok, err) = check("display", src);
    assert!(ok, "{err}");
    assert_eq!(
        karac::mir::lower::run_source(src)
            .expect("MIR interpreter")
            .output,
        "a 0 b\n"
    );
}
