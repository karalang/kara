//! The standard streams and the io.md conformance fixes on the MIR backend
//! (ADR-3 phase 2, B-2026-10-21-8 to -15): `stdin.lines()`, text that is
//! not UTF-8, `stdout.flush()` and the order of a program's output against
//! a child's, `IoError.from`, `env.var` of a `Str` (and its
//! `NotUnicode`, B-2026-10-21-20), `File.append` of a
//! `String` path, and the HTTP client's five redirects. Each program runs on
//! the MIR interpreter (`karac __mir-run`) and as a binary built with
//! `karac build --mir`, with stdout on a pipe; the expected output is
//! docs/library/io.md's, and both backends must print it.
#![cfg(feature = "llvm")]

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-mir-std-streams-{name}-{}",
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

/// Runs `cmd` with `input` on its stdin; its stdout, after asserting it
/// succeeded.
fn run_with(mut cmd: Command, input: &[u8], what: &str) -> String {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("KARA_STD_STREAMS_SET", "hello")
        .env(
            "KARA_STD_STREAMS_BAD",
            <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"\xff\xfe"),
        )
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// The program's stdout on the interpreter and, when the runtime archive
/// links, as a binary built with `--mir`.
fn interp_and_build(dir: &Path, src: &str, input: &[u8]) -> (String, Option<String>) {
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let mut interp = Command::new(karac);
    interp.args(["__mir-run", file.to_str().unwrap()]);
    let interp = run_with(interp, input, "interpreter");
    let exe = dir.join("main");
    let built = Command::new(karac)
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
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
    let run = run_with(Command::new(&exe), input, "binary");
    (interp, Some(run))
}

fn check(name: &str, src: &str, input: &[u8], want: &str) {
    let dir = Scratch::new(name);
    let src = src.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src, input);
    assert_eq!(interp, want, "interpreter");
    if let Some(built) = built {
        assert_eq!(built, want, "binary");
    }
}

/// `read_line` keeps its `\n`; `stdin.lines()` then yields the rest of the
/// lines without `\n` / `\r\n`, a last line with no newline included.
const LINES: &str = r#"
fn main() {
    match stdin.read_line() {
        Ok(l) => println(f"first [{l.trim()}] {l.len()}"),
        Err(_) => println("first failed"),
    }
    let mut n = 0;
    for line in stdin.lines() {
        match line {
            Ok(l) => println(f"<{l}>"),
            Err(_) => println("err"),
        }
        n += 1;
    }
    println(f"{n} lines");
    for line in stdin.lines() {
        println(f"after the end {line.unwrap()}");
    }
    println("done");
}
"#;

#[test]
fn stdin_lines_iterate_on_both_backends() {
    check(
        "lines",
        LINES,
        b"head\na\r\nbb\n\nccc",
        "first [head] 5\n<a>\n<bb>\n<>\n<ccc>\n4 lines\ndone\n",
    );
}

/// Bytes that are not UTF-8 are an `InvalidUtf8`, for `read_line`,
/// `read_to_string` and a line of `stdin.lines()`.
const BAD_UTF8: &str = r#"
fn show(r: Result[String, IoError]) -> String {
    match r {
        Ok(l) => f"ok {l.len()}",
        Err(IoError.InvalidUtf8) => "invalid utf8".to_string(),
        Err(_) => "other".to_string(),
    }
}
fn main() {
    println(show(stdin.read_line()));
    for line in stdin.lines() {
        println(show(line));
    }
    println(show(stdin.read_to_string()));
}
"#;

#[test]
fn text_that_is_not_utf8_is_invalid_utf8() {
    check(
        "utf8",
        BAD_UTF8,
        b"h\xffi\nok\nx\xfey\n",
        "invalid utf8\nok 2\ninvalid utf8\nok 0\n",
    );
}

/// `stdout.flush()` pushes out a partial line, and what a program printed
/// comes out before a child it then starts writes to the same stdout.
const ORDER: &str = r#"
fn main() {
    print("x");
    stdout.flush();
    let _ = Command.new("echo").arg("y").status();
    println("z");
    print("partial ");
    let _ = Command.new("echo").arg("then child").status();
    println("parent first");
    let _ = Command.new("echo").arg("child second").status();
    println("parent third");
    print("A");
    stdout.flush();
    eprintln("to stderr");
    println("C");
}
"#;

#[test]
fn stdout_flush_orders_output_against_a_child() {
    check(
        "order",
        ORDER,
        b"",
        "xy\nz\npartial then child\nparent first\nchild second\nparent third\nAC\n",
    );
}

/// `IoError.from` of a `VarError`, `env.var` given a `Str`, and
/// `File.append` given a `String` path twice (it takes a `Str`, so the
/// path stays the caller's).
const CONVERSIONS: &str = r#"
fn show(e: IoError) -> String {
    match e {
        IoError.NotFound => "NotFound".to_string(),
        IoError.InvalidUtf8 => "InvalidUtf8".to_string(),
        _ => "other".to_string(),
    }
}
fn present(name: Str) -> String {
    match env.var(name) {
        Ok(v) => v,
        Err(VarError.NotPresent) => "unset".to_string(),
        Err(VarError.NotUnicode) => "not unicode".to_string(),
    }
}
fn main() {
    println(show(IoError.from(VarError.NotPresent)));
    println(show(IoError.from(VarError.NotUnicode)));
    println(present("KARA_STD_STREAMS_SET"));
    println(present("KARA_STD_STREAMS_UNSET"));
    println(present("KARA_STD_STREAMS_BAD"));
    let p: String = "DIR/appended.txt".to_string();
    let _ = fs.remove_file(p);
    match File.append(p) {
        Ok(f) => {
            let mut f = f;
            let _ = f.write("ab".bytes());
        }
        Err(e) => println(show(e)),
    }
    match File.append(p) {
        Ok(f) => {
            let mut f = f;
            let _ = f.write("cd".bytes());
        }
        Err(e) => println(show(e)),
    }
    match fs.read_to_string(p) {
        Ok(s) => println(f"{s} {p.len() > 0}"),
        Err(e) => println(show(e)),
    }
}
"#;

#[test]
fn conversions_and_str_parameters_match_io_md() {
    check(
        "conversions",
        CONVERSIONS,
        b"",
        "NotFound\nInvalidUtf8\nhello\nunset\nnot unicode\nabcd true\n",
    );
}

/// A loopback server whose `/redir/k` answers 302 to `/redir/k-1`, and
/// `/redir/0` to `/hello`, which answers `hi`.
fn redirect_server() -> u16 {
    use std::io::{BufRead, BufReader};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut out = conn.try_clone().expect("clone");
                let mut r = BufReader::new(conn);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let path = line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_string();
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                            break;
                        }
                    }
                    let reply = match path.strip_prefix("/redir/").and_then(|k| k.parse::<u32>().ok()) {
                        Some(0) => "HTTP/1.1 302 Found\r\nLocation: /hello\r\nContent-Length: 0\r\n\r\n".to_string(),
                        Some(k) => format!(
                            "HTTP/1.1 302 Found\r\nLocation: /redir/{}\r\nContent-Length: 0\r\n\r\n",
                            k - 1
                        ),
                        None => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_string(),
                    };
                    if out.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// io.md: "The client follows up to five redirects". The message of the
/// failure is not specified, so only its presence is printed.
const REDIRECTS: &str = r#"
fn main() {
    let c = Client.new();
    let mut k = 0;
    while k < 6 {
        match c.get(f"http://127.0.0.1:PORT/redir/{k}") {
            Ok(r) => println(f"{k + 1} redirects: {r.status()} [{r.body()}]"),
            Err(_) => println(f"{k + 1} redirects: err"),
        }
        k += 1;
    }
}
"#;

#[test]
fn the_http_client_follows_five_redirects() {
    let port = redirect_server();
    check(
        "redirects",
        &REDIRECTS.replace("PORT", &port.to_string()),
        b"",
        "1 redirects: 200 [hi]\n2 redirects: 200 [hi]\n3 redirects: 200 [hi]\n\
         4 redirects: 200 [hi]\n5 redirects: 200 [hi]\n6 redirects: err\n",
    );
}
