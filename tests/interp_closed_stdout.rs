//! A program run by the interpreter whose stdout reader goes away stops at
//! once, as `SIGPIPE` would stop it (docs/library/io.md, status 141 in a
//! shell): `karac run --interp prog | head -1` kept running every iteration
//! and exited 0, where the JIT and a built binary stop (B-2026-11-08-28).

#[cfg(unix)]
#[test]
fn the_interpreter_stops_when_its_stdout_reader_closes() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    let path = std::env::temp_dir().join(format!(
        "karac-interp-closed-stdout-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "fn main() {
    let mut i = 0;
    while i < 2000000 {
        println(f\"line {i}\");
        i += 1;
    }
    eprintln(\"finished loop\");
}
",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("karac");
    let mut first = String::new();
    {
        let mut out = BufReader::new(child.stdout.take().unwrap());
        out.read_line(&mut first).unwrap();
        // `out` drops here: the reader is gone.
    }
    let done = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert_eq!(first, "line 0\n");
    assert!(
        done.status.signal() == Some(13) || done.status.code() == Some(141),
        "status {:?}, stderr {stderr}",
        done.status
    );
    assert!(!stderr.contains("finished loop"), "{stderr}");
}
