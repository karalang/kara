//! B-2026-11-25-35: a `pub fn` over a file or a connection declares the
//! resource rooted at the parameter (`writes(f)`, `receives(c)`), which the
//! front-end checker counts as the class it charges the body to; and a
//! socket read is a receive only, a write a send only (io.md § Effects).

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, src: &str, args: &[&str]) -> (Output, String) {
    let dir = std::env::temp_dir().join(format!("karac-fsparams-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

#[test]
fn a_file_parameter_declares_writes_f() {
    let src = "import std.io.{File};

pub fn put(f: ref File) -> bool with writes(f) blocks { f.write(\"z\".bytes()).is_ok() }
fn main() { println(1); }
";
    let (out, text) = karac("put", src, &["check", "m.kara"]);
    assert!(out.status.success(), "{text}");
}

#[test]
fn a_file_parameter_write_declaration_does_not_cover_a_read() {
    let src = "pub fn get(f: ref File) -> bool with writes(f) blocks { let mut b = [0u8; 1]; f.read(mut b).is_ok() }
fn main() { println(1); }
";
    let (out, text) = karac("get", src, &["check", "m.kara"]);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("performs reads(FileSystem) but does not declare it"),
        "{text}"
    );
}

#[test]
fn a_connection_parameter_declares_receives_and_sends() {
    let src = "import std.net.{TcpStream};

pub fn rd(c: ref TcpStream) -> bool with receives(c) blocks suspends { let mut b = [0u8; 1]; c.read(mut b).is_ok() }
pub fn wr(c: ref TcpStream) -> bool with sends(c) blocks suspends { c.write(\"x\".bytes()).is_ok() }
fn main() { println(1); }
";
    let (out, text) = karac("conn", src, &["check", "m.kara"]);
    assert!(out.status.success(), "{text}");
}

#[test]
fn a_socket_read_is_not_a_send() {
    let src = "fn rd(c: ref TcpStream) -> bool { let mut b = [0u8; 1]; c.read(mut b).is_ok() }
fn main() { println(1); }
";
    let (out, text) = karac("rd", src, &["explain", "m.kara.rd"]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("  receives(Network)\n"), "{text}");
    assert!(!text.contains("sends(Network)"), "{text}");
}

#[test]
fn query_effects_shows_the_parameter_a_clause_named() {
    let src =
        "pub fn put(f: ref File) -> bool with writes(f) blocks { f.write(\"z\".bytes()).is_ok() }
pub fn snd(tx: ref Sender[i64]) with sends(tx) blocks { let _ = tx.send(1); }
fn main() { println(1); }
";
    let (out, text) = karac("query-put", src, &["query", "effects", "m.kara.put"]);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("\"declared_effects\":[{\"verb\":\"writes\",\"resource\":\"FileSystem\",\"roots\":[\"f\"]}"),
        "{text}"
    );
    assert!(
        text.contains("{\"verb\":\"writes\",\"resource\":\"FileSystem\",\"roots\":[\"f\"]}"),
        "{text}"
    );
    let (out, text) = karac("query-snd", src, &["query", "effects", "m.kara.snd"]);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("\"declared_effects\":[{\"verb\":\"sends\",\"resource\":\"Channel\",\"roots\":[\"tx\"]}"),
        "{text}"
    );
    let (out, text) = karac("query-all", src, &["query", "effects", "m.kara"]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("\"roots\":[\"tx\"]"), "{text}");
    assert!(text.contains("\"roots\":[\"f\"]"), "{text}");
}
