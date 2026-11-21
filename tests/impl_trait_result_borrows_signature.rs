//! B-2026-11-20-121: what an `impl Trait` result borrows is decided by the
//! signature, not by the hidden type (design.md § `impl Trait`, "What the
//! result borrows"): the receiver for a method, otherwise every bare
//! non-`Copy` parameter, `mut ref` parameter and view parameter. A body
//! returning `xs.into_iter()`, which holds nothing, used to let the caller
//! drop `log` while the iterator was still to be used.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac <cmd> main.kara` on `src`; returns (exit code, stdout, stderr).
fn karac(src: &str, cmd: &str) -> (Option<i32>, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-itrb-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg("main.kara")
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const LOGGER: &str = "struct Logger { lines: Vec[String] }
impl Logger {
    fn write(mut ref self, s: Str) { self.lines.push(s.to_string()); }
    fn items(self) -> impl Iterator[Item = i64] { [1, 2].into_iter() }
}
fn count_logged[T](xs: own Vec[T], log: Logger) -> impl Iterator[Item = T] {
    log.lines.len();
    xs.into_iter()
}
fn borrowed_view(xs: Vec[i64]) -> impl Iterator[Item = i64] { xs.iter().map(|x| *x) }
fn make_counter() -> impl Iterator[Item = i64] { [1, 2, 3].into_iter() }
fn below(n: i64, xs: own Vec[i64]) -> impl Iterator[Item = i64] { xs.into_iter().filter(|x| *x < n) }
";

fn refused(main: &str, what: &str) {
    let (code, _, err) = karac(&format!("{LOGGER}{main}"), "check");
    assert_ne!(code, Some(0), "{err}");
    assert!(err.contains("error[E0516]") && err.contains(what), "{err}");
}

#[test]
fn impl_trait_result_keeps_a_bare_param_borrowed() {
    refused(
        "fn main() { let log = Logger { lines: Vec.new() }; let it = count_logged([1, 2, 3], log); drop(log); println(it.count()); }\n",
        "cannot move `log` while `it` still holds a borrow of it",
    );
}

#[test]
fn impl_trait_result_borrow_survives_an_adaptor() {
    refused(
        "fn main() { let log = Logger { lines: Vec.new() }; let it = count_logged([1, 2, 3], log).map(|x| x * 2); drop(log); println(it.count()); }\n",
        "cannot move `log` while `it` still holds a borrow of it",
    );
}

#[test]
fn impl_trait_result_of_a_method_borrows_its_receiver() {
    refused(
        "fn main() { let log = Logger { lines: Vec.new() }; let it = log.items(); drop(log); println(it.count()); }\n",
        "cannot move `log` while `it` still holds a borrow of it",
    );
}

#[test]
fn impl_trait_result_keeps_a_write_out() {
    refused(
        "fn main() { let mut log = Logger { lines: Vec.new() }; let mut it = count_logged([1, 2, 3], log); log.write(\"x\"); println(it.next()); }\n",
        "cannot mutably borrow `log` while `it` still holds a borrow of it",
    );
}

#[test]
fn impl_trait_result_whose_body_borrows_is_still_refused() {
    refused(
        "fn main() { let xs = [1, 2]; let it = borrowed_view(xs); drop(xs); println(it.count()); }\n",
        "cannot move `xs` while `it` still holds a borrow of it",
    );
}

#[test]
fn impl_trait_result_borrows_nothing_from_owned_and_copy_params() {
    let main = "fn main() {
    let log = Logger { lines: Vec.new() };
    let it = count_logged([1, 2, 3], log);
    println(it.count());
    drop(log);
    let c = make_counter();
    let u = below(3, [1, 2, 3, 4]);
    println(c.count() + u.count());
}
";
    let (code, out, err) = karac(&format!("{LOGGER}{main}"), "__mir-run");
    assert_eq!((code, out.as_str()), (Some(0), "3\n5\n"), "{err}");
}
