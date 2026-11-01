//! A string method's index, count or length takes a borrowed integer the way
//! an `i64` parameter does: `for len in lens.iter()` yields `ref i64`, and
//! `s.substring(0, len)` reads it as its value (B-2026-11-01-4). The same
//! holds for `char_at`, `repeat`, `slice` and `String.truncate`, on the
//! interpreter and compiled.

mod common;

use std::process::Command;

/// `src`'s standard output and whether it exited cleanly, on the MIR
/// interpreter (`karac __mir-run`).
fn interp_stdout(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-str-ref-int-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The same, compiled at `opt`; `None` without the llvm feature or a
/// runtime archive.
#[cfg(feature = "llvm")]
fn compiled_stdout(src: &str, opt: u8) -> Option<(bool, String)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("karac-str-ref-int-b-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

#[cfg(not(feature = "llvm"))]
fn compiled_stdout(_: &str, _: u8) -> Option<(bool, String)> {
    None
}

const PROG: &str = r#"fn main() {
    let s = "abcdef".to_string();
    let lens: Vec[i64] = [1, 3];
    for len in lens.iter() {
        println(s.substring(0, len));
        println(f"{s.char_at(len)}");
        println("x".repeat(len));
        println(s.slice(len, 4));
    }
    let mut t = "hello".to_string();
    for len in lens.iter() {
        t.truncate(len);
    }
    println(t);
}
"#;

const WANT: &str = "a\nSome(b)\nx\nbcd\nabc\nSome(d)\nxxx\nd\nh\n";

#[test]
fn string_methods_take_a_borrowed_integer_index() {
    assert!(
        karac::check_source(PROG).is_empty(),
        "{:?}",
        karac::check_source(PROG)
    );
    let (ok, out) = interp_stdout("idx", PROG);
    assert!(ok, "{out}");
    assert_eq!(out, WANT, "interpreter");
    for opt in [0, 2] {
        if let Some((ok, out)) = compiled_stdout(PROG, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, WANT, "at -O{opt}");
        }
    }
}
