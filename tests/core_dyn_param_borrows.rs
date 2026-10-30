//! A bare `dyn Trait` parameter borrows, as any bare parameter of an owned
//! type does under D5 (core-semantics §4.1), and `own dyn Trait` or
//! `escaping dyn Trait` takes it by value. The check read a bare `dyn`
//! parameter as owned while the MIR builder borrowed it, so the two
//! disagreed both ways (B-2026-10-30-1).

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dynparam-{}-{name}", std::process::id()));
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

const SHAPE: &str = "trait Shape { fn area(self) -> i64; }
struct Sq { s: i64 }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }
";

#[test]
fn a_bare_dyn_parameter_borrows_and_an_owned_one_moves() {
    for (name, body, want) in [
        (
            "read_twice",
            "fn show(s: dyn Shape) -> i64 { s.area() }
fn main() {
    let s: dyn Shape = Sq { s: 2 };
    println(show(s));
    println(show(s));
}
",
            "4\n4\n",
        ),
        (
            "own_stores",
            "fn keep(v: mut ref Vec[dyn Shape], s: own dyn Shape) { v.push(s) }
fn main() {
    let mut v: Vec[dyn Shape] = Vec.new();
    keep(mut v, Sq { s: 3 });
    println(v[0].area());
}
",
            "9\n",
        ),
        (
            "escaping_stores",
            "fn keep(v: mut ref Vec[dyn Shape], s: escaping dyn Shape) { v.push(s) }
fn main() {
    let mut v: Vec[dyn Shape] = Vec.new();
    keep(mut v, Sq { s: 4 });
    println(v[0].area());
}
",
            "16\n",
        ),
    ] {
        let src = format!("{SHAPE}{body}");
        let (ok, err) = check(name, &src);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(&src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}

#[test]
fn a_bare_dyn_parameter_cannot_be_stored() {
    let src = format!(
        "{SHAPE}fn keep(v: mut ref Vec[dyn Shape], s: dyn Shape) {{ v.push(s) }}
fn main() {{
    let mut v: Vec[dyn Shape] = Vec.new();
    keep(mut v, Sq {{ s: 3 }});
    println(v[0].area());
}}
"
    );
    let (ok, err) = check("bare_stores", &src);
    assert!(!ok, "a borrowed `dyn` parameter was stored");
    assert!(err.contains("s: own dyn Shape"), "{err}");
}
