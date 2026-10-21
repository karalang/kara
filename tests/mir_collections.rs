//! VecDeque `clear`, `insert` and `try_insert` (B-2026-10-21-26) and
//! `StableHash.siphash24` (B-2026-10-21-27) on the v2 backends: each
//! program runs on the MIR interpreter and, compiled, at -O0 and -O2, with
//! the output docs/library/collections.md gives; `clear` also runs under
//! the ASan lane.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "karac-mir-collections-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The program's output and exit code on the compiled backend at `opt`;
/// `None` when the runtime archive is absent.
fn build_and_run(src: &str, opt: u8) -> Option<(String, i32)> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::codegen::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    Some((
        String::from_utf8(out.stdout).expect("utf-8"),
        out.status.code().unwrap_or(-1),
    ))
}

/// `src` prints `want` and exits with `code` on the interpreter and,
/// compiled, at -O0 and -O2.
fn agrees(src: &str, want: &str, code: i32) {
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "on the interpreter");
    let aborted = matches!(interp.outcome, karac::mir::interp::Outcome::Aborted(_));
    assert_eq!(
        aborted,
        code == 101,
        "interpreter outcome {:?}",
        interp.outcome
    );
    for opt in [0, 2] {
        if let Some((out, exit)) = build_and_run(src, opt) {
            assert_eq!(out, want, "compiled at -O{opt}");
            assert_eq!(exit, code, "exit code compiled at -O{opt}");
        }
    }
}

/// The program's stdout under the ASan lane, which fails on a leak, double
/// free or use-after-free; `None` without an ASan toolchain.
fn under_asan(src: &str) -> Option<String> {
    let dir = Scratch::new();
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&file)
        .output()
        .expect("karac");
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("not found") || err.contains("fsanitize") {
        eprintln!("skipping: no ASan toolchain or runtime archive: {err}");
        return None;
    }
    assert!(!err.contains("Sanitizer"), "{err}");
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `VecDeque.insert` at the front, the back and the middle, across the
/// ring's wrap; `try_insert`; `clear`, then reuse. collections.md:
/// `insert(i, x)` shifts the rest, and `clear` empties the deque.
const DEQUE: &str = r#"struct Noisy { id: i64 }
impl Drop for Noisy {
    fn drop(mut ref self) { println(f"drop {self.id}"); }
}
fn show(d: VecDeque[i64]) -> String {
    let mut out = String.new();
    let mut i = 0;
    while i < d.len() {
        match d.get(i) { Some(x) => out.push_str(f"{x} "), None => out.push_str("? ") }
        i += 1;
    }
    out
}
fn main() {
    let mut d: VecDeque[i64] = VecDeque.new();
    d.insert(0, 2);
    d.insert(0, 1);
    d.insert(2, 4);
    d.insert(2, 3);
    println(show(d));
    d.push_front(0);
    d.push_front(-1);
    d.insert(1, 9);
    println(show(d));
    match d.try_insert(7, 5) { Ok(_) => println("ok"), Err(_) => println("err") }
    println(show(d));
    d.clear();
    println(f"{d.len()} {d.is_empty()}");
    d.push_back(6);
    println(show(d));
    let mut s: VecDeque[String] = VecDeque.new();
    s.push_back("a".to_string());
    s.push_front("b".to_string());
    s.insert(1, "c".to_string());
    println(f"{s.len()}");
    s.clear();
    s.push_back("d".to_string());
    println(f"{s.len()}");
    let mut n: VecDeque[Noisy] = VecDeque.new();
    n.push_back(Noisy { id: 1 });
    n.push_back(Noisy { id: 2 });
    n.insert(1, Noisy { id: 3 });
    n.clear();
    println("cleared");
    n.push_back(Noisy { id: 4 });
    println("end");
}
"#;

const WANT_DEQUE: &str = "1 2 3 4 \n-1 9 0 1 2 3 4 \nok\n-1 9 0 1 2 3 4 5 \n0 true\n6 \n3\n1\n\
                          drop 1\ndrop 3\ndrop 2\ncleared\nend\ndrop 4\n";

#[test]
fn vecdeque_insert_and_clear_agree_on_both_backends() {
    agrees(DEQUE, WANT_DEQUE, 0);
}

/// `clear` drops what it removes: the `String` elements are freed once,
/// and the deque is usable after.
#[test]
fn vecdeque_clear_is_clean_under_asan() {
    if let Some(out) = under_asan(DEQUE) {
        assert_eq!(out, WANT_DEQUE);
    }
}

/// An index past the length panics, as `Vec.insert` does.
#[test]
fn vecdeque_insert_past_the_end_panics() {
    agrees(
        "fn main() {\n    let mut d: VecDeque[i64] = VecDeque.new();\n    d.push_back(1);\n    \
         println(\"before\");\n    d.insert(2, 5);\n    println(\"after\");\n}\n",
        "before\n",
        101,
    );
}

/// The SipHash-2-4 paper's key `00 01 .. 0f` on the empty input and on
/// `00 .. 0e`, and a zero key on a `Str`'s bytes.
const SIPHASH: &str = r#"fn content_id(payload: Str) -> u64 {
    StableHash.siphash24(payload.bytes(), 0, 0)
}
fn main() {
    let empty: Vec[u8] = Vec.new();
    println(f"{StableHash.siphash24(empty, 506097522914230528, 1084818905618843912)}");
    let fifteen: Vec[u8] = Vec.from_fn(15, |i| i as u8);
    println(f"{StableHash.siphash24(fifteen, 506097522914230528, 1084818905618843912)}");
    println(f"{content_id("x") == content_id("x")} {content_id("x") == content_id("y")}");
}
"#;

#[test]
fn stable_siphash24_agrees_on_both_backends() {
    let want = format!(
        "{}\n{}\ntrue false\n",
        karac_hash::siphash24(&[], 0x0706050403020100, 0x0f0e0d0c0b0a0908),
        karac_hash::siphash24(
            &(0u8..15).collect::<Vec<u8>>(),
            0x0706050403020100,
            0x0f0e0d0c0b0a0908
        ),
    );
    assert_eq!(
        want,
        "8246050544436514353\n11613035633349379557\ntrue false\n"
    );
    agrees(SIPHASH, &want, 0);
}
