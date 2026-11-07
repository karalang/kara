//! design.md §4 second-pass probe rows. B-2026-11-06-22: a `#[target(..)]`
//! gate on an impl member, a struct field or an enum variant removes the
//! member on a target its condition does not hold for, as one on an item
//! does. B-2026-11-06-23: a platform entry file (`src/main_linux.kara`)
//! replaces `src/main.kara` on its platform, an unknown edition is reported
//! in §4's words, and a constant of a struct whose field holds a `String`
//! is refused like a `String` constant. `karac init` takes `--cli` and
//! `--backend` as §4 lists them (7ac213a162).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static ID: AtomicU32 = AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = ID.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-s4gate-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn run_file(tag: &str, src: &str) -> (bool, String, String) {
    let dir = scratch(tag);
    write(&dir, "main.kara", src);
    let o = karac(&dir, &["run", "main.kara"]);
    let (out, err) = text(&o);
    let _ = std::fs::remove_dir_all(&dir);
    (o.status.success(), out, err)
}

fn check_file(tag: &str, src: &str) -> (bool, String) {
    let dir = scratch(tag);
    write(&dir, "main.kara", src);
    let o = karac(&dir, &["check", "main.kara"]);
    let (out, err) = text(&o);
    let _ = std::fs::remove_dir_all(&dir);
    (o.status.success(), out + &err)
}

#[test]
fn target_gated_twin_methods_pick_the_native_one() {
    let (ok, out, err) = run_file(
        "twin",
        "struct S {}

impl S {
    #[target(native)]
    fn f(self) -> i64 {
        1
    }

    #[target(wasm_wasi)]
    fn f(self) -> i64 {
        2
    }
}

fn main() {
    println(S {}.f());
}
",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "1\n");
}

#[test]
fn target_gated_fields_and_variants_are_absent_off_target() {
    let (ok, out, err) = run_file(
        "field_variant",
        "struct S {
    a: i64,
    #[target(wasm_wasi)]
    b: i64,
}

enum E {
    A,
    #[target(wasm_wasi)]
    B,
}

fn main() {
    let s = S { a: 1 };
    let e = E.A;
    match e {
        E.A => println(f\"A {s.a}\"),
    }
}
",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "A 1\n");
}

#[test]
fn check_still_reads_a_member_gated_to_another_target() {
    let (ok, out) = check_file(
        "wasm_member_body",
        "struct S {}

impl S {
    #[target(native)]
    fn f(self) -> i64 {
        1
    }

    #[target(wasm_wasi)]
    fn f(self) -> i64 {
        \"two\"
    }
}

fn main() {
    println(S {}.f());
}
",
    );
    assert!(!ok, "the wasm member's type error must be reported:\n{out}");
    assert!(out.contains("wasm_wasi"), "{out}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_platform_entry_file_replaces_main_on_its_platform() {
    let dir = scratch("main_linux");
    write(
        &dir,
        "kara.toml",
        "[package]\nname = \"pick\"\nversion = \"0.1.0\"\n",
    );
    write(
        &dir,
        "src/main.kara",
        "fn main() {\n    println(\"shared\");\n}\n",
    );
    write(
        &dir,
        "src/main_linux.kara",
        "fn main() {\n    println(\"linux\");\n}\n",
    );
    write(
        &dir,
        "src/main_macos.kara",
        "fn main() {\n    println(\"macos\");\n}\n",
    );
    let o = karac(&dir, &["run"]);
    let (out, err) = text(&o);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(o.status.success(), "{err}");
    assert_eq!(out, "linux\n");
}

#[test]
fn an_unknown_edition_is_reported_in_the_spec_words() {
    let dir = scratch("edition");
    write(
        &dir,
        "kara.toml",
        "[package]\nname = \"ed\"\nversion = \"0.1.0\"\nedition = \"2027\"\n",
    );
    write(&dir, "src/main.kara", "fn main() {}\n");
    let o = karac(&dir, &["check"]);
    let (out, err) = text(&o);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!o.status.success());
    let all = out + &err;
    assert!(
        all.contains("unknown edition \"2027\"; this compiler supports editions up to \"2026\""),
        "{all}"
    );
}

#[test]
fn a_constant_of_a_struct_holding_a_string_is_refused() {
    for (tag, src, owner) in [
        (
            "direct",
            "struct Holder {
    n: String,
}

const HELD: Holder = Holder { n: \"a\" };

fn main() {
    let h = HELD;
    println(h.n);
}
",
            "Holder.n",
        ),
        (
            "nested",
            "struct Inner {
    tag: Option[String],
}

struct Outer {
    k: i64,
    inner: Inner,
}

const HELD: Outer = Outer { k: 1, inner: Inner { tag: None } };

fn main() {
    println(HELD.k);
}
",
            "Inner.tag",
        ),
    ] {
        let (ok, out) = check_file(tag, src);
        assert!(!ok, "{tag}: {out}");
        assert!(out.contains("E_MODULE_BINDING_HEAP_TYPE"), "{tag}: {out}");
        assert!(out.contains(owner), "{tag}: {out}");
    }
    let (ok, out) = check_file(
        "str_field",
        "struct Holder {
    n: Str,
}

const HELD: Holder = Holder { n: \"a\" };

fn main() {
    let h = HELD;
    println(h.n);
}
",
    );
    assert!(ok, "a `Str` field is constant data: {out}");
}

#[test]
fn init_takes_the_cli_and_backend_scaffolds() {
    for (flag, marker) in [("--cli", "read_to_string"), ("--backend", "8080")] {
        let dir = scratch(&format!("init{flag}"));
        let o = karac(&dir, &["init", "tool", flag]);
        let (_, err) = text(&o);
        assert!(o.status.success(), "{flag}: {err}");
        let main = std::fs::read_to_string(dir.join("tool/src/main.kara")).unwrap();
        assert!(main.contains(marker), "{flag}: {main}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    let dir = scratch("init_two");
    let o = karac(&dir, &["init", "tool", "--cli", "--lib"]);
    let (_, err) = text(&o);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!o.status.success());
    assert!(err.contains("mutually exclusive"), "{err}");
}
