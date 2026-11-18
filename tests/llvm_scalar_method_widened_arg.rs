//! B-2026-11-18-6 (kata 1603): a number's method given a narrower integer the
//! typechecker widens implicitly, `n.min(v[0])` with `n: i64` and `v:
//! Vec[i32]`, ran under the MIR interpreter and was refused by the LLVM
//! backend, ``native `i64.min` is not lowered yet``: it lowered such a
//! call only when every argument was the receiver's width or a literal.
//! The argument is now widened by its own signedness.

use std::process::{Command, Stdio};

const SRC: &str = "fn main() {
    let v: Vec[i32] = [3, -7, 2];
    let w: Vec[u8] = [200];
    let n = v.iter().filter(|x| *x > 1).count();
    println(n.min(v[0]));
    println(n.max(v[1]));
    let big: i64 = 5000000000;
    println(big.min(v[1]));
    println(big.max(w[0]));
    let u: u64 = 3;
    println(u.max(w[0]));
}
";

const OUT: &str = "2\n2\n-7\n5000000000\n200\n";

fn run(lane: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-widened-arg-{lane}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("w.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([lane, "w.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_widened_argument_to_a_number_method_runs_in_the_interpreter() {
    let (out, err) = run("__mir-run");
    assert_eq!(out, OUT, "{err}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_widened_argument_to_a_number_method_is_lowered_by_llvm() {
    let (out, err) = run("__mir-llvm-run");
    if err.contains("Linker failed") && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive");
        return;
    }
    assert_eq!(out, OUT, "{err}");
}
