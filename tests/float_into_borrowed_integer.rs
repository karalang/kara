//! B-2026-11-09-19: a float handed to an integer slot through a borrow was
//! accepted. `Vec[u8].contains(d)` with `let d = 1.5` checked clean, then
//! the interpreter stopped on "Eq on Int(1) and Float(1.5)" and the binary
//! printed `false`; a `ref u8` or `mut ref u8` parameter took a float the
//! same way, while a by-value `u8` parameter and `Set[u8].contains` refused
//! it (design.md: a float does not implicitly convert to an integer). The
//! refusal now looks through the borrow. The needles the element's view
//! rule allows (B-2026-11-08-55) still check and run.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

/// `karac check`'s errors, or "" when it passes.
fn check_err(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-float-ref-int-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    if out.status.success() {
        String::new()
    } else {
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn a_float_needle_on_a_vec_of_u8_is_refused() {
    let err = check_err(
        "fn main() {
    let v: Vec[u8] = [1];
    let d = 1.5;
    println(v.contains(d));
}
",
    );
    assert!(
        err.contains("expected 'ref u8', found 'f64'; a float does not implicitly convert to an integer — use `as u8` to truncate"),
        "{err}"
    );
}

#[test]
fn a_float_needle_on_a_deque_of_u8_is_refused() {
    let err = check_err(
        "fn main() {
    let v: VecDeque[u8] = VecDeque.new();
    let d = 2.5;
    println(v.contains(d));
}
",
    );
    assert!(err.contains("expected 'ref u8', found 'f64'"), "{err}");
}

#[test]
fn a_float_into_a_ref_integer_parameter_is_refused() {
    let err = check_err(
        "fn is_one(x: ref u8) -> bool {
    x == 1
}

fn main() {
    let d = 1.5;
    println(is_one(d));
}
",
    );
    assert!(err.contains("expected 'ref u8', found 'f64'"), "{err}");
}

#[test]
fn a_float_into_a_mut_ref_integer_parameter_is_refused() {
    let err = check_err(
        "fn bump(x: mut ref u8) {
    x += 1;
}

fn main() {
    let mut d = 1.5;
    bump(mut d);
    println(d);
}
",
    );
    assert!(
        err.contains("expected 'mut ref u8', found 'f64'; a float is not an integer, so it cannot be written through a `mut ref u8`"),
        "{err}"
    );
}

#[test]
fn the_element_and_its_view_are_still_accepted_needles() {
    let src = "fn main() {
    let names: Vec[String] = [\"a\".to_string(), \"b\".to_string()].to_vec();
    let s: Str = \"b\";
    println(names.contains(s));
    let rows: Vec[Vec[i64]] = [[1, 2].to_vec()].to_vec();
    let probe: Vec[i64] = [1, 2].to_vec();
    println(rows.contains(probe.as_slice()));
    let v: Vec[u8] = [1];
    let one: u8 = 1;
    println(v.contains(one));
    let w: Vec[f64] = [1.5].to_vec();
    let d = 1.5;
    println(w.contains(d));
}
";
    assert_eq!(check_err(src), "");
    assert_eq!(run(src), "true\ntrue\ntrue\ntrue\n");
}
