//! A call through a resource bounded by a generic trait (`effect resource
//! Sink: Store[i64]`) passes each argument in the mode the trait's method
//! declares: a bare `v: T` borrows under D5, so the provider's impl takes
//! `ref i64` and the call lends even a `Copy` argument (B-2026-10-31-27).

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-provider-generic-trait-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .current_dir(path.parent().unwrap())
        .env("KARAC_AUTO_PAR", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn all(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` passes and the MIR interpreter prints `expected`; under
/// `llvm`, `karac run` prints it too.
fn checks_and_prints(tag: &str, src: &str, expected: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", all(&out));
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("run");
    }
    for cmd in cmds {
        let out = karac(&[cmd], &path);
        assert!(out.status.success(), "{cmd}: {}", all(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{cmd}");
    }
}

#[test]
fn a_generic_trait_resource_lends_a_copy_argument_to_its_provider() {
    checks_and_prints(
        "copy-arg",
        "pub trait Channel[T] {\n    fn send(ref self, v: T) -> T;\n}\n\n\
         pub effect resource RequestCh: Channel[i64];\n\n\
         pub struct Echo {\n    bump: i64,\n}\n\n\
         impl Channel[i64] for Echo {\n    fn send(ref self, v: i64) -> i64 {\n        v + self.bump\n    }\n}\n\n\
         pub fn push(v: i64) -> i64 with reads(RequestCh) {\n    RequestCh.send(v)\n}\n\n\
         fn main() {\n    with_provider[RequestCh](Echo { bump: 1 }, || {\n        println(f\"{push(41)}\");\n    });\n}\n",
        "42\n",
    );
}

#[test]
fn a_generic_trait_resource_passes_ref_and_mut_ref_modes_per_instance() {
    checks_and_prints(
        "modes",
        "pub trait Store[T] {\n    fn put(ref self, v: T) -> i64;\n    fn bump(ref self, n: mut ref T);\n}\n\n\
         pub effect resource Sink: Store[i64];\npub effect resource Names: Store[String];\n\n\
         pub struct Count {\n    base: i64,\n}\n\n\
         impl Store[i64] for Count {\n    fn put(ref self, v: i64) -> i64 {\n        v + self.base\n    }\n\n    \
         fn bump(ref self, n: mut ref i64) {\n        *n = *n + self.base;\n    }\n}\n\n\
         pub struct Lens {\n    base: i64,\n}\n\n\
         impl Store[String] for Lens {\n    fn put(ref self, v: String) -> i64 {\n        v.len() + self.base\n    }\n\n    \
         fn bump(ref self, n: mut ref String) {\n        n.push_str(\"!\");\n    }\n}\n\n\
         pub fn add(v: i64) -> i64 with reads(Sink) {\n    let mut x = v;\n    Sink.bump(mut x);\n    Sink.put(x)\n}\n\n\
         fn name(s: String) -> i64 with reads(Names) writes(Stdout) {\n    let mut t = s.clone();\n    Names.bump(mut t);\n    \
         println(t);\n    Names.put(s)\n}\n\n\
         fn main() {\n    with_provider[Sink](Count { base: 10 }, || {\n        println(add(1));\n    });\n    \
         with_provider[Names](Lens { base: 1 }, || {\n        println(name(\"abc\".to_string()));\n    });\n}\n",
        "21\nabc!\n4\n",
    );
}
