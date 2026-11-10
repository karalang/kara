//! `BufReader` and `BufWriter` over a socket a `Network` provider opened
//! (docs/library/io.md § Buffered I/O; B-2026-11-10-28): the buffer's reads
//! and writes call the fake's `read_stream` and `write_stream`, its drop
//! calls `close`, and a fake's `NetError` comes back as the matching
//! `IoError`, on the MIR interpreter and as a built binary alike. Before,
//! the interpreter refused at the constructor and the binary exited 101.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-buffered-provider-{name}-{}",
            std::process::id()
        ));
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
    let run = |cmd: &mut Command| cmd.stdin(Stdio::null()).output().unwrap();
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

const SRC: &str = r#"import std.io.{BufReader, BufWriter};

struct Fake { n: i64, pos: i64, half: bool }

impl NetworkProvider for Fake {
    fn connect(mut ref self, addr: Str) -> Result[i64, NetError] {
        self.n = self.n + 1;
        Ok(self.n)
    }

    // Stream 1 hands out its text five bytes at a time; stream 2 a part
    // of a line, then a timeout; stream 3 has no reads.
    fn read_stream(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, NetError] {
        if h == 2 {
            if self.half {
                return Err(NetError.TimedOut);
            }
            self.half = true;
            buf[0] = 104u8;
            buf[1] = 105u8;
            return Ok(2);
        }
        if h == 3 {
            return Err(NetError.Unsupported);
        }
        let text = "one\ntwo\nthree\n".bytes();
        let mut k = 0;
        while k < buf.len() and k < 5 and self.pos < text.len() {
            buf[k] = text[self.pos];
            self.pos = self.pos + 1;
            k = k + 1;
        }
        Ok(k)
    }

    fn write_stream(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, NetError] {
        println(f"write {h} {buf.len()}");
        Ok(buf.len())
    }

    fn close(mut ref self, h: i64) {
        println(f"close {h}");
    }
}

fn main() {
    let mut kept: Vec[TcpStream] = Vec.new();
    with_provider[Network](Fake { n: 0, pos: 0, half: false }, || {
        let r = BufReader.with_capacity(TcpStream.connect("a:1").unwrap(), 4);
        for x in r.lines() {
            match x {
                Ok(t) => println(f"line {t}"),
                Err(e) => println(f"line err {e}"),
            }
        }
        drop(r);
        let mut q = BufReader.new(TcpStream.connect("b:1").unwrap());
        let mut s = String.new();
        match q.read_line(mut s) {
            Ok(n) => println(f"quiet read {n}"),
            Err(e) => println(f"quiet err {e}"),
        }
        drop(q);
        let mut u = BufReader.new(TcpStream.connect("c:1").unwrap());
        let mut s = String.new();
        match u.read_line(mut s) {
            Ok(n) => println(f"other read {n}"),
            Err(e) => println(f"other err {e}"),
        }
        drop(u);
        let mut w = BufWriter.with_capacity(TcpStream.connect("d:1").unwrap(), 8);
        let _ = w.write_all("abc".bytes());
        println("buffered");
        let _ = w.write_all("defg".bytes());
        println(f"flushed {w.flush().is_ok()}");
        let _ = w.write_all("hi".bytes());
        drop(w);
        kept.push(TcpStream.connect("e:1").unwrap());
    });
    let mut late = BufReader.new(kept.pop().unwrap());
    let mut s = String.new();
    match late.read_line(mut s) {
        Ok(n) => println(f"late read {n}"),
        Err(e) => println(f"late err {e}"),
    }
    drop(late);
    println("end");
}
"#;

#[test]
fn buffered_provider_socket_on_both_backends() {
    assert_both(
        "lines",
        SRC,
        "line one\nline two\nline three\nclose 1\nquiet err timed out\nclose 2\n\
         other err unsupported\nclose 3\nbuffered\nwrite 4 7\nflushed true\nwrite 4 2\n\
         close 4\nlate err provider unavailable\nend\n",
    );
}
