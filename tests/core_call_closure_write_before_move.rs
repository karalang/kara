//! A closure literal passed to a named function as a non-escaping argument
//! (an `own MutFn` parameter, `with_provider[R](p, || ..)`) is called, if at
//! all, before the call returns (core semantics §9.3), so moving a binding
//! it writes after the call is fine. The ownership pass paired the body's
//! write with the later move as a reuse ("moved inside a loop", with no loop
//! in the program), the argument-position case B-2026-10-29-37 and
//! B-2026-10-29-43 left (B-2026-10-31-13). The pair inside a loop, a move
//! before the call, and an escaping parameter are still refused.

use std::process::Command;

/// `karac <cmd>` on `src`: whether it succeeded, and its stdout and stderr.
fn karac(cmd: &str, name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-callclo-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const OWN_MUTFN: &str = "fn run(f: own MutFn() -> ()) {\n    f();\n}\n\nfn main() {\n    let mut got: Vec[String] = Vec.new();\n    run(|| {\n        got.push(\"x\");\n    });\n    let all = got;\n    println(f\"{all.len()}\");\n}\n";

const WITH_PROVIDER: &str = "effect resource Log: LogSink;\n\ntrait LogSink {\n    fn note(self, s: String) -> String;\n}\n\nstruct Plain {}\n\nimpl LogSink for Plain {\n    fn note(self, s: String) -> String {\n        f\"[{s}]\"\n    }\n}\n\nfn main() {\n    let lines = [\"a\", \"b\"];\n    let mut got: Vec[String] = Vec.new();\n    with_provider[Log](Plain {}, || {\n        for line in lines {\n            got.push(Log.note(line));\n        }\n    });\n    let all = got;\n    println(f\"{all.len()} {all[0]}\");\n}\n";

const IN_A_LOOP: &str = "fn run(f: own MutFn() -> ()) {\n    f();\n}\n\nfn main() {\n    let mut got: Vec[String] = Vec.new();\n    for i in 0..2 {\n        run(|| {\n            got.push(\"x\");\n        });\n        let all = got;\n        println(f\"{all.len()} {i}\");\n    }\n}\n";

const ESCAPING: &str = "fn stash(hs: mut ref Vec[MutFn() -> ()], f: escaping MutFn() -> ()) {\n    hs.push(f);\n}\n\nfn main() {\n    let mut hs: Vec[MutFn() -> ()] = Vec.new();\n    let mut got: Vec[String] = Vec.new();\n    stash(mut hs, || {\n        got.push(\"x\");\n    });\n    let all = got;\n    println(f\"{all.len()}\");\n}\n";

const MOVED_BEFORE: &str = "fn run(f: own MutFn() -> ()) {\n    f();\n}\n\nfn main() {\n    let mut got: Vec[String] = Vec.new();\n    let all = got;\n    run(|| {\n        got.push(\"x\");\n    });\n    println(f\"{all.len()}\");\n}\n";

#[test]
fn a_binding_moved_after_a_call_whose_closure_writes_it_is_accepted() {
    for (name, src, want) in [
        ("own_mutfn", OWN_MUTFN, "1\n"),
        ("with_provider", WITH_PROVIDER, "2 [a]\n"),
    ] {
        let (ok, _, err) = karac("check", name, src);
        assert!(ok, "{name}: {err}");
        let (ok, out, err) = karac("__mir-run", name, src);
        assert!(ok, "{name}: {err}");
        assert_eq!(out, want, "{name}");
    }
}

#[test]
fn the_pair_in_a_loop_a_move_before_and_an_escaping_closure_are_still_refused() {
    for (name, src, want) in [
        ("in_a_loop", IN_A_LOOP, "is moved inside a loop"),
        ("escaping", ESCAPING, "moved into an escaping closure"),
        ("moved_before", MOVED_BEFORE, "moved here, used again here"),
    ] {
        let (ok, _, err) = karac("check", name, src);
        assert!(!ok, "{name} must be refused");
        assert!(err.contains(want), "{name}: {err}");
    }
}
