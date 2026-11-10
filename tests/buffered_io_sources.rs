//! What `BufReader` and `BufWriter` wrap (docs/library/io.md § Buffered
//! I/O; B-2026-11-10-14, B-2026-11-10-28): a `File`, a child process's pipe,
//! a `TcpStream` or a `TlsStream`. Anything else passed check, then failed in
//! the interpreter ("of a closed file") or was freed as a file by a compiled
//! build (an invalid free under ASan). It is refused at check, E0297, and so
//! is a generic wrapper's instance over it, at the call that instantiates it.

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
fn buffered_wrappers_take_a_tls_stream() {
    let (ok, out) = check(
        r#"import std.io.{BufReader, BufWriter};
pub fn wrap(s: own TlsStream, t: own TlsStream) with sends(Network) receives(Network) blocks {
    let mut r = BufReader.new(s);
    let mut line = String.new();
    let _ = r.read_line(mut line);
    let mut w = BufWriter.with_capacity(t, 64);
    let _ = w.write_all(line.bytes());
    let _ = w.flush();
}
fn main() {}
"#,
    );
    assert!(ok, "{out}");
    assert!(!out.contains("E0297"), "{out}");
}

/// A generic wrapper is judged at each instance: the body passes check for
/// its type parameter, and the call that instantiates it with a value that
/// is not a source is refused there (B-2026-11-10-28).
#[test]
fn a_generic_buffered_wrapper_is_judged_at_its_call() {
    let (ok, out) = check(
        r#"import std.io.{BufReader};
fn wrap[R](r: own R) -> BufReader[R] {
    BufReader.new(r)
}
fn outer[S](s: own S) -> BufReader[S] {
    wrap(s)
}
fn main() {
    let f = wrap(File.open("x.txt").unwrap());
    drop(f);
    let v: Vec[u8] = Vec.new();
    let r = outer(v);
    drop(r);
}
"#,
    );
    assert!(!ok, "{out}");
    assert_eq!(out.matches("\"severity\":\"error\"").count(), 1, "{out}");
    assert!(out.contains("\"E0297\""), "{out}");
    assert!(out.contains("not a `Vec[u8]`"), "{out}");
    assert!(out.contains("\"line\":12"), "{out}");
    assert!(out.contains("instantiates"), "{out}");
}

/// Over a connection a buffer's calls have the connection's effects
/// (`sends(Network) receives(Network) blocks`, as `TcpStream.read`), not a
/// file's `reads(FileSystem)`.
#[test]
fn a_socket_buffer_has_the_network_effects() {
    let (ok, out) = check(
        r#"import std.io.{BufReader, BufWriter};
pub fn first_line(conn: own TcpStream) -> String with sends(Network) receives(Network) blocks {
    let mut r = BufReader.new(conn);
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
    let mut r = BufReader.new(conn);
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
