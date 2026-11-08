//! `IoError` and `VarError` display as the phrases io.md § Errors names, and
//! `Debug` shows the variant. `main`'s `Err` exit writes the phrase too:
//! the library's `impl Display for IoError` re-states the baked one, and was
//! lowered under a `#1` name the entry display did not look up, so the exit
//! printed `Error: NotFound` (B-2026-11-08-30).

fn run(name: &str, src: &str) -> std::process::Output {
    let path = std::env::temp_dir().join(format!(
        "karac-io-error-display-{name}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", path.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

#[test]
fn main_s_io_error_exit_prints_the_phrase() {
    let out = run(
        "main",
        "fn main() -> Result[(), IoError] {
    let s = fs.read_to_string(\"/nonexistent/kara-io-error-exit\")?;
    println(s);
    Ok(())
}
",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("Error: not found\n"), "{stderr}");
}

#[test]
fn main_s_var_error_exit_prints_the_phrase_and_debug_the_variant() {
    let out = run(
        "var",
        "fn main() -> Result[(), VarError] {
    dbg(IoError.NotFound);
    dbg(VarError.NotUnicode);
    Err(VarError.NotPresent)
}
",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("IoError.NotFound = NotFound\n"), "{stderr}");
    assert!(
        stderr.contains("VarError.NotUnicode = NotUnicode\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Error: environment variable not found\n"),
        "{stderr}"
    );
}
