//! The library's two conversions into `IoError` coexist (io.md § Errors,
//! B-2026-11-17-7): `impl From[VarError] for IoError` and
//! `impl From[NetError] for IoError`, "so a function that uses `?` on file
//! and socket operations alike can return IoError alone". The `NetError`
//! impl is written in library source and the `VarError` one is baked; the
//! source impl used to replace the baked one, so a program that used both
//! was refused. Both backends must print what io.md's mapping gives.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

/// `.into()` and `IoError.from` from both sources in one program.
const BOTH: &str = r#"fn main() {
    let n: IoError = NetError.TimedOut.into();
    let e: IoError = VarError.NotPresent.into();
    let f = IoError.from(VarError.NotUnicode);
    let g = IoError.from(NetError.TimedOut);
    println(f"{n} | {e} | {f} | {g}");
}
"#;

/// `?` on an environment variable and a socket in one function.
const QUESTION: &str = r#"fn g() -> Result[i64, IoError] {
    let h = env.var("KARAC_TEST_B_2026_11_17_7_UNSET")?;
    let _s = TcpStream.connect(h)?;
    Ok(1)
}
fn main() {
    match g() {
        Ok(v) => println(f"ok {v}"),
        Err(e) => println(f"err {e}"),
    }
}
"#;

fn run_both(name: &str, src: &str, want: &str) {
    let dir = std::env::temp_dir().join(format!("karac-ioerr-from-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let mut ran = 0;
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .env_remove("KARAC_TEST_B_2026_11_17_7_UNSET")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
            if common::link_or_skip(Err(stderr)).is_none() {
                continue;
            }
            unreachable!("link_or_skip passes only a successful link");
        }
        assert!(out.status.success(), "{cmd}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{cmd}");
        ran += 1;
    }
    assert!(ran >= 1, "neither backend ran");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn io_error_converts_from_var_error_beside_net_error() {
    run_both(
        "both",
        BOTH,
        "timed out | not found | invalid UTF-8 | timed out\n",
    );
}

#[test]
fn question_mark_on_env_var_and_socket_returns_io_error() {
    run_both("question", QUESTION, "err not found\n");
}
