//! Coercion to `dyn` of a generic trait or through a generic impl: a
//! view type argument (`dyn Cv[Str]`, `dyn Cv[Slice[i64]]`,
//! B-2026-11-20-1), a blanket impl (`impl[T: Nm] Loud for T`,
//! B-2026-11-20-2), a type with two impls of one generic trait
//! (B-2026-11-20-4), and an open generic value whose one impl fixes it
//! (`Bag { xs: Vec.new() }` into `dyn Ac`, B-2026-11-20-8). Each was
//! refused; each now runs the same on the interpreter and compiled.

mod common;

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dyngeneric-{}-{name}", std::process::id()));
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
    let dir = std::env::temp_dir().join(format!("karac-dyngeneric-b-{}-{n}", std::process::id()));
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

const CV: &str = "trait Cv[T] { fn cv(self, x: T) -> i64; }\nstruct A { }\nstruct B { }\n";

#[test]
fn a_dyn_of_a_generic_trait_at_str() {
    runs(
        "i05",
        &format!(
            "{CV}impl Cv[Str] for A {{ fn cv(self, x: Str) -> i64 {{ x.len() }} }}
impl Cv[Str] for B {{ fn cv(self, x: Str) -> i64 {{ 10 }} }}
fn main() {{ let v: Vec[dyn Cv[Str]] = [A {{ }}, B {{ }}]; let mut t = 0; for e in v.iter() {{ t += e.cv(\"xyz\"); }} println(t); }}
"
        ),
        "13\n",
    );
}

#[test]
fn a_dyn_of_a_generic_trait_at_a_slice() {
    runs(
        "slice",
        &format!(
            "{CV}impl Cv[Slice[i64]] for A {{ fn cv(self, x: Slice[i64]) -> i64 {{ x.len() }} }}
fn main() {{ let v = [1, 2, 3]; let e: dyn Cv[Slice[i64]] = A {{ }}; println(e.cv(v.as_slice())); }}
"
        ),
        "3\n",
    );
}

#[test]
fn a_dyn_through_a_blanket_impl() {
    runs(
        "blanket",
        "trait Nm { fn nm(self) -> i64; }
trait Loud { fn loud(self) -> i64; }
impl[T: Nm] Loud for T { fn loud(self) -> i64 { self.nm() * 10 } }
struct A { k: i64 }
impl Nm for A { fn nm(self) -> i64 { self.k } }
fn main() { let e: dyn Loud = A { k: 4 }; println(e.loud()); }
",
        "40\n",
    );
}

#[test]
fn a_type_with_two_impls_of_one_generic_trait_coerces_to_either() {
    runs(
        "i06",
        &format!(
            "{CV}impl Cv[Str] for A {{ fn cv(self, x: Str) -> i64 {{ x.len() }} }}
impl Cv[i64] for A {{ fn cv(self, x: i64) -> i64 {{ x }} }}
fn main() {{ let e: dyn Cv[Str] = A {{ }}; let f: dyn Cv[i64] = A {{ }}; println(e.cv(\"ab\") + f.cv(5)); }}
"
        ),
        "7\n",
    );
    runs(
        "y08",
        &format!(
            "{CV}impl Cv[i64] for A {{ fn cv(self, x: i64) -> i64 {{ x }} }}
impl Cv[String] for A {{ fn cv(self, x: String) -> i64 {{ x.len() }} }}
fn main() {{ let f: dyn Cv[String] = A {{ }}; println(f.cv(\"abc\".to_string())); }}
"
        ),
        "3\n",
    );
}

#[test]
fn no_impl_at_the_dyn_arguments_names_every_impl() {
    let (ok, err) = check(
        "misfit",
        &format!(
            "{CV}impl Cv[i64] for A {{ fn cv(self, x: i64) -> i64 {{ x }} }}
impl Cv[String] for A {{ fn cv(self, x: String) -> i64 {{ x.len() }} }}
fn main() {{ let f: dyn Cv[bool] = A {{ }}; }}
"
        ),
    );
    assert!(!ok);
    assert!(
        err.contains("`A` implements `Cv[i64]` and `Cv[String]`"),
        "{err}"
    );
}

#[test]
fn an_open_generic_value_takes_its_one_impls_arguments() {
    runs(
        "v08",
        "trait Ac { fn add(mut ref self, s: own String); fn n(self) -> i64; }
struct Bag[T] { xs: Vec[T] }
impl Ac for Bag[String] { fn add(mut ref self, s: own String) { self.xs.push(s); } fn n(self) -> i64 { self.xs.len() } }
fn main() { let mut e: dyn Ac = Bag { xs: Vec.new() }; e.add(\"a\".to_string()); println(e.n()); }
",
        "1\n",
    );
}
