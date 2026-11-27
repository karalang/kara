//! Three programs whose check passed with nothing reported, then stopped
//! at lowering with MIR's internal E0516 "... is not lowered yet"
//! (B-2026-11-26-77):
//!
//! - a `ref` binding of a `Vec[Vector[..]]` element used as a method
//!   receiver (`let g = ref v[0]; g.reduce_sum()`): the receiver is a
//!   reference to a `Vector`, which the method dispatch now sees through,
//!   so it runs as `v[0].reduce_sum()` does;
//! - a `break` or `continue` with no label inside a closure (a
//!   `with_provider` body or a plain `||`) aimed at the loop outside it:
//!   refused E0107 by design.md § Label scope's rule, as a labeled one
//!   already was, and one outside any loop is refused too;
//! - a let-bound closure whose `return v.iter().collect()` leaves the
//!   target open: a `return` in the body takes its target from the
//!   closure's uses, as the tail call already did (B-2026-11-09-40), so
//!   the program runs; with no use deciding it, it is E0238.

mod common;

use std::process::Command;

fn karac(args: &[&str], name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-silent-refusal-{name}-{}-{}",
        args[0].trim_start_matches('_'),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` accepts `src`, and the interpreter (and, with the backend,
/// the compiled program) prints `want`.
fn accepted_and_prints(name: &str, src: &str, want: &str) {
    let check = karac(&["check"], name, src);
    assert!(check.status.success(), "{name}: check: {}", text(&check));
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for cmd in backends {
        let out = karac(&[cmd], name, src);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if *cmd != "__mir-run"
            && common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            })
            .is_none()
        {
            continue;
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{name}: {cmd}\n{src}"
        );
    }
}

/// `karac check` refuses `src` with `code` and a message holding `says`,
/// at `at`, and never with the builder's own wording.
fn refused(name: &str, src: &str, code: &str, says: &str, at: &str) {
    let out = karac(&["check"], name, src);
    let t = text(&out);
    assert!(!out.status.success(), "{name}: accepted:\n{t}");
    assert!(t.contains(&format!("error[{code}]")), "{name}: {t}");
    assert!(t.contains(says), "{name}: {t}");
    assert!(t.contains(at), "{name}: not at {at}: {t}");
    assert!(!t.contains("not lowered yet"), "{name}: {t}");
}

#[test]
fn silent_refusal_ref_vector_element_is_a_receiver() {
    accepted_and_prints(
        "ref_vector",
        "import std.simd.{Vector}; fn main() {\n    let v: Vec[Vector[i64, 4]] = [Vector[i64, 4](1, 2, 3, 4)];\n    let g = ref v[0];\n    println(g.reduce_sum());\n    let w: Vector[i64, 4] = Vector[i64, 4](1, 1, 1, 1);\n    let r = ref w;\n    println(r.reduce_sum());\n}\n",
        "10\n4\n",
    );
}

#[test]
fn silent_refusal_break_through_a_provider_closure_is_e0107() {
    refused(
        "provider_break",
        "trait Counter { fn get(ref self) -> i64; }\neffect resource Ctr: Counter;\nstruct InMem { n: i64 }\nimpl Counter for InMem { fn get(ref self) -> i64 { self.n } }\nfn read() -> i64 with reads(Ctr) { Ctr.get() }\nfn main() with writes(Stdout) {\n    let mut acc = 0;\n    for i in 0..5 {\n        acc = acc + with_provider[Ctr](InMem { n: i }, || {\n            if read() == 3 { break; }\n            read()\n        });\n    }\n    println(acc);\n}\n",
        "E0107",
        "`break` inside a closure cannot leave a loop outside it",
        "main.kara:10:30",
    );
}

#[test]
fn silent_refusal_continue_through_a_closure_is_e0107() {
    refused(
        "closure_continue",
        "fn main() {\n    for i in 0..3 {\n        let f = || { if i == 1 { continue; } i };\n        println(f());\n    }\n}\n",
        "E0107",
        "`continue` inside a closure cannot leave a loop outside it",
        "main.kara:3:34",
    );
}

#[test]
fn silent_refusal_break_and_continue_outside_a_loop_are_e0107() {
    refused(
        "bare_break",
        "fn main() {\n    break;\n}\n",
        "E0107",
        "`break` outside of a loop",
        "main.kara:2:5",
    );
    refused(
        "bare_continue",
        "fn main() {\n    let x = 'b: { if true { break 'b 1; } 2 };\n    println(x);\n    continue;\n}\n",
        "E0107",
        "`continue` outside of a loop",
        "main.kara:4:5",
    );
}

#[test]
fn silent_refusal_break_inside_a_closure_s_own_loop_still_runs() {
    accepted_and_prints(
        "own_loop",
        "fn main() {\n    let f = |n: i64| {\n        let mut s = 0;\n        for i in 0..n { if i == 3 { break; } if i == 1 { continue; } s += i; }\n        s\n    };\n    println(f(10));\n}\n",
        "2\n",
    );
}

#[test]
fn silent_refusal_returned_collect_takes_the_closure_s_use() {
    accepted_and_prints(
        "return_collect",
        "fn apply(f: Fn(i64) -> Vec[i64], n: i64) -> Vec[i64] { return f(n); }\nfn main() {\n    let v: Vec[i64] = [1, 2, 3];\n    let f = |x: i64| { return v.iter().map(|y| y + x).collect(); };\n    let inner = apply(f, 10);\n    println(inner.len());\n    let g = |x: i64| { if x > 0 { return v.iter().map(|y| y * x).collect(); } v.iter().map(|y| y - x).collect() };\n    let r: Vec[i64] = g(2);\n    println(r[2]);\n}\n",
        "3\n6\n",
    );
}

#[test]
fn silent_refusal_returned_collect_with_no_use_is_e0238() {
    let out = karac(
        &["check"],
        "return_collect_unused",
        "fn main() {\n    let v: Vec[i64] = [1, 2, 3];\n    let f = |x: i64| { return v.iter().map(|y| y + x).collect(); };\n    println(1);\n}\n",
    );
    let t = text(&out);
    assert!(!out.status.success(), "accepted:\n{t}");
    assert!(t.contains("error[E0238]"), "{t}");
    assert!(!t.contains("not lowered yet"), "{t}");
}
