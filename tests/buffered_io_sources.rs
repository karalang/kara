//! What `BufReader` and `BufWriter` wrap (docs/library/io.md § Buffered
//! I/O; B-2026-11-10-14): a `File`, a child process's pipe or a
//! `TcpStream`. Anything else passed check, then failed in the interpreter
//! ("of a closed file") or was freed as a file by a compiled build (an
//! invalid free under ASan). It is refused at check, E0297, and so, until
//! it is built, is a `TlsStream`.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn check(src: &str) -> (bool, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-buffered-io-sources-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn buffered_wrappers_take_files_and_child_pipes() {
    let (ok, out) = check(
        r#"import std.io.{BufReader, BufWriter};
fn main() {
    let w = BufWriter.new(File.create("b.txt").unwrap());
    drop(w);
    let r = BufReader.with_capacity(File.open("b.txt").unwrap(), 16);
    for line in r.lines() {
        println(line.unwrap_or(String.new()));
    }
    let c = Command.new("cat").stdin(Stdio.piped()).stdout(Stdio.piped()).spawn().unwrap();
    let pin = BufWriter.new(c.stdin().unwrap());
    drop(pin);
    let out = BufReader.new(c.stdout().unwrap());
    drop(out);
}
"#,
    );
    assert!(ok, "{out}");
    assert!(!out.contains("E0297"), "{out}");
}

#[test]
fn buffered_wrappers_take_a_tcp_stream() {
    let (ok, out) = check(
        r#"import std.io.{BufReader, BufWriter};
fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let r = BufReader.new(c.try_clone().unwrap());
    let w = BufWriter.with_capacity(c, 64);
    drop(w);
    drop(r);
}
"#,
    );
    assert!(ok, "{out}");
    assert!(!out.contains("E0297"), "{out}");
}

#[test]
fn a_buffered_reader_over_a_tls_stream_is_refused_until_built() {
    let (ok, out) = check(
        r#"import std.io.{BufReader};
fn wrap(s: own TlsStream) {
    let r = BufReader.new(s);
    drop(r);
}
fn main() {}
"#,
    );
    assert!(!ok, "{out}");
    assert!(out.contains("E0297"), "{out}");
    assert!(out.contains("not a `TlsStream`"), "{out}");
    assert!(out.contains("B-2026-11-10-14"), "{out}");
}

/// Over a connection a buffer's calls have the connection's effects
/// (`sends(Network) receives(Network) blocks`, as `TcpStream.read`), not a
/// file's `reads(FileSystem)`.
#[test]
fn a_socket_buffer_has_the_network_effects() {
    let (ok, out) = check(
        r#"import std.io.{BufReader, BufWriter};
pub fn first_line(conn: own TcpStream) -> String with sends(Network) receives(Network) blocks {
    let r = BufReader.new(conn);
    let mut s = String.new();
    let _ = r.read_line(mut s);
    for line in r.lines() {
        s.push_str(line.unwrap_or(String.new()));
    }
    s
}
pub fn say(conn: own TcpStream) with sends(Network) receives(Network) blocks {
    let mut w = BufWriter.new(conn);
    let _ = w.write_all("hi".bytes());
    let _ = w.flush();
}
fn main() {}
"#,
    );
    assert!(ok, "{out}");
    let (ok, out) = check(
        r#"import std.io.{BufReader};
pub fn first_line(conn: own TcpStream) -> String with reads(FileSystem) blocks {
    let r = BufReader.new(conn);
    let mut s = String.new();
    let _ = r.read_line(mut s);
    s
}
fn main() {}
"#,
    );
    assert!(!ok, "{out}");
    assert!(out.contains("Network"), "{out}");
}

#[test]
fn a_buffered_wrapper_over_a_value_is_refused() {
    let (ok, out) = check(
        r#"import std.io.{BufReader, BufWriter};
fn main() {
    let v: Vec[u8] = Vec.new();
    let r = BufReader.new(v);
    let w = BufWriter.with_capacity("text", 8);
}
"#,
    );
    assert!(!ok, "{out}");
    assert_eq!(out.matches("E0297").count(), 2, "{out}");
    assert!(out.contains("not a `Vec[u8]`"), "{out}");
}

#[test]
fn a_buffered_writer_over_an_output_pipe_is_refused() {
    let (ok, out) = check(
        r#"import std.io.{BufWriter};
fn main() {
    let c = Command.new("true").stdout(Stdio.piped()).spawn().unwrap();
    let w = BufWriter.new(c.stdout().unwrap());
}
"#,
    );
    assert!(!ok, "{out}");
    assert!(out.contains("E0297"), "{out}");
    assert!(out.contains("not a `ChildStdout`"), "{out}");
}
