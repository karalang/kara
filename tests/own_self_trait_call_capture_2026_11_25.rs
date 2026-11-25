//! B-2026-11-25-46: a closure that calls an `own self` trait method on a
//! captured `dyn W` or generic `T: W` value (`|| a.w()`) moves the value in,
//! as it does for a concrete receiver (design.md § Captures: a move use
//! captures by move). It captured by reference and the call was refused
//! E0516, in a `OnceFn` argument, in `TaskGroup.spawn` and in a local closure.

use std::path::PathBuf;
use std::process::Command;

const W: &str = "import std.sync.{TaskGroup};\ntrait W { fn w(own self) -> String; }\nstruct A { s: String }\nimpl W for A { fn w(own self) -> String { self.s + \"!\" } }\nfn run(f: own OnceFn() -> String) -> String { f() }\n";

const FORMS: &[(&str, &str)] = &[
    (
        "dyn_oncefn_argument",
        "fn main() {\n    let a: dyn W = A { s: \"x\".to_string() };\n    println(run(|| a.w()));\n}\n",
    ),
    (
        "dyn_spawned",
        "fn main() {\n    let a: dyn W + CrossTask = A { s: \"x\".to_string() };\n    let mut r = \"\".to_string();\n    {\n        let tg = TaskGroup.new();\n        let h = tg.spawn(|| a.w());\n        r = h.join();\n    }\n    println(r);\n}\n",
    ),
    (
        "dyn_local_closure",
        "fn main() {\n    let a: dyn W = A { s: \"x\".to_string() };\n    let f = || a.w();\n    println(f());\n}\n",
    ),
    (
        "generic_oncefn_argument",
        "fn g[T: W](a: own T) -> String { run(|| a.w()) }\nfn main() { println(g(A { s: \"x\".to_string() })); }\n",
    ),
    (
        "generic_spawned",
        "fn g[T: W + CrossTask](a: own T) -> String {\n    let mut r = \"\".to_string();\n    {\n        let tg = TaskGroup.new();\n        let h = tg.spawn(|| a.w());\n        r = h.join();\n    }\n    r\n}\nfn main() { println(g(A { s: \"x\".to_string() })); }\n",
    ),
    (
        "generic_local_closure",
        "fn g[T: W](a: own T) -> String {\n    let f = || a.w();\n    f()\n}\nfn main() { println(g(A { s: \"x\".to_string() })); }\n",
    ),
];

fn program(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-own-self-capture-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, format!("{W}{body}")).unwrap();
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
fn own_self_trait_calls_in_closures_move_the_receiver_on_the_interpreter() {
    for (name, body) in FORMS {
        let file = program(&format!("interp-{name}"), body);
        let f = file.to_str().unwrap();
        let (ok, out) = karac(&["check", f]);
        assert!(ok, "{name}: {out}");
        let (ok, out) = karac(&["__mir-run", f]);
        assert!(ok, "{name}: {out}");
        assert_eq!(out, "x!\n", "{name}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn own_self_trait_calls_in_closures_move_the_receiver_compiled() {
    for (name, body) in FORMS {
        let file = program(&format!("run-{name}"), body);
        let (ok, out) = karac(&["run", file.to_str().unwrap()]);
        assert!(ok, "{name}: {out}");
        assert_eq!(out, "x!\n", "{name}");
    }
}

#[test]
fn a_ref_self_trait_call_in_a_closure_still_borrows_the_receiver() {
    // The receiver stays usable after the closure when the trait borrows it.
    let file = program(
        "ref-self",
        "trait R { fn r(self) -> i64; }\nimpl R for A { fn r(self) -> i64 { self.s.len() as i64 } }\nfn main() {\n    let a: dyn R = A { s: \"xy\".to_string() };\n    let f = || a.r();\n    println(f());\n    println(a.r());\n}\n",
    );
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, "2\n2\n");
}
