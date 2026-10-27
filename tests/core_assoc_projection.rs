//! B-2026-10-27-18: a projection names one trait's associated type
//! (design.md § Associated types, "A projection names one trait's type").
//! `T.Name` belongs to the one bound on `T` that declares `Name`; with two,
//! it is an error where it is written (`E_AMBIGUOUS_ASSOC_TYPE`), each bound
//! fixes its own, and an operator's result carries its trait.

use std::process::Command;

fn karac(tag: &str, cmd: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-assoc-projection-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

// `V`'s `Add` and `Mul` each bind an `Output`, to different types.
const V: &str = r#"
#[derive(Copy, Clone)]
struct V { x: i64 }
impl Add for V {
    type Output = V;
    fn add(own self, rhs: own V) -> V { V { x: self.x + rhs.x } }
}
impl Mul for V {
    type Output = i64;
    fn mul(own self, rhs: own V) -> i64 { self.x * rhs.x }
}
"#;

#[test]
fn each_bound_fixes_its_own_output() {
    let src = format!(
        "{V}fn both[T: Add[Output = T] + Mul[Output = i64] + Copy](a: own T, b: own T) -> (T, i64) {{
    (a + b, a * b)
}}
fn open[T: Add[Output = A] + Mul[Output = M] + Copy, A, M](a: own T, b: own T) -> (A, M) {{
    (a + b, a * b)
}}
fn main() {{
    let (s, p) = both(V {{ x: 2 }}, V {{ x: 3 }});
    println(f\"{{s.x}} {{p}}\");
    let (s, p) = open(V {{ x: 4 }}, V {{ x: 5 }});
    println(f\"{{s.x}} {{p}}\");
    let (s, p) = open(2, 3);
    println(f\"{{s}} {{p}}\");
}}
"
    );
    let (ok, out, err) = karac("own", "run", &src);
    assert!(ok, "{err}");
    assert_eq!(out, "5 6\n9 20\n5 6\n");
}

#[test]
fn an_operators_result_is_its_own_traits_output() {
    // `Add[Output = T]` says nothing of `a * b`, which is Mul's `T.Output`.
    let src = format!(
        "{V}fn f[T: Add[Output = T] + Mul](a: own T, b: own T) -> T {{ a * b }}\nfn main() {{ }}\n"
    );
    let (ok, _, err) = karac("other-trait", "check", &src);
    assert!(!ok);
    assert!(
        err.contains("expected 'T', found 'T.Output'") && err.contains("write `Mul[Output = T]`"),
        "{err}"
    );
}

#[test]
fn a_projection_two_bounds_declare_is_ambiguous() {
    let src = "fn f[T: Add + Mul](a: own T, b: own T) -> T.Output { a + b }\n\
               trait A { type Out; }\n\
               trait B { type Out; }\n\
               fn g[T: A + B](x: own T) -> i64 where T.Out = i64 { 0 }\n\
               fn h[I: Iterator + IntoIterator](x: own I) -> Option[I.Item] { None }\n\
               fn main() { }\n";
    let (ok, _, err) = karac("ambiguous", "check", src);
    assert!(!ok);
    assert!(
        err.contains(
            "error[E_AMBIGUOUS_ASSOC_TYPE]: `T.Output` is ambiguous: `Add` and `Mul` on 'T' \
             each declare `Output`"
        ),
        "{err}"
    );
    assert!(
        err.contains("`T.Out` is ambiguous: `A` and `B` on 'T' each declare `Out`"),
        "{err}"
    );
    assert!(
        err.contains("`I.Item` is ambiguous: `IntoIterator` and `Iterator` on 'I'"),
        "{err}"
    );
    assert_eq!(
        err.matches("error[typecheck]").count(),
        3,
        "once each: {err}"
    );
}

#[test]
fn a_projection_one_bound_declares_names_it() {
    // A supertrait's associated type is the bound's: `Dbl: Iterator`.
    let src = "trait Dbl: Iterator { fn twice(mut ref self) -> i64; }\n\
               fn first[I: Iterator](it: own I) -> Option[I.Item] { let mut it = it; it.next() }\n\
               fn diff[T: Sub + Copy](a: own T, b: own T) -> T.Output { a - b }\n\
               fn main() { println(first(vec![7].into_iter()).unwrap()); println(diff(9, 4)); }\n";
    let (ok, out, err) = karac("one", "run", src);
    assert!(ok, "{err}");
    assert_eq!(out, "7\n5\n");
}
