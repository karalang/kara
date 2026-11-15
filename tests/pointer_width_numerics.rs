//! B-2026-11-15-88: `isize.abs_diff` was typed `isize`, not `usize`, so
//! `isize.MIN.abs_diff(0)` printed negative on the compiled backend and
//! `let e: usize = d` was refused. B-2026-11-15-89: an unsuffixed literal
//! past `i64.MAX` at a `usize` context was refused as out of range for
//! `i64`; it now accepts what the `usize` suffix accepts, and an
//! out-of-range literal names the context type.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(tag: &str, src: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("karac-ptrwidth-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f.to_str().unwrap().to_string())
}

fn run_ok(tag: &str, src: &str) -> String {
    let (dir, p) = scratch(tag, src);
    let check = karac(&["check", &p]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let run = karac(&["run", "--interp", &p]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[test]
fn isize_abs_diff_is_a_usize() {
    let src = "fn o(x: isize) -> isize {\n    x\n}\n\nfn main() {\n    \
let d = o(isize.MIN).abs_diff(o(0));\n    let e: usize = d;\n    println(e);\n    \
println(d > 0);\n    println(o(isize.MIN).abs_diff(o(isize.MAX)));\n}\n";
    assert_eq!(
        run_ok("absdiff", src),
        "9223372036854775808\ntrue\n18446744073709551615\n"
    );
}

#[test]
fn a_literal_past_i64_max_fits_a_usize_context() {
    let src = "fn o_usize(x: usize) -> usize {\n    x\n}\n\nfn main() {\n    \
let a: usize = 18446744073709551614;\n    println(a);\n    \
println(o_usize(9223372036854775808));\n    let m: usize = 18446744073709551615;\n    \
println(m == usize.MAX);\n    let c: isize = 9223372036854775807;\n    println(c);\n}\n";
    assert_eq!(
        run_ok("lit", src),
        "18446744073709551614\n9223372036854775808\ntrue\n9223372036854775807\n"
    );
}

#[test]
fn a_literal_out_of_range_names_the_pointer_width_type() {
    let src = "fn main() {\n    let a: usize = 18446744073709551616;\n    \
let b: isize = 9223372036854775808;\n    println(a);\n    println(b);\n}\n";
    let (dir, p) = scratch("range", src);
    let check = karac(&["check", &p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success(), "{text}");
    assert!(
        text.contains("integer literal 18446744073709551616 out of range for 'usize'"),
        "{text}"
    );
    assert!(
        text.contains("integer literal 9223372036854775808 out of range for 'isize'"),
        "{text}"
    );
    assert!(!text.contains("for 'i64'"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
