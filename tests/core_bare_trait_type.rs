//! A trait in a type position is written `dyn Trait` (design.md § `dyn
//! Trait`): a bare trait name there is `E_BARE_TRAIT_TYPE`, reported once
//! per site, and `karac fix` inserts the `dyn`. Before, `Vec[Shape]`
//! lowered to a named type no value has, and the error was a bare
//! "expected 'Shape', found 'Sq'".

use std::process::Command;

const SRC: &str = "trait Shape { fn area(self) -> i64; }
struct Sq { s: i64 }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }
fn show(s: ref Shape) -> i64 { s.area() }
fn main() {
    let v: Vec[Shape] = [Sq { s: 2 }];
    println(show(v[0]));
}
";

fn karac(cmd: &str, path: &std::path::Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(path)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_bare_trait_type_is_refused_once_per_site_and_fixed_with_dyn() {
    let dir = std::env::temp_dir().join(format!("karac-baretrait-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, SRC).unwrap();
    let (ok, err) = karac("check", &path);
    assert!(!ok, "a bare trait type was accepted");
    assert_eq!(err.matches("E_BARE_TRAIT_TYPE").count(), 2, "{err}");
    assert!(err.contains("written `dyn Shape`"), "{err}");
    let (ok, err) = karac("fix", &path);
    assert!(ok, "{err}");
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(fixed.contains("fn show(s: ref dyn Shape)"), "{fixed}");
    assert!(fixed.contains("let v: Vec[dyn Shape] ="), "{fixed}");
    assert_eq!(
        karac::mir::lower::run_source(&fixed)
            .expect("MIR interpreter")
            .output,
        "4\n"
    );
}
