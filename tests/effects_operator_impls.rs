//! A program's operator impl on a primitive (`impl Mul[Col] for f64`) and an
//! operator whose operands differ in type (`v * 2.0` on `impl Mul[f64] for
//! V`): the built-in `f64 * f64` beside such an impl stays pure
//! (B-2026-10-29-48), and the impl's own effects reach the operator that calls
//! it (B-2026-10-29-51).

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-op-impls-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn float_multiply_beside_a_primitive_operator_impl_is_not_that_impl() {
    let (ok, text) = check(
        "recursion",
        "struct Col { v: Vec[f64] }
         fn scaled(c: ref Col, k: f64) -> Col {
             let mut out: Vec[f64] = Vec.new();
             for x in c.v { out.push(*x * k); }
             Col { v: out }
         }
         impl Mul[Col] for f64 {
             type Output = Col;
             fn mul(self, rhs: Col) -> Col { scaled(rhs, self) }
         }
         fn main() {
             let c = Col { v: vec![1.0, 2.0] };
             let d = 2.0 * c;
             println(f\"{d.v[1]}\");
         }",
    );
    assert!(ok, "check must pass: {text}");
    assert!(
        !text.contains("mutual recursion"),
        "`scaled` never calls the impl, so no recursion note: {text}"
    );
}

#[test]
fn float_multiply_does_not_take_a_printing_operator_impls_effects() {
    let (ok, text) = check(
        "pure",
        "pub struct Col { v: Vec[f64] }
         impl Mul[Col] for f64 {
             type Output = Col;
             fn mul(self, rhs: Col) -> Col {
                 println(\"scaling\");
                 Col { v: rhs.v.clone() }
             }
         }
         pub fn area(w: f64, h: f64) -> f64 {
             w * h
         }
         fn main() {
             let d = 2.0 * Col { v: vec![1.0] };
             println(f\"{d.v[0]} {area(2.0, 3.0)}\");
         }",
    );
    assert!(
        ok,
        "`area` only multiplies two floats, so it declares nothing: {text}"
    );
}

#[test]
fn primitive_left_operator_is_charged_its_impls_effects() {
    let (ok, text) = check(
        "primitive-left",
        "pub struct Col { v: Vec[f64] }
         impl Mul[Col] for f64 {
             type Output = Col;
             fn mul(self, rhs: Col) -> Col {
                 println(\"scaling\");
                 Col { v: rhs.v.clone() }
             }
         }
         pub fn doubled(c: own Col) -> Col {
             2.0 * c
         }
         fn main() {
             println(f\"{doubled(Col { v: vec![1.0] }).v[0]}\");
         }",
    );
    assert!(!ok, "check must refuse: {text}");
    assert!(
        text.contains("public function 'doubled' performs effects [writes(Stdout)]"),
        "`2.0 * c` calls the printing impl: {text}"
    );
}

#[test]
fn mixed_type_operator_is_charged_its_impls_effects() {
    let (ok, text) = check(
        "mixed",
        "pub struct V { x: f64 }
         impl Mul[f64] for V {
             type Output = V;
             fn mul(self, k: f64) -> V {
                 println(\"mul\");
                 V { x: self.x * k }
             }
         }
         pub fn scale(v: own V) -> V {
             v * 2.0
         }
         pub fn scale_with(v: own V) -> V with writes(Stdout) {
             v * 2.0
         }
         fn main() {
             println(f\"{scale(V { x: 1.5 }).x} {scale_with(V { x: 1.0 }).x}\");
         }",
    );
    assert!(!ok, "check must refuse: {text}");
    assert!(
        text.contains("public function 'scale' performs effects [writes(Stdout)]"),
        "`v * 2.0` calls the printing impl, as `v.mul(2.0)` does: {text}"
    );
    assert!(
        !text.contains("'scale_with'"),
        "a declared `writes(Stdout)` covers it: {text}"
    );
}
