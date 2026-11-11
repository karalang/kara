//! `read_exact` on the sockets, `File` and a child's pipes, and the
//! library's `From[NetError] for IoError` (docs/library/io.md;
//! B-2026-11-10-44, B-2026-11-11-25), on the MIR interpreter and as a built
//! binary alike.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-read-exact-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stdout_of(out: Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The program's stdout on the interpreter and, when the runtime archive
/// links, as a built binary.
fn interp_and_build(dir: &Path, src: &str) -> (String, Option<String>) {
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let run = |cmd: &mut Command| cmd.current_dir(dir).stdin(Stdio::null()).output().unwrap();
    let interp = stdout_of(
        run(Command::new(karac).args(["__mir-run", file.to_str().unwrap()])),
        "interpreter",
    );
    let exe = dir.join("main");
    let built = Command::new(karac)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_none() {
        return (interp, None);
    }
    let binary = stdout_of(run(&mut Command::new(&exe)), "binary");
    (interp, Some(binary))
}

fn assert_both(name: &str, src: &str, want: &str) {
    let dir = Scratch::new(name);
    let (interp, built) = interp_and_build(dir.path(), src);
    assert_eq!(interp, want);
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A socket's `read_exact` fills its buffer across short reads and ends
/// with `NetError.UnexpectedEof` when the peer closes first; an empty
/// buffer reads nothing.
#[test]
fn tcp_read_exact_fills_across_writes_and_reports_the_early_end() {
    let src = r#"fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let s = l.accept().unwrap();
    s.write_all("hel".bytes()).unwrap();
    s.write_all("lo wor".bytes()).unwrap();
    let mut a = [0u8; 5];
    c.read_exact(mut a).unwrap();
    println(f"{a[0]} {a[4]}");
    s.write_all("ld".bytes()).unwrap();
    s.shutdown_write().unwrap();
    let mut b = [0u8; 8];
    match c.read_exact(mut b) {
        Ok(()) => println("full"),
        Err(e) => println(f"{e}: {b[0]} {b[5]}"),
    }
    let mut empty = [0u8; 0];
    println(f"{c.read_exact(mut empty).is_ok()}");
    match c.read_exact(mut b) {
        Err(NetError.UnexpectedEof) => println("eof"),
        _ => println("other"),
    }
}
"#;
    assert_both(
        "tcp",
        src,
        "104 111\nunexpected end of stream: 32 100\ntrue\neof\n",
    );
}

/// `File.read_exact` and a child's stdout's: `IoError.UnexpectedEof` at an
/// early end.
#[test]
fn file_and_pipe_read_exact_end_with_unexpected_eof() {
    let src = r#"fn main() {
    fs.write("data.bin", "abcdefghij").unwrap();
    let f = File.open("data.bin").unwrap();
    let mut a = [0u8; 4];
    f.read_exact(mut a).unwrap();
    f.read_exact(mut a).unwrap();
    println(f"{a[0]} {a[3]}");
    match f.read_exact(mut a) {
        Ok(()) => println("full"),
        Err(e) => println(f"file: {e}"),
    }
    let mut child = Command.new("printf").arg("abcdef").stdout(Stdio.piped()).spawn().unwrap();
    let out = child.stdout().unwrap();
    out.read_exact(mut a).unwrap();
    println(f"{a[0]} {a[3]}");
    match out.read_exact(mut a) {
        Err(IoError.UnexpectedEof) => println("pipe: eof"),
        _ => println("pipe: other"),
    }
    child.wait().unwrap();
}
"#;
    assert_both(
        "file",
        src,
        "101 104\nfile: unexpected end of file\n97 100\npipe: eof\n",
    );
}

/// `?` converts a socket's `NetError` into a function's `IoError` through
/// the library's `From[NetError] for IoError` (B-2026-11-11-25): same-named
/// variants keep their name, the rest are `Other` of the display.
#[test]
fn question_mark_converts_net_error_into_io_error() {
    let src = r#"fn first_byte(c: ref TcpStream, path: Str) -> Result[i64, IoError] {
    let text = fs.read_to_string(path)?;
    let mut b = [0u8; 1];
    c.read_exact(mut b)?;
    Ok(b[0] as i64 + text.len())
}

fn main() {
    fs.write("n.txt", "ab").unwrap();
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let s = l.accept().unwrap();
    s.write_all("A".bytes()).unwrap();
    println(f"{first_byte(c, "n.txt").unwrap()}");
    s.shutdown_write().unwrap();
    match first_byte(c, "n.txt") {
        Err(IoError.UnexpectedEof) => println("eof"),
        _ => println("other"),
    }
    println(f"{IoError.from(NetError.TimedOut)} / {IoError.from(NetError.Interrupted)}");
    println(f"{IoError.from(NetError.Other(104))}");
}
"#;
    assert_both(
        "question",
        src,
        "67\neof\ntimed out / interrupted\nos error 104\n",
    );
}

const FAKE: &str = r#"import std.io.{BufReader};

struct Fake { n: i64, given: i64 }

impl NetworkProvider for Fake {
    fn connect(mut ref self, addr: Str) -> Result[i64, NetError] {
        self.n = self.n + 1;
        Ok(self.n)
    }

    // Stream 1 hands out "abcdefg" two bytes at a time; stream 2 fails
    // with `UnexpectedEof` at once.
    fn read_stream(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, NetError] {
        if h == 2 {
            return Err(NetError.UnexpectedEof);
        }
        let text = "abcdefg".bytes();
        let mut k = 0;
        while k < 2 and self.given < text.len() and k < buf.len() {
            buf[k] = text[self.given];
            self.given = self.given + 1;
            k = k + 1;
        }
        Ok(k)
    }

    fn write_stream(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, NetError] {
        Ok(buf.len())
    }

    fn close(mut ref self, h: i64) {}
}

fn main() {
    let fake = Fake { n: 0, given: 0 };
    with_provider[Network](fake, || {
        let c = TcpStream.connect("fake:1").unwrap();
        let mut a = [0u8; 5];
        c.read_exact(mut a).unwrap();
        println(f"{a[0]} {a[4]}");
        match c.read_exact(mut a) {
            Err(e) => println(f"{e}"),
            Ok(()) => println("full"),
        }
        let mut r = BufReader.new(TcpStream.connect("fake:2").unwrap());
        let mut line = String.new();
        match r.read_line(mut line) {
            Err(e) => println(f"buffered: {e}"),
            Ok(n) => println(f"read {n}"),
        }
    });
}
"#;

/// Over a `Network` provider's socket `read_exact` calls the fake's
/// `read_stream` until the buffer is full, and a fake's
/// `NetError.UnexpectedEof` reaches a `BufReader` as `IoError.UnexpectedEof`.
#[test]
fn read_exact_over_a_fake_socket() {
    assert_both(
        "fake",
        FAKE,
        "97 101\nunexpected end of stream\nbuffered: unexpected end of file\n",
    );
}
