//! `BufReader` and `BufWriter` over a `TcpStream` (docs/library/io.md §
//! Buffered I/O; B-2026-11-10-14): a read waits inside its task as the
//! connection's own read does, a timeout and a reset come back as
//! `IoError`s, and dropping the buffer closes the connection. The same
//! output on the interpreter and a compiled build, at one and four threads.

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn run(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "karac-buffered-sockets-i-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(feature = "llvm")]
fn compiled(src: &str, threads: &str) -> Option<String> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!(
        "karac-buffered-sockets-b-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| {
        Command::new(&exe)
            .current_dir(&dir)
            .env("KARAC_THREADS", threads)
            .output()
            .expect("run")
    });
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    assert!(out.status.success(), "{:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Two clients, each in a task, talk to a server whose connection tasks
/// read lines through a `BufReader` and answer through a `BufWriter`: every
/// read waits inside its task, so neither client holds up the other.
const ECHO: &str = r#"import std.io.{BufReader, BufWriter};
import std.sync.{TaskGroup};

fn serve(conn: own TcpStream) {
    let mut w = BufWriter.new(conn.try_clone().unwrap());
    let r = BufReader.new(conn);
    for line in r.lines() {
        match line {
            Ok(l) => {
                let _ = w.write_all(f"{l.to_uppercase()}\n".bytes());
                let _ = w.flush();
            }
            Err(e) => {
                println(f"server err {e}");
                break;
            }
        }
    }
}

fn client(addr: Str, words: own Vec[Str]) -> String {
    let c = TcpStream.connect(addr).unwrap();
    let mut r = BufReader.new(c.try_clone().unwrap());
    let mut out = String.new();
    for w in words.into_iter() {
        let _ = c.write_all(f"{w}\n".bytes());
        let mut line = String.new();
        let _ = r.read_line(mut line);
        out.push_str(line.trim_end());
        out.push_str(" ");
    }
    let _ = c.shutdown_write();
    let mut rest = String.new();
    let _ = r.read_to_string(mut rest);
    out.push_str(f"[{rest}]");
    out
}

fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let group = TaskGroup.new();
    let server = group.spawn(|| {
        let tasks = TaskGroup.new();
        for _ in 0..2 {
            let conn = l.accept().unwrap();
            tasks.spawn(|| serve(conn));
        }
    });
    let a = group.spawn(|| client(addr, ["one", "two", "three"].to_vec()));
    let b = group.spawn(|| client(addr, ["four", "five"].to_vec()));
    let mut both = [a.join(), b.join()].to_vec();
    both.sort();
    for x in both.into_iter() {
        println(x);
    }
    let _ = server.join();
}
"#;

const ECHO_OUT: &str = "FOUR FIVE []\nONE TWO THREE []\n";

/// A buffered read reports the socket's read timeout as an `IoError` and
/// reads on once the bytes come, on the main task and inside a spawned one.
/// `TimedOut` prints the same on every platform.
const TIMEOUT: &str = r#"import std.io.{BufReader};
import std.sync.{TaskGroup};

fn probe(tag: Str, l: TcpListener) {
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let _ = c.set_read_timeout(Some(200));
    let s = l.accept().unwrap();
    let mut r = BufReader.new(c);
    let mut line = String.new();
    match r.read_line(mut line) {
        Ok(n) => println(f"{tag} first ok {n}"),
        Err(e) => println(f"{tag} first err {e}"),
    }
    let _ = s.write_all("late\n".bytes());
    match r.read_line(mut line) {
        Ok(n) => println(f"{tag} then ok {n} {line.trim_end()}"),
        Err(e) => println(f"{tag} then err {e}"),
    }
}

fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    probe("main", l);
    let l2 = TcpListener.bind("127.0.0.1:0").unwrap();
    let g = TaskGroup.new();
    let h = g.spawn(|| probe("task", l2));
    let _ = h.join();
}
"#;

const TIMEOUT_OUT: &str = "main first err timed out\nmain then ok 5 late\n\
task first err timed out\ntask then ok 5 late\n";

/// A buffered read reports the socket's read timeout and its reset as
/// `IoError`s, on the main task and inside a spawned one. Unix only: Windows
/// words the reset its own way and drops the unread `partial` with it.
#[cfg(unix)]
const EDGES: &str = r#"import std.io.{BufReader};
import std.sync.{TaskGroup};

fn probe(tag: Str, l: TcpListener) {
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let _ = c.set_read_timeout(Some(200));
    let _ = c.write_all("unread\n".bytes());
    let s = l.accept().unwrap();
    let mut r = BufReader.new(c);
    let mut line = String.new();
    match r.read_line(mut line) {
        Ok(n) => println(f"{tag} first ok {n}"),
        Err(e) => println(f"{tag} first err {e}"),
    }
    let _ = s.write_all("partial\n".bytes());
    drop(s);
    for x in r.lines() {
        match x {
            Ok(t) => println(f"{tag} line {t}"),
            Err(e) => {
                println(f"{tag} line err {e}");
                break;
            }
        }
    }
}

fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    probe("main", l);
    let l2 = TcpListener.bind("127.0.0.1:0").unwrap();
    let g = TaskGroup.new();
    let h = g.spawn(|| probe("task", l2));
    let _ = h.join();
}
"#;

/// The reset's errno is the platform's (104 on Linux, 54 on macOS), so it
/// is left out of the comparison.
#[cfg(unix)]
const EDGES_OUT: &str = "main first err timed out\nmain line partial\n\
main line err Connection reset by peer\ntask first err timed out\n\
task line partial\ntask line err Connection reset by peer\n";

/// `out` without its ` (os error N)` suffixes.
#[cfg(unix)]
fn without_errno(out: &str) -> String {
    let mut s = out.to_string();
    while let Some(at) = s.find(" (os error ") {
        let end = s[at..].find(')').map_or(s.len(), |e| at + e + 1);
        s.replace_range(at..end, "");
    }
    s
}

/// `fill_buf` over a connection views the reader's buffer, waiting inside
/// its task for the first bytes; `consume` steps through them. A `fill_buf`
/// is not a constructor (op 12 sat inside the constructors' op range: the
/// compiled build freed the reader as a file).
const FILL: &str = r#"import std.io.{BufReader};
import std.sync.{TaskGroup};

fn drain(s: own TcpStream) -> String {
    let mut r = BufReader.with_capacity(s, 4);
    let mut seen = String.new();
    loop {
        let n = match r.fill_buf() {
            Ok(b) => {
                if b.len() == 0 {
                    break;
                }
                seen.push_str(f"{b.len()}:{b[0]} ");
                b.len()
            }
            Err(e) => {
                seen.push_str(f"err {e}");
                break;
            }
        };
        r.consume(n);
    }
    seen
}

fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let s = l.accept().unwrap();
    let g = TaskGroup.new();
    let h = g.spawn(|| drain(s));
    let _ = c.write_all("HELLOWORLD".bytes());
    let _ = c.shutdown_write();
    println(h.join());
}
"#;

const FILL_OUT: &str = "4:72 4:79 2:76 \n";

#[test]
fn interp_buffered_socket_fill_buf() {
    assert_eq!(run(FILL), FILL_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_buffered_socket_fill_buf() {
    for threads in ["1", "4"] {
        if let Some(out) = compiled(FILL, threads) {
            assert_eq!(out, FILL_OUT, "KARAC_THREADS={threads}");
        }
    }
}

#[test]
fn interp_buffered_socket_echo() {
    assert_eq!(run(ECHO), ECHO_OUT);
}

#[test]
fn interp_buffered_socket_read_timeout() {
    assert_eq!(run(TIMEOUT), TIMEOUT_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_buffered_socket_read_timeout() {
    for threads in ["1", "4"] {
        if let Some(out) = compiled(TIMEOUT, threads) {
            assert_eq!(out, TIMEOUT_OUT, "KARAC_THREADS={threads}");
        }
    }
}

#[cfg(unix)]
#[test]
fn interp_buffered_socket_timeout_and_reset() {
    assert_eq!(without_errno(&run(EDGES)), EDGES_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_buffered_socket_echo() {
    for threads in ["1", "4"] {
        if let Some(out) = compiled(ECHO, threads) {
            assert_eq!(out, ECHO_OUT, "KARAC_THREADS={threads}");
        }
    }
}

#[cfg(all(unix, feature = "llvm"))]
#[test]
fn compiled_buffered_socket_timeout_and_reset() {
    for threads in ["1", "4"] {
        if let Some(out) = compiled(EDGES, threads) {
            assert_eq!(without_errno(&out), EDGES_OUT, "KARAC_THREADS={threads}");
        }
    }
}
