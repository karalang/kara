//! Kata 2267's bits arm fills a `Vec[u128]` of masks with a literal,
//! `Vec.filled(n, 0)`. The literal reached the native as an `i64`
//! constant in an 8-byte slot while the native copied the element's 16
//! bytes from it, so under the compiled backend the high half of every
//! element after the first was whatever lay past the slot, and
//! AddressSanitizer reported a stack-buffer-overflow (B-2026-11-18-81). The fill
//! is now fitted to the element type, as `push` fits its value.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

const SRC: &str = "fn main() {
    let v: Vec[u128] = Vec.filled(3, 5);
    println(f\"{v[0]} {v[1]} {v[2]}\");
    let w: Vec[i128] = Vec.filled(2, -1);
    println(f\"{w[0]} {w[1]}\");
    let f: Vec[f64] = Vec.filled(2, 1.5);
    println(f\"{f[1]}\");
}
";

const WANT: &str = "5 5 5\n-1 -1\n1.5\n";

#[test]
fn kata_2267_filled_wide_literal_interp() {
    let p = write("k2267-fill-interp", SRC);
    let out = karac(&["__mir-run", &p]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn kata_2267_filled_wide_literal_llvm() {
    let p = write("k2267-fill-llvm", SRC);
    let out = karac(&["__mir-llvm-run", &p]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT);
}
