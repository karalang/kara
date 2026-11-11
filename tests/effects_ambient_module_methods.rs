//! A call on an ambient module (`fs.write(...)`, `stdin.read_line()`) has
//! that module's effects only, in a program that also uses sockets
//! (B-2026-11-11-85). The name-keyed fallbacks matched `write` and
//! `read_line` to `TcpStream.write` and `BufReader[TcpStream].read_line`, so
//! a function that wrote a file was refused for undeclared network effects.

use std::path::PathBuf;
use std::process::{Command, Output};

fn check(tag: &str, src: &str) -> Output {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-ambient-methods-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// The socket half every program below shares: a write on a `TcpStream` and
/// a `read_line` through a `BufReader` over one.
const SOCKETS: &str = r#"
fn talk() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let s = l.accept().unwrap();
    let _ = c.write_all("x\n".bytes());
    let mut r = BufReader.new(s);
    let mut t = String.new();
    let _ = r.read_line(mut t);
}
"#;

#[test]
fn ambient_fs_and_stdin_calls_keep_their_own_effects_beside_sockets() {
    let src = format!(
        r#"fn save() with writes(FileSystem) {{
    fs.write("x.txt", "hi").unwrap();
}}

fn ask() -> String with reads(Stdin) writes(Stdin) blocks {{
    match stdin.read_line() {{
        Ok(l) => l,
        Err(_) => String.new(),
    }}
}}
{SOCKETS}
fn main() {{
    save();
    let _ = ask();
    talk();
}}
"#
    );
    let out = check("accepted", &src);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check must accept: {err}");
}

#[test]
fn ambient_fs_call_is_still_charged_its_own_effect_beside_sockets() {
    let src = format!(
        r#"fn save() with reads(Env) {{
    fs.write("x.txt", "hi").unwrap();
}}
{SOCKETS}
fn main() {{
    save();
    talk();
}}
"#
    );
    let out = check("refused", &src);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(
        err.contains("performs writes(FileSystem) (via call to 'FileSystem.write'"),
        "{err}"
    );
    assert!(!err.contains("Network"), "{err}");
}
