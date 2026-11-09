//! B-2026-11-09-43, found building the tus_upload app: `let t = text?;` on a
//! bare `text: Option[String]` parameter, which reads as `ref Option[String]`
//! (parameters borrow by default), was refused as "'?' operator requires
//! `Result` or `Option`, found 'ref Option[String]'", about a value that is
//! one, with no fix. `?` yields the payload and propagates the error by
//! value, so a non-`Copy` one would move out of the borrow: the refusal now
//! says that and offers `.clone()`, as `??` does (B-2026-10-28-32). A `Copy`
//! payload and error are read out of the borrow and the program runs.

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn scratch(src: &str) -> std::path::PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-question-borrow-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn check_err(src: &str) -> String {
    let path = scratch(src);
    let out = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    if out.status.success() {
        String::new()
    } else {
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

/// The program after `karac fix`, which must then check clean.
fn fixed(src: &str) -> String {
    let path = scratch(src);
    let file = path.to_str().unwrap().to_string();
    let _ = karac(&["fix", &file]);
    let out = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}\n{out}",
        String::from_utf8_lossy(&check.stderr)
    );
    out
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const OPTION_STRING: &str = "fn first_len(text: Option[String]) -> Option[i64] {
    let t = text?;
    Some(t.len())
}

fn main() {
    println(first_len(Some(\"abc\".to_string())));
    println(first_len(None));
}
";

const RESULT_STRING: &str = "fn ok_len(r: Result[String, String]) -> Result[i64, String] {
    let t = r?;
    Ok(t.len())
}

fn main() {
    println(ok_len(Ok(\"abcd\".to_string())));
    println(ok_len(Err(\"bad\".to_string())));
}
";

const COPY: &str = "fn next_of(n: Option[i64]) -> Option[i64] {
    let v = n?;
    Some(v + 1)
}

fn doubled(r: Result[i64, i64]) -> Result[i64, i64] {
    let v = r?;
    Ok(v * 2)
}

fn main() {
    println(next_of(Some(4)));
    println(next_of(None));
    println(doubled(Ok(5)));
    println(doubled(Err(9)));
    let xs: Vec[Option[i64]] = [Some(1), None, Some(3)].to_vec();
    let mut total = 0;
    for x in xs.iter() {
        total += next_of(*x) ?? 0;
    }
    println(total);
}
";

#[test]
fn question_on_a_borrowed_option_of_string_offers_a_clone() {
    let err = check_err(OPTION_STRING);
    assert!(
        err.contains("'?' on a borrowed 'Option[String]' would move its payload ('String') out of the borrow"),
        "{err}"
    );
    assert!(!err.contains("requires `Result` or `Option`"), "{err}");
    let out = fixed(OPTION_STRING);
    assert!(out.contains("let t = text.clone()?;"), "{out}");
    assert_eq!(run(&out), "Some(3)\nNone\n");
}

#[test]
fn question_on_a_borrowed_result_of_string_offers_a_clone() {
    let err = check_err(RESULT_STRING);
    assert!(
        err.contains("'?' on a borrowed 'Result[String, String]' would move its value or error ('String') out of the borrow"),
        "{err}"
    );
    let out = fixed(RESULT_STRING);
    assert!(out.contains("let t = r.clone()?;"), "{out}");
    assert_eq!(run(&out), "Ok(4)\nErr(bad)\n");
}

const COPY_OUT: &str = "Some(5)\nNone\nOk(10)\nErr(9)\n6\n";

#[test]
fn question_on_a_borrowed_copy_wrapper_reads_it() {
    assert_eq!(check_err(COPY), "");
    assert_eq!(run(COPY), COPY_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn question_on_a_borrowed_copy_wrapper_reads_it_compiled() {
    let lowered = karac::mir::lower::build_source(COPY).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-question-borrow-b-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let Some(out) = out else { return };
    assert!(out.status.success(), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stdout), COPY_OUT);
}
