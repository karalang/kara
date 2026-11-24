//! B-2026-11-20-151: `String.from` named as a value, Rust's
//! `.map(String::from)` as `karac fix` writes it. The checker typed every
//! such value `Fn(char) -> String`, whatever its use passed, and the MIR
//! builder refused the path as "not lowered yet" with nothing to write
//! instead. The value now takes the parameter its use gives it (a `Str`
//! view, a borrowed or owned `String`, a `char`) and lowers to the
//! library's wrapper for that form; `String.from_utf8` and
//! `String.from_utf8_lossy` likewise. The interpreter's `String.from` also
//! reads a borrowed `String`, which it refused while the compiled program
//! copied it.

use std::path::PathBuf;
use std::process::Command;

const PROGRAM: &str = "fn conv(t: ref String) -> String {
    String.from(t)
}

fn main() {
    let words = [\"a\", \"\", \"bc\"];
    let v: Vec[String] = words.iter().filter(|s| not s.is_empty()).map(String.from).collect();
    println(v.len());
    println(v[1]);
    let parts: Vec[String] = \"x,y\".split(\",\").map(String.from).collect();
    println(parts[0] + parts[1]);
    let cs: Vec[String] = ['p', 'q'].iter().map(String.from).collect();
    println(cs[0] + cs[1]);
    let owned: Vec[String] = words.into_iter().map(String.from).collect();
    println(owned.len());
    let f: Fn(char) -> String = String.from;
    println(f('z'));
    let raw: Vec[Vec[u8]] = [[104u8, 105u8]];
    let ok: Vec[Result[String, Utf8Error]] = raw.iter().map(String.from_utf8).collect();
    println(ok.len());
    let lossy: Vec[String] = raw.iter().map(String.from_utf8_lossy).collect();
    println(lossy[0]);
    println(conv(String.from(\"ab\")));
}
";

const EXPECTED: &str = "2\nbc\nxy\npq\n3\nz\n1\nhi\nab\n";

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-string-from-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn string_from_as_a_value_checks_and_interprets() {
    let file = program("interp", PROGRAM);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn string_from_as_a_value_runs_compiled() {
    let file = program("run", PROGRAM);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, EXPECTED);
}

#[test]
fn string_from_as_a_value_takes_its_parameter_from_the_annotation() {
    // The annotation fixes the parameter, as a use does: a `Str` here, where
    // the value used to be `Fn(char) -> String` and refused it.
    let file = program(
        "annot",
        "fn main() {\n    let f: Fn(Str) -> String = String.from;\n    println(f(\"view\"));\n}\n",
    );
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, "view\n");
}
