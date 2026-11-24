//! design.md § 11 (Ownership and sharing) rows the 2026-11-24 sentence-map
//! audit found the compiler short of (conformance/design/ownership/
//! SENTENCE_MAP.md):
//!
//! - `a.ref_eq(b)` on two `frozen` handles was refused as an escape, though
//!   the page provides `ref_eq` on every frozen handle type (B-2026-11-24-50);
//! - the refusal of a struct literal of a `#[non_exhaustive]` type from
//!   another package named a guessed `T.new(...)`, not the package's own
//!   constructor function (B-2026-11-24-51);
//! - a frozen `let` binding that escaped was called a "parameter", and a
//!   `freeze` of a call whose argument holds a handle got a second error
//!   calling the call a temporary (B-2026-11-24-53).

use std::process::{Command, Output, Stdio};

fn karac_in(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-d11-rows-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn file(tag: &str, src: &str, args: &[&str]) -> (Output, String) {
    let dir = scratch(tag);
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    all.push("main.kara");
    let out = karac_in(&dir, &all);
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

#[test]
fn d11_ref_eq_on_two_frozen_handles_runs() {
    let src = "shared struct N { v: i64 }
fn same(a: frozen N, b: frozen N) -> bool { a.ref_eq(b) }
fn main() { let n = N { v: 1 }; let g = freeze n; let m = N { v: 1 }; let h = freeze m; println(same(g, g)); println(same(g, h)); }
";
    let (out, text) = file("refeq", src, &["check"]);
    assert!(out.status.success(), "{text}");
    let (out, text) = file("refeq-run", src, &["run", "--interp"]);
    assert!(out.status.success(), "{text}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true\nfalse\n");
}

#[test]
fn d11_a_user_ref_eq_on_a_frozen_handle_is_still_judged() {
    // A user body named `ref_eq` could keep the handle, so it is a method
    // call like any other.
    let src = "shared struct N { v: i64 }
impl N { fn ref_eq(self, o: N) -> bool { self.v == o.v } }
fn same(a: frozen N, b: frozen N) -> bool { a.ref_eq(b) }
fn main() { let n = N { v: 1 }; let g = freeze n; println(same(g, g)); }
";
    let (out, text) = file("user-refeq", src, &["check"]);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("E0511"), "{text}");
}

#[test]
fn d11_a_frozen_binding_that_escapes_is_named_a_binding() {
    let src = "shared struct N { v: i64, mut k: Vec[N] }
fn build() -> N { N { v: 1, k: Vec.new() } }
fn mk() -> N { let g = freeze build(); g }
fn main() { println(mk().v); }
";
    let (out, text) = file("binding", src, &["check"]);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("`frozen` binding `g` escapes here"), "{text}");
    assert!(!text.contains("`frozen` parameter `g`"), "{text}");
}

#[test]
fn d11_freeze_of_a_call_sharing_a_handle_is_one_error() {
    let src = "shared struct N { mut v: i64 }
fn mk(x: N) -> N { x }
fn main() { let a = N { v: 1 }; let g = freeze mk(a); println(1); }
";
    let (out, text) = file("call", src, &["check"]);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("E0512"), "{text}");
    assert!(!text.contains("temporary, not a place"), "{text}");
}

#[test]
fn d11_freeze_of_a_struct_literal_says_how_to_bind_it() {
    let src = "shared struct N { v: i64 }
fn read(n: frozen N) -> i64 { n.v }
fn main() { let g = freeze N { v: 3 }; println(read(g)); }
";
    let (out, text) = file("literal", src, &["check"]);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("its operand is a temporary, not a place: bind it to a local first"),
        "{text}"
    );
}

#[test]
fn d11_non_exhaustive_literal_names_the_packages_constructor() {
    let root = scratch("ne");
    let lib = root.join("lib");
    let app = root.join("app");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        lib.join("kara.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        app.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nlib = { path = \"../lib\" }\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("src/lib.kara"),
        "#[non_exhaustive]
pub struct Options { pub port: i64 }
pub fn options(port: i64) -> Options { Options { port: port } }
#[non_exhaustive]
pub struct Bare { pub n: i64 }
",
    )
    .unwrap();
    std::fs::write(
        app.join("src/main.kara"),
        "import lib.{Options, Bare};
fn main() { let o = Options { port: 1 }; let b = Bare { n: 2 }; println(o.port + b.n); }
",
    )
    .unwrap();
    let out = karac_in(&app, &["check"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("Construct it with the package's constructor, `lib.options(...)`,"),
        "{text}"
    );
    assert!(text.contains("it declares none"), "{text}");
}
