//! A call through a `dyn` value has the effects of the concrete types that
//! can be behind it (design.md § `dyn Trait`): at each call site of a `dyn`
//! parameter, the value passed there; on a local, the values coerced into
//! it in view. A value in a field, a collection's element or a call's
//! result has the effects its type declares, which with no clause is every
//! effect.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

const SHAPES: &str = "trait Shape {
    fn area(self) -> f64;
    fn draw(self);
}

struct Circle { r: f64 }
struct Loud { n: i64 }

impl Shape for Circle {
    fn area(self) -> f64 { 3.0 * self.r * self.r }
    fn draw(self) { }
}

impl Shape for Loud {
    fn area(self) -> f64 { println(\"loud area\"); 1.0 }
    fn draw(self) { println(\"loud\"); }
}

struct Holder { s: dyn Shape }

fn show(s: ref dyn Shape) -> f64 { s.area() }
fn paint(s: ref dyn Shape) { s.draw(); }
fn twice(s: ref dyn Shape) { paint(s); paint(s); }
fn make(k: i64) -> dyn Shape { if k == 0 { Circle { r: 1.0 } } else { Loud { n: k } } }
fn swap_in(s: mut ref dyn Shape) { *s = Loud { n: 9 }; }
";

fn fixture(tag: &str, main: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-dyn-effects-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, format!("{SHAPES}\nfn main() {{\n{main}\n}}\n")).unwrap();
    (dir, path)
}

fn check(tag: &str, main: &str) -> (bool, String) {
    let (dir, path) = fixture(tag, main);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn accepted(tag: &str, main: &str) {
    let (ok, out) = check(tag, main);
    assert!(ok, "{tag}: check must accept: {out}");
}

fn refused(tag: &str, main: &str, want: &str) {
    let (ok, out) = check(tag, main);
    assert!(!ok, "{tag}: check must refuse: {out}");
    assert!(out.contains(want), "{tag}: want `{want}` in: {out}");
}

const STDOUT: &str = "the `par` conflict: `writes(Stdout)` and `writes(Stdout)`";

#[test]
fn dyn_param_calls_have_the_passed_values_effects() {
    let main = "    let c = Circle { r: 1.0 };
    let l = Loud { n: 1 };
    let (x, y) = par { show(c), show(l) };
    println(f\"{x} {y}\");";
    accepted("param_apart", main);
    let (dir, path) = fixture("param_apart_run", main);
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "loud area\n3.0 1.0\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    refused(
        "param_shared",
        "    let a = Loud { n: 1 };
    let b = Loud { n: 2 };
    par { paint(a), paint(b) };",
        STDOUT,
    );
}

#[test]
fn dyn_param_passed_along_keeps_the_callers_value() {
    accepted(
        "forward_apart",
        "    let c = Circle { r: 1.0 };
    let l = Loud { n: 1 };
    par { twice(c), twice(l), paint(c) };",
    );
    refused(
        "forward_shared",
        "    let a = Loud { n: 1 };
    let b = Loud { n: 2 };
    par { twice(a), twice(b) };",
        STDOUT,
    );
}

#[test]
fn dyn_local_has_the_values_coerced_into_it() {
    accepted(
        "local_quiet",
        "    let o: dyn Shape = Circle { r: 1.0 };
    par { o.draw(), println(\"x\") };",
    );
    refused(
        "local_loud",
        "    let o: dyn Shape = Loud { n: 1 };
    par { o.draw(), println(\"x\") };",
        STDOUT,
    );
    refused(
        "local_reassigned",
        "    let mut o: dyn Shape = Circle { r: 1.0 };
    o = Loud { n: 1 };
    par { o.draw(), println(\"x\") };",
        STDOUT,
    );
    refused(
        "local_written_by_callee",
        "    let mut o: dyn Shape = Circle { r: 1.0 };
    swap_in(mut o);
    par { o.area(), println(\"x\") };",
        STDOUT,
    );
}

#[test]
fn dyn_value_in_a_field_element_or_result_has_its_declared_effects() {
    refused(
        "field",
        "    let h = Holder { s: Circle { r: 1.0 } };
    par { h.s.draw(), println(\"x\") };",
        "a call through a `dyn` value in a field",
    );
    refused(
        "element",
        "    let v: Vec[dyn Shape] = vec![Circle { r: 1.0 }];
    for s in v { par { s.draw(), println(\"x\") }; }",
        "a call through a `dyn` value a call hands back",
    );
    refused(
        "result",
        "    let m = make(0);
    par { m.draw(), println(\"x\") };",
        "a call through a `dyn` value a call hands back",
    );
}
