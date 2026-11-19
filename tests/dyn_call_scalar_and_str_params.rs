//! A call through `dyn` passes a `Str` or scalar argument as the slot's
//! impl takes it: a string literal or `0.5` given to a `Str` / `f32`
//! parameter, written by name or as the `dyn` type's trait argument
//! (`dyn Cv[f32]`). The call once typed each argument as it came, a
//! `String` constant or an `f64`, and the compiled program read the wrong
//! bytes: an overflow panic, `0.0`, or every later argument shifted
//! (B-2026-11-19-128). Each program runs the same on the interpreter and
//! compiled.

mod common;

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dynscalar-{}-{name}", std::process::id()));
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

/// What the program prints compiled at `opt`, and whether it exited
/// cleanly; `None` without the llvm feature or a runtime archive.
#[cfg(feature = "llvm")]
fn build_and_run(src: &str, opt: u8) -> Option<(bool, String)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("karac-dynscalar-b-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    Some((
        out.status.success(),
        String::from_utf8(out.stdout).expect("utf-8"),
    ))
}

#[cfg(not(feature = "llvm"))]
fn build_and_run(_: &str, _: u8) -> Option<(bool, String)> {
    None
}

/// The check accepts `src`, the interpreter prints `want`, and so does the
/// program compiled.
fn runs(name: &str, src: &str, want: &str) {
    let (ok, err) = check(name, src);
    assert!(ok, "{err}");
    let got = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(got, want);
    for opt in [0, 2] {
        if let Some((ok, out)) = build_and_run(src, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn a_string_literal_into_a_str_parameter() {
    runs(
        "e01",
        "trait Nm { fn nm(self, p: Str) -> i64; fn cat(self, p: Str) -> String; }
struct A { k: i64, s: String }
impl Nm for A {
    fn nm(self, p: Str) -> i64 { p.len() + self.k }
    fn cat(self, p: Str) -> String { p.to_string() + self.s }
}
fn main() { let e: dyn Nm = A { k: 1, s: \"x\".to_string() }; println(e.nm(\"abcd\")); println(e.cat(\"ab\")); }
",
        "5\nabx\n",
    );
}

#[test]
fn float_and_narrow_integer_literals_into_their_parameters() {
    runs(
        "e04",
        "trait Sc { fn sc(self, y: f32) -> f64; fn two(self, x: f64, y: f32) -> f64; fn ints(self, a: i32, b: u8) -> i64; }
struct B { k: f64 }
impl Sc for B {
    fn sc(self, y: f32) -> f64 { (y as f64) + self.k }
    fn two(self, x: f64, y: f32) -> f64 { x + (y as f64) }
    fn ints(self, a: i32, b: u8) -> i64 { (a as i64) * 1000 + (b as i64) }
}
fn main() { let e: dyn Sc = B { k: 2.0 }; println(e.sc(0.5)); println(e.two(3.0, 0.5)); println(e.ints(-21, 200)); }
",
        "2.5\n3.5\n-20800\n",
    );
}

#[test]
fn a_trait_argument_parameter_takes_the_dyn_types_argument() {
    runs(
        "h02",
        "trait Cv[T] { fn cv(self, x: T) -> f64; }
struct A { }
impl Cv[f32] for A { fn cv(self, x: f32) -> f64 { x as f64 } }
fn main() { let c: dyn Cv[f32] = A { }; println(c.cv(0.5)); }
",
        "0.5\n",
    );
}

#[test]
fn mixed_parameters_after_a_str_and_an_f32_keep_their_places() {
    runs(
        "h08",
        "trait Mx { fn mx(self, a: i8, b: f32, c: Str, d: u16, e: f64, f: char) -> String; }
struct A { }
impl Mx for A { fn mx(self, a: i8, b: f32, c: Str, d: u16, e: f64, f: char) -> String { f\"{a} {b} {c} {d} {e} {f}\" } }
fn main() { let e: dyn Mx = A { }; println(e.mx(-3, 1.5, \"hi\", 700, 2.25, 'q')); }
",
        "-3 1.5 hi 700 2.25 q\n",
    );
}

#[test]
fn typed_arguments_still_pass_as_before() {
    runs(
        "typed",
        "trait Nm { fn nm(self, p: Str) -> i64; fn sc(self, y: f32) -> f64; }
struct A { k: i64 }
impl Nm for A { fn nm(self, p: Str) -> i64 { p.len() + self.k } fn sc(self, y: f32) -> f64 { y as f64 } }
fn main() { let e: dyn Nm = A { k: 1 }; let s = \"abcd\".to_string(); let x: Str = \"xy\"; let y: f32 = 0.25; println(e.nm(s.as_str())); println(e.nm(x)); println(e.sc(y)); }
",
        "5\n3\n0.25\n",
    );
}
