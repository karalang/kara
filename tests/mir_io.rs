//! The I/O natives on the MIR backend (redesign M2): `File`, `Clock`,
//! processes, TCP, and the HTTP client and server.
//! The program runs on the MIR interpreter (`karac __mir-run`) and, built
//! ahead of time (`karac build --mir`), as a native binary; both must print
//! the same, byte for byte.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("karac-mir-io-{name}-{}", std::process::id()));
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

/// The program's stdout on the interpreter and, when the runtime archive
/// links, as a binary built with `--mir`. `data.txt` in `dir` is removed
/// before each run, so both start from nothing.
fn interp_and_build(dir: &Path, src: &str) -> (String, Option<String>) {
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let data = dir.join("data.txt");
    let _ = std::fs::remove_file(&data);
    let interp = Command::new(karac)
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        interp.status.success(),
        "interpreter: {}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let interp = String::from_utf8(interp.stdout).unwrap();
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
    let _ = std::fs::remove_file(&data);
    let run = Command::new(&exe).output().unwrap();
    assert!(
        run.status.success(),
        "binary: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    (interp, Some(String::from_utf8(run.stdout).unwrap()))
}

const FILES: &str = r#"
fn show(e: IoError) -> String {
    match e {
        IoError.NotFound => "not found".to_string(),
        IoError.InvalidInput => "invalid input".to_string(),
        IoError.Other(m) => f"other {m}",
        _ => "some other error".to_string(),
    }
}

fn p() -> String { "DIR/data.txt".to_string() }

fn main() {
    match File.create(p()) {
        Ok(f) => {
            let mut data: Vec[u8] = Vec.new();
            data.push(65u8); data.push(66u8); data.push(10u8);
            match f.write(data) { Ok(n) => println(f"wrote {n}"), Err(e) => println(show(e)) }
            match f.sync_all() { Ok(_) => println("synced"), Err(e) => println(show(e)) }
            match f.sync_data() { Ok(_) => println("data synced"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match File.append(p()) {
        Ok(f) => {
            let more = [67u8, 68u8, 10u8];
            match f.write(more[0..3]) { Ok(n) => println(f"appended {n}"), Err(e) => println(show(e)) }
            match f.flush() { Ok(_) => println("flushed"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match FileSystem.read_to_string(p()) {
        Ok(s) => println(f"contents {s.len()} {s.trim()}"),
        Err(e) => println(show(e)),
    }
    match File.open(p()) {
        Ok(g) => {
            match g.seek(SeekFrom.Start, 1i64) { Ok(p) => println(f"pos {p}"), Err(e) => println(show(e)) }
            let mut buf = [0u8, 0u8, 0u8];
            match g.read(mut buf) { Ok(n) => println(f"read {n} {buf[0]} {buf[1]} {buf[2]}"), Err(e) => println(show(e)) }
            match g.seek(SeekFrom.End, -2i64) { Ok(p) => println(f"end {p}"), Err(e) => println(show(e)) }
            match g.seek(SeekFrom.Current, 0i64) { Ok(p) => println(f"cur {p}"), Err(e) => println(show(e)) }
            match g.seek(SeekFrom.Start, -5i64) { Ok(_) => println("unexpected"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match File.open("DIR/missing/none.txt".to_string()) {
        Ok(_) => println("unexpected"),
        Err(e) => println(show(e)),
    }
    let t = Clock.now();
    println(f"clock {t > 1700000000}");
}
"#;

/// Create, write, sync, append, reopen, seek and read a file, and a
/// missing one, on both backends.
#[test]
fn file_and_clock_natives_match_the_interpreter() {
    let dir = Scratch::new("files");
    let src = FILES.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(
        interp,
        "wrote 3\nsynced\ndata synced\nappended 3\nflushed\ncontents 6 AB\nCD\npos 1\n\
         read 3 66 10 67\nend 4\ncur 4\ninvalid input\n\
         not found\nclock true\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

const PROCESSES: &str = r#"
fn show(e: IoError) -> String {
    match e {
        IoError.NotFound => "not found".to_string(),
        IoError.Other(m) => f"other {m}",
        _ => "some other error".to_string(),
    }
}

fn main() {
    let echo = Command.new("sh")
        .arg("-c")
        .arg("printf \"$GREETING\" ; exit 3")
        .env("GREETING", "hi there")
        .stdout(Stdio.piped());
    match echo.spawn() {
        Ok(child) => {
            match child.stdout() {
                Some(out) => {
                    let mut s = String.new();
                    match out.read_to_string(s) {
                        Ok(n) => println(f"read [{s}] {n}"),
                        Err(e) => println(show(e)),
                    }
                }
                None => println("no stdout"),
            }
            match child.stdout() { Some(_) => println("taken twice"), None => println("taken once") }
            match child.wait() {
                Ok(st) => println(f"exit {st.code().unwrap()} {st.success()}"),
                Err(e) => println(show(e)),
            }
            match child.wait() { Ok(st) => println(f"waited twice {st.code().unwrap()}"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    let cat = Command.new("cat").stdin(Stdio.piped()).stdout(Stdio.piped());
    match cat.spawn() {
        Ok(child) => {
            match child.stdin() {
                Some(w) => {
                    match w.write_all("piped through".bytes()) { Ok(_) => println("wrote"), Err(e) => println(show(e)) }
                    match w.close() { Ok(_) => println("closed"), Err(e) => println(show(e)) }
                }
                None => println("no stdin"),
            }
            match child.stdout() {
                Some(out) => {
                    let mut s = String.new();
                    match out.read_to_string(s) {
                        Ok(_) => println(f"cat [{s}]"),
                        Err(e) => println(show(e)),
                    }
                }
                None => println("no stdout"),
            }
            match child.wait() { Ok(st) => println(f"cat exit {st.code().unwrap()}"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    let sleeper = Command.new("sleep").arg("5");
    match sleeper.spawn() {
        Ok(child) => {
            match child.try_wait() {
                Ok(Some(_)) => println("done early"),
                Ok(None) => println("still running"),
                Err(e) => println(show(e)),
            }
            match child.kill() { Ok(_) => println("killed"), Err(e) => println(show(e)) }
            match child.wait() {
                Ok(st) => println(f"killed by {st.signal().unwrap()}, code none {st.code().is_none()}, {st.success()}"),
                Err(e) => println(show(e)),
            }
        }
        Err(e) => println(show(e)),
    }
    match Command.new("/nonexistent/program").spawn() {
        Ok(_) => println("unexpected"),
        Err(e) => println(show(e)),
    }
    match Command.new("sh").arg("-c").arg("printf out; printf err >&2; exit 4").output() {
        Ok(o) => println(f"output [{String.from_utf8(o.stdout).unwrap()}] [{String.from_utf8(o.stderr).unwrap()}] {o.status.code().unwrap()}"),
        Err(e) => println(show(e)),
    }
    let mut a = Command.new("printf").arg("a\nb\nc\n").stdout(Stdio.piped()).spawn().unwrap();
    let b = Command.new("wc").arg("-l")
        .stdin(Stdio.from_stdout(a.stdout().unwrap()))
        .stdout(Stdio.piped())
        .spawn()
        .unwrap();
    let out = b.wait_with_output().unwrap();
    a.wait().unwrap();
    println(f"a | b [{String.from_utf8(out.stdout).unwrap().trim()}]");
}
"#;

/// Spawn with an environment and piped streams, read and write the pipes,
/// wait, poll, kill, spawn a missing program, collect `output`, and pipe one
/// child into another, on both backends.
#[test]
fn process_natives_match_the_interpreter() {
    let dir = Scratch::new("procs");
    let (interp, built) = interp_and_build(dir.path(), PROCESSES);
    assert_eq!(
        interp,
        "read [hi there] 8\ntaken once\nexit 3 false\nwaited twice 3\nwrote\nclosed\n\
         cat [piped through]\ncat exit 0\nstill running\nkilled\n\
         killed by 9, code none true, false\nnot found\noutput [out] [err] 4\na | b [3]\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// `std.process` beyond the basics (B-2026-10-15-6): a file as a stream
/// (`>` and `<`), a pipe given to one child only, waiting twice, a read
/// loop, `read_to_end`, text that is not UTF-8, a dropped or untaken stdin
/// ending its child, `env_remove`, a dropped child and `Stdio`, and a
/// missing working directory.
const PROCESS_SURFACE: &str = r#"
fn show(e: IoError) -> String {
    match e {
        IoError.NotFound => "not found".to_string(),
        IoError.InvalidInput => "invalid input".to_string(),
        IoError.InvalidUtf8 => "invalid utf8".to_string(),
        IoError.Other(m) => f"other {m}",
        _ => "some other error".to_string(),
    }
}

fn main() {
    let _ = FileSystem.remove_file("DIR/out.txt");
    let f = File.create("DIR/out.txt").unwrap();
    let st = Command.new("printf").args(["%s-%s", "x", "y"]).stdout(Stdio.from_file(f)).status().unwrap();
    println(f"redirect {st.success()} {FileSystem.read_to_string("DIR/out.txt").unwrap()}");
    let g = File.open("DIR/out.txt").unwrap();
    let o = Command.new("cat").stdin(Stdio.from_file(g)).output().unwrap();
    println(f"from file [{String.from_utf8(o.stdout).unwrap()}]");
    let mut a = Command.new("printf").arg("abc").stdout(Stdio.piped()).spawn().unwrap();
    let b = Command.new("cat").stdin(Stdio.from_stdout(a.stdout().unwrap())).stdout(Stdio.piped());
    let first = b.output().unwrap();
    println(f"first [{String.from_utf8(first.stdout).unwrap()}]");
    match b.output() { Ok(_) => println("reused"), Err(e) => println(show(e)) }
    let st = a.wait().unwrap();
    let again = a.wait().unwrap();
    println(f"wait twice {st.code().unwrap()} {again.code().unwrap()}");
    match a.try_wait() { Ok(Some(s)) => println(f"try after {s.success()}"), Ok(None) => println("none"), Err(e) => println(show(e)) }
    let c = Command.new("printf").arg("0123456789").stdout(Stdio.piped()).spawn().unwrap();
    let r = c.stdout().unwrap();
    let mut buf: Vec[u8] = vec![0, 0, 0, 0];
    let mut total = 0;
    loop {
        let n = r.read(mut buf).unwrap();
        if n == 0 { break; }
        total = total + n;
    }
    println(f"read loop {total}");
    c.wait().unwrap();
    let d = Command.new("printf").arg("\\377\\376").stdout(Stdio.piped()).stderr(Stdio.piped()).spawn().unwrap();
    let mut s = String.from("keep");
    match d.stdout().unwrap().read_to_string(s) { Ok(_) => println("utf8?"), Err(e) => println(f"{show(e)} [{s}]") }
    let mut bytes: Vec[u8] = Vec.new();
    let n = d.stderr().unwrap().read_to_end(bytes).unwrap();
    println(f"stderr bytes {n} {bytes.len()}");
    d.wait().unwrap();
    let e = Command.new("cat").stdin(Stdio.piped()).stdout(Stdio.piped()).spawn().unwrap();
    {
        let w = e.stdin().unwrap();
        let n = w.write("hello".bytes()).unwrap();
        w.flush().unwrap();
        println(f"wrote {n}");
    }
    let out = e.wait_with_output().unwrap();
    println(f"dropped stdin [{String.from_utf8(out.stdout).unwrap()}] {out.status.success()}");
    let h = Command.new("cat").stdin(Stdio.piped()).stdout(Stdio.null()).spawn().unwrap();
    println(f"wait closes stdin {h.wait().unwrap().success()}");
    let o = Command.new("sh").arg("-c").arg("echo \"[$KEEP][$GONE]\"").env("KEEP", "k").env("GONE", "g").env_remove("GONE").stdin(Stdio.null()).output().unwrap();
    println(f"env {String.from_utf8(o.stdout).unwrap().trim()}");
    let k = Command.new("true").spawn().unwrap();
    println(f"id positive {k.id() > 0}");
    let unused = Stdio.piped();
    match Command.new("true").current_dir("/nonexistent/dir").spawn() { Ok(_) => println("?"), Err(e) => println(show(e)) }
}
"#;

#[test]
fn process_surface_matches_the_interpreter() {
    let dir = Scratch::new("procsurf");
    let src = PROCESS_SURFACE.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(
        interp,
        "redirect true x-y\nfrom file [x-y]\nfirst [abc]\ninvalid input\nwait twice 0 0\n\
         try after true\nread loop 10\ninvalid utf8 [keep]\nstderr bytes 0 0\nwrote 5\n\
         dropped stdin [hello] true\nwait closes stdin true\nenv [k][]\nid positive true\n\
         not found\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A listener and a client on one fixed loopback port, in one thread: the
/// handshake completes into the backlog before `accept`. Covers each
/// `TcpError` cause the runtime reports, `try_clone`, a half-close, and the
/// listener's `Drop` closing its port.
const SOCKETS: &str = r#"
fn show(e: TcpError) -> String {
    match e {
        TcpError.Interrupted => "interrupted".to_string(),
        TcpError.Other(n) => f"other {n}",
        TcpError.AddrInUse => "addr in use".to_string(),
        TcpError.ConnectionRefused => "refused".to_string(),
        TcpError.PermissionDenied => "denied".to_string(),
        TcpError.TimedOut => "timed out".to_string(),
        TcpError.WouldBlock => "would block".to_string(),
        TcpError.InvalidInput => "invalid input".to_string(),
    }
}

fn bytes_of(buf: ref Array[u8, 16], n: i64) -> String {
    let mut s = "".to_string();
    let mut i = 0;
    while i < n {
        s.push(buf[i] as char);
        i = i + 1;
    }
    s
}

fn main() {
    match TcpListener.bind("127.0.0.1:47613".to_string()) {
        Ok(listener) => {
            println("bound");
            match TcpListener.bind("127.0.0.1:47613".to_string()) {
                Ok(_) => println("bound twice"),
                Err(e) => println(show(e)),
            }
            match TcpStream.connect("127.0.0.1:47613".to_string()) {
                Ok(client) => {
                    println("connected");
                    match listener.accept() {
                        Ok(server) => {
                            println("accepted");
                            let msg = "ping".to_string();
                            match client.write_all(msg.bytes()) {
                                Ok(n) => println(f"sent {n}"),
                                Err(e) => println(show(e)),
                            }
                            match client.try_clone() {
                                Ok(other) => {
                                    match other.write("!".to_string().bytes()) {
                                        Ok(n) => println(f"clone sent {n}"),
                                        Err(e) => println(show(e)),
                                    }
                                }
                                Err(e) => println(show(e)),
                            }
                            match client.shutdown_write() {
                                Ok(_) => println("shut"),
                                Err(e) => println(show(e)),
                            }
                            let mut buf: Array[u8, 16] = [0u8; 16];
                            let mut got = "".to_string();
                            let mut more = true;
                            while more {
                                match server.read(mut buf) {
                                    Ok(0) => more = false,
                                    Ok(n) => got.push_str(bytes_of(buf, n)),
                                    Err(e) => {
                                        println(show(e));
                                        more = false;
                                    }
                                }
                            }
                            println(f"got [{got}]");
                        }
                        Err(e) => println(show(e)),
                    }
                }
                Err(e) => println(show(e)),
            }
        }
        Err(e) => println(show(e)),
    }
    match TcpStream.connect("127.0.0.1:1".to_string()) {
        Ok(_) => println("connected to port 1"),
        Err(e) => println(show(e)),
    }
    match TcpStream.connect("not an address".to_string()) {
        Ok(_) => println("connected to nothing"),
        Err(e) => println(show(e)),
    }
    match TcpListener.bind("127.0.0.1:47613".to_string()) {
        Ok(_) => println("bound again after the drop"),
        Err(e) => println(show(e)),
    }
}
"#;

/// TCP on both backends.
#[test]
fn tcp_natives_match_the_interpreter() {
    let dir = Scratch::new("tcp");
    let (interp, built) = interp_and_build(dir.path(), SOCKETS);
    assert_eq!(
        interp,
        "bound\naddr in use\nconnected\naccepted\nsent 4\nclone sent 1\nshut\n\
         got [ping!]\nrefused\ninvalid input\nbound again after the drop\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// `UdpSocket` over loopback (`send_to` / `recv_from`, `connect` / `send` /
/// `recv`, a datagram cut to the buffer, `local_addr`, a read timeout,
/// `try_clone`, a second bind), `TcpListener.local_addr`, a `TcpStream`
/// read timeout, and `TcpError` matching as `NetError` (B-2026-10-15-18).
const UDP: &str = r#"
fn show(e: NetError) -> String {
    match e {
        NetError.Interrupted => "interrupted".to_string(),
        NetError.Other(n) => f"other {n}",
        NetError.AddrInUse => "addr in use".to_string(),
        NetError.ConnectionRefused => "refused".to_string(),
        NetError.PermissionDenied => "denied".to_string(),
        NetError.TimedOut => "timed out".to_string(),
        NetError.WouldBlock => "would block".to_string(),
        NetError.InvalidInput => "invalid input".to_string(),
    }
}

fn text(buf: ref Array[u8, 8], n: i64) -> String {
    let mut s = "".to_string();
    let mut i = 0;
    while i < n {
        s.push(buf[i] as char);
        i = i + 1;
    }
    s
}

fn main() {
    let a = match UdpSocket.bind("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            println(show(e));
            return;
        }
    };
    let b = match UdpSocket.bind("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            println(show(e));
            return;
        }
    };
    let a_at = match a.local_addr() {
        Ok(s) => s,
        Err(e) => show(e),
    };
    let b_at = match b.local_addr() {
        Ok(s) => s,
        Err(e) => show(e),
    };
    println(f"ports differ: {a_at != b_at}");
    match a.send_to("hello world".to_string().bytes(), b_at) {
        Ok(n) => println(f"sent {n}"),
        Err(e) => println(show(e)),
    }
    let mut buf: Array[u8, 8] = [0u8; 8];
    match b.recv_from(mut buf) {
        Ok((n, from)) => println(f"got {n} [{text(buf, n)}] from a: {from == a_at}"),
        Err(e) => println(show(e)),
    }
    match b.connect(a_at) {
        Ok(_) => println("connected"),
        Err(e) => println(show(e)),
    }
    match b.send("pong".to_string().bytes()) {
        Ok(n) => println(f"sent {n}"),
        Err(e) => println(show(e)),
    }
    match a.recv(mut buf) {
        Ok(n) => println(f"got [{text(buf, n)}]"),
        Err(e) => println(show(e)),
    }
    match a.set_read_timeout(Some(0)) {
        Ok(_) => println("zero accepted"),
        Err(e) => println(show(e)),
    }
    match a.set_read_timeout(Some(50)) {
        Ok(_) => println("timeout set"),
        Err(e) => println(show(e)),
    }
    match a.recv(mut buf) {
        Ok(n) => println(f"got {n}"),
        Err(e) => println(show(e)),
    }
    match a.try_clone() {
        Ok(c) => match c.local_addr() {
            Ok(s) => println(f"clone at a: {s == a_at}"),
            Err(e) => println(show(e)),
        },
        Err(e) => println(show(e)),
    }
    match UdpSocket.bind(a_at) {
        Ok(_) => println("bound twice"),
        Err(e) => println(show(e)),
    }
    match UdpSocket.bind("nowhere") {
        Ok(_) => println("bound nowhere"),
        Err(e) => println(show(e)),
    }
    match TcpListener.bind("127.0.0.1:0") {
        Ok(l) => match l.local_addr() {
            Ok(s) => match TcpStream.connect(s) {
                Ok(c) => {
                    match l.accept() {
                        Ok(server) => {
                            match server.set_read_timeout(Some(50)) {
                                Ok(_) => println("tcp timeout set"),
                                Err(e) => println(show(e)),
                            }
                            match server.read(mut buf) {
                                Ok(n) => println(f"tcp got {n}"),
                                Err(e) => println(show(e)),
                            }
                            match server.set_read_timeout(None) {
                                Ok(_) => println("tcp timeout cleared"),
                                Err(e) => println(show(e)),
                            }
                            match c.write_all("x".to_string().bytes()) {
                                Ok(n) => println(f"tcp sent {n}"),
                                Err(e) => println(show(e)),
                            }
                            match server.read(mut buf) {
                                Ok(n) => println(f"tcp got {n}"),
                                Err(e) => println(show(e)),
                            }
                        }
                        Err(e) => println(show(e)),
                    }
                }
                Err(e) => println(show(e)),
            },
            Err(e) => println(show(e)),
        },
        Err(e) => println(show(e)),
    }
    match TcpStream.connect("127.0.0.1:1") {
        Ok(_) => println("connected to 1"),
        Err(TcpError.ConnectionRefused) => println("refused, through the alias"),
        Err(e) => println(show(e)),
    }
}
"#;

#[test]
fn udp_and_socket_options_match_the_interpreter() {
    let dir = Scratch::new("udp");
    let (interp, built) = interp_and_build(dir.path(), UDP);
    // `bind` on port 0 prints nothing; the program asks `local_addr`
    // (B-2026-10-15-69).
    assert_eq!(
        interp,
        "ports differ: true\nsent 11\ngot 8 [hello wo] from a: true\nconnected\n\
         sent 4\ngot [pong]\ninvalid input\ntimeout set\ntimed out\nclone at a: true\n\
         addr in use\ninvalid input\ntcp timeout set\ntimed out\ntcp timeout cleared\n\
         tcp sent 1\ntcp got 1\nrefused, through the alias\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// `Clock.monotonic_ns` and `Clock.wall_ns`, the nanosecond readings
/// `Instant` and `SystemTime` are built on (`clock.` reaches them too).
const CLOCKS: &str = r#"
fn main() {
    let a = Clock.monotonic_ns();
    let mut spin = 0;
    while spin < 100000 {
        spin = spin + 1;
    }
    let b = clock.monotonic_ns();
    let w = Clock.wall_ns();
    let s = clock.now();
    println(f"monotonic: {b >= a}, {a >= 0}");
    println(f"wall after 2026: {w > 1767225600000000000}");
    println(f"wall agrees with now: {w / 1000000000 - s <= 1}");
}
"#;

#[test]
fn nanosecond_clocks_match_the_interpreter() {
    let dir = Scratch::new("clocks");
    let (interp, built) = interp_and_build(dir.path(), CLOCKS);
    assert_eq!(
        interp,
        "monotonic: true, true\nwall after 2026: true\nwall agrees with now: true\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A loopback HTTP/1.1 server for the client test, on its own thread:
/// `GET /hello` is "hi", `GET /empty` empty, any other `GET` a 404, `POST`
/// echoes its body and `PUT` its method, body and `X-Echo` header (as a
/// header too, with a 201). Every response carries `X-Kara: yes`.
fn http_server() -> u16 {
    use std::io::{BufRead, BufReader, Read, Write};
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
                    let mut words = line.split_whitespace();
                    let method = words.next().unwrap_or_default().to_string();
                    let path = words.next().unwrap_or_default().to_string();
                    let (mut len, mut echo) = (0, String::new());
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            match k.trim().to_ascii_lowercase().as_str() {
                                "content-length" => len = v.trim().parse().unwrap_or(0),
                                "x-echo" => echo = v.trim().to_string(),
                                _ => {}
                            }
                        }
                    }
                    let mut body = vec![0; len];
                    if r.read_exact(&mut body).is_err() {
                        return;
                    }
                    let body = String::from_utf8_lossy(&body).into_owned();
                    let (code, text, extra) = match (method.as_str(), path.as_str()) {
                        ("GET", "/hello") => ("200 OK", "hi".to_string(), String::new()),
                        ("GET", "/empty") => ("200 OK", String::new(), String::new()),
                        ("GET", _) => ("404 Not Found", "nope".to_string(), String::new()),
                        ("POST", _) => ("200 OK", body, String::new()),
                        _ => (
                            "201 Created",
                            format!("{method} {body}"),
                            format!("X-Echo: {echo}\r\n"),
                        ),
                    };
                    let reply = format!(
                        "HTTP/1.1 {code}\r\nContent-Length: {}\r\nX-Kara: yes\r\n{extra}\r\n{text}",
                        text.len()
                    );
                    if out.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// `Client.get` / `post`, the `RequestBuilder` chain, a response's status,
/// body, bytes and headers, a non-2xx status, and a refused connection.
const HTTP: &str = r#"
fn main() {
    let c = Client.new();
    let base = "http://127.0.0.1:PORT";
    match c.get(f"{base}/hello") {
        Ok(r) => {
            println(f"get {r.status()} [{r.body()}] {r.bytes().len()} bytes");
            match r.header("X-KARA".to_string()) {
                Some(v) => println(f"header {v}"),
                None => println("no header"),
            }
            match r.header("x-absent".to_string()) {
                Some(v) => println(f"absent {v}"),
                None => println("no absent header"),
            }
            for (k, v) in r.headers() {
                if k.to_lowercase() == "x-kara" {
                    println(f"listed {v}");
                }
            }
        }
        Err(e) => println(f"get failed: {e.message()}"),
    }
    match c.post(f"{base}/echo", "posted body".to_string()) {
        Ok(r) => println(f"post {r.status()} [{r.body()}]"),
        Err(e) => println(f"post failed: {e.message()}"),
    }
    let req = c.request("PUT".to_string(), f"{base}/echo")
        .header("X-Echo".to_string(), "from builder".to_string())
        .body("built body".to_string())
        .timeout(5000);
    match req.send() {
        Ok(r) => {
            println(f"send {r.status()} [{r.body()}]");
            match r.header("x-echo".to_string()) {
                Some(v) => println(f"echoed {v}"),
                None => println("no echo"),
            }
        }
        Err(e) => println(f"send failed: {e.message()}"),
    }
    match c.get(f"{base}/missing") {
        Ok(r) => println(f"missing {r.status()}"),
        Err(e) => println(f"missing: {e.message()}"),
    }
    match c.get(f"{base}/empty") {
        Ok(r) => println(f"empty {r.status()} [{r.body()}]"),
        Err(e) => println(f"empty failed: {e.message()}"),
    }
    match c.get("http://127.0.0.1:1/") {
        Ok(r) => println(f"refused? {r.status()}"),
        Err(e) => println(f"refused: {e.message()}"),
    }
}
"#;

/// The HTTP client on both backends, against a loopback server.
#[test]
fn http_client_natives_match_the_interpreter() {
    let port = http_server();
    let dir = Scratch::new("http");
    let src = HTTP.replace("PORT", &port.to_string());
    // The OS's wording for a refused connection.
    let refused = std::net::TcpStream::connect("127.0.0.1:1")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(
        interp,
        format!(
            "get 200 [hi] 2 bytes\nheader yes\nno absent header\nlisted yes\n\
             post 200 [posted body]\nsend 201 [PUT built body]\nechoed from builder\n\
             missing: http://127.0.0.1:{port}/missing: status code 404\nempty 200 []\n\
             refused: http://127.0.0.1:1/: connect failed: {refused}\n"
        )
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// `Server.serve` with a function handler returning a program's own
/// `Response`, whose `headers` the server sends: the handler echoes what
/// `Request`'s methods read.
const SERVE: &str = r#"
struct Response { status: i64, body: String, headers: Vec[(String, String)] }

fn handle(req: Request) -> Response {
    let mut out = f"{req.method()} {req.path()} [{req.body()}]";
    for (k, v) in req.query() {
        out = out + f" {k}={v}";
    }
    match req.header("x-a".to_string()) {
        Some(v) => { out = out + f" x-a:{v}"; }
        None => { out = out + " no x-a"; }
    }
    let mut hosts = 0;
    for (k, _) in req.headers() {
        if k.to_lowercase() == "host" {
            hosts = hosts + 1;
        }
    }
    out = out + f" hosts={hosts}";
    let mut headers: Vec[(String, String)] = Vec.new();
    headers.push(("X-Trace-Id".to_string(), "abc-123".to_string()));
    headers.push(("Content-Type".to_string(), "text/plain".to_string()));
    let status = if req.path() == "/missing" { 404 } else { 200 };
    Response { status: status, body: out, headers: headers }
}

fn main() {
    match Server.serve("127.0.0.1:0".to_string(), handle) {
        Ok(_) => println("served"),
        Err(e) => println(f"serve: {e.message()}"),
    }
}
"#;

/// `Server.serve` with a closure handler and `std.http`'s `Response`.
const SERVE_CLOSURE: &str = r#"
fn main() {
    let greeting = "hello".to_string();
    let r = Server.serve("127.0.0.1:0".to_string(), |req| {
        Response { status: 200, body: f"{greeting} {req.path()}" }
    });
    match r {
        Ok(_) => println("served"),
        Err(e) => println(f"serve: {e.message()}"),
    }
}
"#;

/// A `Server.serve` that cannot bind.
const SERVE_UNBOUND: &str = r#"
fn handle(req: Request) -> Response {
    Response { status: 200, body: req.path() }
}

fn main() {
    match Server.serve("999.0.0.1:0".to_string(), handle) {
        Ok(_) => println("served"),
        Err(e) => println(f"serve: {e.message()}"),
    }
}
"#;

/// Starts a server program, sends it `requests(port)` once it prints its
/// `BOUND_PORT` on stderr (B-2026-10-15-69), and stops it: the transcript
/// of what came back.
fn exchange(mut cmd: Command, requests: fn(u16) -> String) -> String {
    use std::io::{BufRead, BufReader, Read};
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the server");
    let mut err = BufReader::new(child.stderr.take().expect("stderr"));
    let mut line = String::new();
    let _ = err.read_line(&mut line);
    let Some(port) = line.trim().strip_prefix("BOUND_PORT=") else {
        let _ = child.kill();
        let mut rest = String::new();
        let _ = err.read_to_string(&mut rest);
        panic!("the server printed {line:?}, not its port: {rest}");
    };
    let transcript = requests(port.parse().expect("a port"));
    let _ = child.kill();
    let _ = child.wait();
    transcript
}

/// Sends `raw` on `s` and appends the response to `t`: its head and body
/// (by `content-length`), and `closed` when the server then closes.
fn round_trip(s: &mut std::net::TcpStream, raw: &str, closes: bool, t: &mut String) {
    use std::io::{BufRead, BufReader, Read, Write};
    s.write_all(raw.as_bytes()).expect("send");
    let mut r = BufReader::new(s.try_clone().expect("clone"));
    let mut len = 0;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).expect("a response line");
        let h = h.trim_end().to_string();
        t.push_str(&h);
        t.push('\n');
        if h.is_empty() {
            break;
        }
        if let Some(n) = h.strip_prefix("content-length: ") {
            len = n.parse().expect("a length");
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).expect("the body");
    t.push_str(&String::from_utf8_lossy(&body));
    t.push('\n');
    if closes {
        let mut rest = Vec::new();
        let _ = r.read_to_end(&mut rest);
        t.push_str(&format!("closed after {} more bytes\n", rest.len()));
    }
}

fn connect(port: u16) -> std::net::TcpStream {
    let s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .expect("timeout");
    s
}

/// Three requests on one kept-alive connection (a query and a header, a
/// sized body, a chunked body that closes it), then an HTTP/1.0 request.
fn serve_requests(port: u16) -> String {
    let mut t = String::new();
    let mut s = connect(port);
    round_trip(
        &mut s,
        "GET /a/b?x=1+2&y=%41&flag&&z= HTTP/1.1\r\nHost: h\r\nX-A: hi\r\n\r\n",
        false,
        &mut t,
    );
    round_trip(
        &mut s,
        "POST /post HTTP/1.1\r\nHost: h\r\nContent-Length: 7\r\n\r\npayload",
        false,
        &mut t,
    );
    round_trip(
        &mut s,
        "PUT /c HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
         3\r\nabc\r\n3;x=y\r\ndef\r\n0\r\n\r\n",
        true,
        &mut t,
    );
    round_trip(
        &mut connect(port),
        "GET /missing HTTP/1.0\r\n\r\n",
        true,
        &mut t,
    );
    t
}

fn closure_requests(port: u16) -> String {
    let mut t = String::new();
    round_trip(
        &mut connect(port),
        "GET /p HTTP/1.1\r\nConnection: close\r\n\r\n",
        true,
        &mut t,
    );
    t
}

/// `Server.serve` and `Request` on both backends: the same requests get
/// the same bytes back.
#[test]
fn http_server_natives_match_the_interpreter() {
    let dir = Scratch::new("serve");
    let karac = env!("CARGO_BIN_EXE_karac");
    let (unbound, built) = interp_and_build(dir.path(), SERVE_UNBOUND);
    assert_eq!(
        unbound,
        "serve: 999.0.0.1:0: bind failed: invalid socket address syntax\n"
    );
    if let Some(built) = built {
        assert_eq!(built, unbound);
    }
    let reply = |status: &str, body: &str, close: bool| {
        format!(
            "HTTP/1.1 {status}\nX-Trace-Id: abc-123\nContent-Type: text/plain\n\
             content-length: {}\n{}\n{body}\n{}",
            body.len(),
            if close { "connection: close\n" } else { "" },
            if close {
                "closed after 0 more bytes\n"
            } else {
                ""
            },
        )
    };
    let want = [
        reply(
            "200 OK",
            "GET /a/b [] x=1 2 y=A flag= z= x-a:hi hosts=1",
            false,
        ),
        reply("200 OK", "POST /post [payload] no x-a hosts=1", false),
        reply("200 OK", "PUT /c [abcdef] no x-a hosts=1", true),
        reply("404 Not Found", "GET /missing [] no x-a hosts=0", true),
    ]
    .concat();
    let closure_want = "HTTP/1.1 200 OK\ncontent-type: application/json\ncontent-length: 8\n\
                        connection: close\n\nhello /p\nclosed after 0 more bytes\n";
    for (name, src, requests, want) in [
        (
            "serve",
            SERVE,
            serve_requests as fn(u16) -> String,
            want.as_str(),
        ),
        ("closure", SERVE_CLOSURE, closure_requests, closure_want),
    ] {
        let file = dir.path().join(format!("{name}.kara"));
        std::fs::write(&file, src).unwrap();
        let mut interp = Command::new(karac);
        interp.args(["__mir-run", file.to_str().unwrap()]);
        assert_eq!(
            exchange(interp, requests),
            want,
            "{name} on the interpreter"
        );
        let exe = dir.path().join(name);
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
        if linked.is_some() {
            assert_eq!(exchange(Command::new(&exe), requests), want, "{name} built");
        }
    }
}

/// `Server.serve_static` answers every request with a 200 and its body,
/// and `Request.headers()` names each header in lower case, as legacy's
/// server did (a `header` lookup ignores case either way). Found by the
/// corpus driver (ADR-4 plan step 4).
const SERVE_STATIC: &str = r#"
fn main() {
    match Server.serve_static("127.0.0.1:0", "{}") {
        Ok(_) => println("served"),
        Err(e) => println(f"serve: {e.message()}"),
    }
}
"#;

const HEADER_NAMES: &str = r#"
fn handle(req: Request) -> Response {
    let mut s = "".to_string();
    for (k, v) in req.headers() {
        s = s + k + "=" + v + ";";
    }
    match req.header("X-TEST-ECHO") {
        Some(v) => { s = s + " found " + v; }
        None => { s = s + " none"; }
    }
    Response { status: 200, body: s }
}

fn main() {
    let _ = Server.serve("127.0.0.1:0", handle);
}
"#;

#[test]
fn serve_static_and_lowercased_header_names_match_on_both_backends() {
    let dir = Scratch::new("serve_static");
    let karac = env!("CARGO_BIN_EXE_karac");
    let one = |port: u16| {
        let mut t = String::new();
        let mut s = connect(port);
        round_trip(
            &mut s,
            "GET /x HTTP/1.1\r\nHost: h\r\nX-Test-Echo: hi\r\nConnection: close\r\n\r\n",
            true,
            &mut t,
        );
        t
    };
    for (name, src, want) in [
        (
            "static",
            SERVE_STATIC,
            "HTTP/1.1 200 OK\ncontent-type: application/json\ncontent-length: 2\n\
             connection: close\n\n{}\nclosed after 0 more bytes\n",
        ),
        (
            "names",
            HEADER_NAMES,
            "HTTP/1.1 200 OK\ncontent-type: application/json\ncontent-length: 48\n\
             connection: close\n\nhost=h;x-test-echo=hi;connection=close; found hi\n\
             closed after 0 more bytes\n",
        ),
    ] {
        let file = dir.path().join(format!("{name}.kara"));
        std::fs::write(&file, src).unwrap();
        let mut interp = Command::new(karac);
        interp.args(["__mir-run", file.to_str().unwrap()]);
        assert_eq!(exchange(interp, one), want, "{name} on the interpreter");
        let exe = dir.path().join(name);
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
        if linked.is_some() {
            assert_eq!(exchange(Command::new(&exe), one), want, "{name} built");
        }
    }
}

/// B-2026-10-14-44 — io.md's spellings: paths and addresses are `Str`
/// (a `String` still passes), and `File.read` / `write` count in `i64`.
/// `karac check` accepts the program and both MIR backends run it.
const STR_PATHS: &str = r#"fn main() {
    let p: Str = "DIR/data.txt";
    match fs.write(p, "hi\n") { Ok(_) => {}, Err(_) => println("w") }
    match File.create(p) {
        Ok(f) => {
            let n: i64 = f.write("abc".bytes()).unwrap_or(0);
            println(f"wrote {n}");
            let _ = f.flush();
        }
        Err(_) => println("create"),
    }
    match File.append(p) {
        Ok(f) => { let _ = f.write("de".bytes()); let _ = f.flush(); }
        Err(_) => println("append"),
    }
    match File.open(p) {
        Ok(f) => {
            let mut buf: Vec[u8] = vec![0; 16];
            let n: i64 = f.read(mut buf).unwrap_or(-1);
            println(f"read {n}");
        }
        Err(_) => println("open"),
    }
    let owned = p.to_string();
    println(fs.read_to_string(owned).unwrap_or(String.new()));
    println(f"{fs.read_lines(p).unwrap_or(Vec.new()).len()} {owned}");
    let a: Str = "127.0.0.1:1";
    match TcpStream.connect(a) { Ok(_) => println("conn"), Err(_) => println("refused") }
}
"#;

#[test]
fn file_and_socket_functions_take_str() {
    let dir = Scratch::new("str-paths");
    let src = STR_PATHS.replace("DIR", &dir.path().to_string_lossy());
    let file = dir.path().join("check.kara");
    std::fs::write(&file, &src).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "check: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let (interp, built) = interp_and_build(dir.path(), &src);
    let path = dir.path().join("data.txt");
    assert_eq!(
        interp,
        format!("wrote 3\nread 5\nabcde\n1 {}\nrefused\n", path.display())
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// B-2026-10-14-45 — `BufReader` / `BufWriter` (docs/library/io.md §
/// Buffered I/O): `read_line` appends and counts in `i64`, `for line in
/// r.lines()` drops the `\n` / `\r\n` and stops at the end (or a `break`),
/// a bad line is `Err(InvalidUtf8)` and the next one still reads, `?` on
/// the `File` inside the constructor, a small capacity, `read` / `consume`
/// / `read_to_string`, and a `BufWriter` that flushes when it drops.
const BUFFERED: &str = r#"fn count(path: Str) -> Result[i64, IoError] {
    let mut n = 0;
    for line in BufReader.new(File.open(path)?).lines() {
        let l = line?;
        if l == "stop" { break; }
        n += l.len() as i64;
    }
    Ok(n)
}
fn main() {
    let t = "DIR/t.txt";
    match fs.write(t, "a\nb\r\nc") { Ok(_) => {}, Err(_) => println("w") }
    match File.open(t) {
        Ok(f) => {
            let r = BufReader.with_capacity(f, 2);
            let mut s = String.new();
            let n: i64 = r.read_line(mut s).unwrap_or(-1);
            println(f"{n} [{s.trim()}]");
            for line in r.lines() {
                match line { Ok(l) => println(f"<{l}>"), Err(_) => println("e") }
            }
        }
        Err(_) => println("open"),
    }
    let u = "DIR/u.txt";
    match File.create(u) {
        Ok(f) => {
            let w = BufWriter.with_capacity(f, 4);
            match w.write_all("hi\n".bytes()) { Ok(_) => {}, Err(_) => println("e") }
            let n: i64 = w.write("there\n".bytes()).unwrap_or(-1);
            println(f"wrote {n}");
            match w.flush() { Ok(_) => {}, Err(_) => println("e") }
            let _ = w.write_all("tail\n".bytes());
        }
        Err(_) => println("create"),
    }
    print(fs.read_to_string(u).unwrap_or(String.new()));
    match File.open(u) {
        Ok(f) => {
            let r = BufReader.new(f);
            let mut buf: Vec[u8] = vec![0; 3];
            let n = r.read(mut buf).unwrap_or(-1);
            println(f"read {n} {buf[0]}");
            r.consume(1);
            let mut rest = String.new();
            let k = r.read_to_string(mut rest).unwrap_or(-1);
            print(f"{k} [{rest}]\n");
        }
        Err(_) => println("open2"),
    }
    let v = "DIR/v.txt";
    match fs.write(v, "ab\ncde\nstop\nzzzz\n") { Ok(_) => {}, Err(_) => println("w") }
    println(f"{count(v).unwrap_or(-1)}");
    match count("DIR/missing.txt") {
        Ok(_) => println("?"),
        Err(IoError.NotFound) => println("notfound"),
        Err(_) => println("other"),
    }
    let b = "DIR/bad.txt";
    match File.create(b) {
        Ok(f) => {
            let w = BufWriter.new(f);
            let _ = w.write_all(vec![104u8, 255u8, 10u8, 105u8].as_slice());
        }
        Err(_) => println("c"),
    }
    match File.open(b) {
        Ok(f) => {
            for line in BufReader.new(f).lines() {
                match line {
                    Ok(l) => println(f"ok {l}"),
                    Err(IoError.InvalidUtf8) => println("utf8"),
                    Err(_) => println("err"),
                }
            }
        }
        Err(_) => println("o"),
    }
}
"#;

#[test]
fn buffered_readers_and_writers_run() {
    let dir = Scratch::new("buffered");
    let src = BUFFERED.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(
        interp,
        "2 [a]\n<b>\n<c>\nwrote 6\nhi\nthere\ntail\nread 3 104\n10 [here\ntail\n]\n5\nnotfound\nutf8\nok i\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// The file system beyond read and write (docs/library/io.md § Files,
/// B-2026-10-14-54): `exists`, `metadata`, a sorted `read_dir`,
/// `create_dir` / `create_dir_all`, `remove_file` / `remove_dir` /
/// `remove_dir_all`, `rename` and `copy`, their `IoError`s, and a
/// `Result[(), IoError]` passed to a function that names `()`.
const FS: &str = r#"
fn show(r: Result[(), IoError]) -> String {
    match r { Ok(_) => "ok".to_string(), Err(e) => match e { IoError.NotFound => "notfound".to_string(), IoError.AlreadyExists => "exists".to_string(), _ => "other".to_string() } }
}
fn main() {
    println(f"{fs.exists("DIR/work")}");
    println(show(fs.create_dir("DIR/work")));
    println(show(fs.create_dir("DIR/work")));
    println(show(fs.create_dir_all("DIR/work/a/b")));
    fs.write("DIR/work/b.txt", "hello").unwrap();
    fs.write("DIR/work/a.txt", "hi").unwrap();
    match fs.read_dir("DIR/work") { Ok(ns) => println(ns.join(",")), Err(_) => println("err") }
    match fs.metadata("DIR/work/b.txt") {
        Ok(m) => println(f"{m.len} {m.is_file} {m.is_dir} {m.modified_ms > 0}"),
        Err(_) => println("err"),
    }
    match fs.metadata("DIR/work/a") { Ok(m) => println(f"{m.is_file} {m.is_dir}"), Err(_) => println("err") }
    match fs.copy("DIR/work/b.txt", "DIR/work/c.txt") { Ok(n) => println(f"copied {n}"), Err(_) => println("err") }
    println(show(fs.rename("DIR/work/c.txt", "DIR/work/d.txt")));
    println(f"{fs.exists("DIR/work/c.txt")} {fs.exists("DIR/work/d.txt")}");
    println(fs.read_to_string("DIR/work/d.txt").unwrap());
    println(show(fs.remove_file("DIR/work/d.txt")));
    println(show(fs.remove_file("DIR/work/d.txt")));
    println(show(fs.remove_dir("DIR/work/a/b")));
    println(show(fs.remove_dir("DIR/work")));
    println(show(fs.remove_dir_all("DIR/work")));
    println(f"{fs.exists("DIR/work")}");
    match fs.metadata("nope") { Ok(_) => println("?"), Err(IoError.NotFound) => println("notfound"), Err(_) => println("other") }
}
"#;

#[test]
fn file_system_natives_match_the_interpreter() {
    let dir = Scratch::new("fs");
    let src = FS.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(
        interp,
        "false\nok\nexists\nok\na,a.txt,b.txt\n5 true false true\nfalse true\ncopied 5\nok\n\
         false true\nhello\nok\nnotfound\nok\nother\nok\nfalse\nnotfound\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// Links, modes and times (docs/library/io.md § Files, B-2026-10-15-59):
/// `symlink` and `read_link`, `symlink_metadata` against `metadata` on a
/// link, a dangling link and a file, `Metadata.mode` after
/// `set_permissions` (and its `InvalidInput` range), and `set_modified` on
/// a file and a directory, read back through `modified_ms`.
const FS_LINKS: &str = r#"
fn show(r: Result[(), IoError]) -> String {
    match r { Ok(_) => "ok".to_string(), Err(e) => match e { IoError.NotFound => "notfound".to_string(), IoError.AlreadyExists => "exists".to_string(), IoError.InvalidInput => "invalid".to_string(), _ => "other".to_string() } }
}
fn kind(m: Metadata) -> String {
    f"{m.len} file={m.is_file} dir={m.is_dir} link={m.is_symlink}"
}
fn main() {
    fs.create_dir("DIR/t").unwrap();
    fs.write("DIR/t/a.txt", "hello").unwrap();
    println(show(fs.symlink("a.txt", "DIR/t/l")));
    println(show(fs.symlink("a.txt", "DIR/t/l")));
    println(fs.read_link("DIR/t/l").unwrap());
    match fs.read_link("DIR/t/a.txt") { Ok(_) => println("?"), Err(IoError.InvalidInput) => println("not a link"), Err(_) => println("other") }
    println(kind(fs.metadata("DIR/t/l").unwrap()));
    match fs.symlink_metadata("DIR/t/l") { Ok(m) => println(f"file={m.is_file} dir={m.is_dir} link={m.is_symlink}"), Err(_) => println("err") }
    println(kind(fs.symlink_metadata("DIR/t/a.txt").unwrap()));
    println(show(fs.symlink("missing", "DIR/t/d")));
    println(f"{fs.exists("DIR/t/d")}");
    match fs.metadata("DIR/t/d") { Ok(_) => println("?"), Err(IoError.NotFound) => println("dangling"), Err(_) => println("other") }
    match fs.symlink_metadata("DIR/t/d") { Ok(m) => println(f"link={m.is_symlink}"), Err(_) => println("err") }
    println(fs.read_link("DIR/t/d").unwrap());
    println(show(fs.set_permissions("DIR/t/a.txt", 0o755)));
    println(f"{fs.metadata("DIR/t/a.txt").unwrap().mode == 0o755}");
    println(show(fs.set_permissions("DIR/t/a.txt", 0o640)));
    let m = fs.metadata("DIR/t/a.txt").unwrap().mode;
    println(f"{m == 0o640} {m}");
    println(show(fs.set_permissions("DIR/t/a.txt", -1)));
    println(show(fs.set_permissions("DIR/t/a.txt", 0o10000)));
    println(show(fs.set_permissions("DIR/t/nope", 0o644)));
    println(show(fs.set_modified("DIR/t/a.txt", 1759311000123)));
    println(fs.metadata("DIR/t/a.txt").unwrap().modified_ms);
    println(show(fs.set_modified("DIR/t/a.txt", -5000)));
    println(fs.metadata("DIR/t/a.txt").unwrap().modified_ms);
    println(show(fs.set_modified("DIR/t", 1000)));
    println(fs.metadata("DIR/t").unwrap().modified_ms);
    println(show(fs.set_modified("DIR/t/nope", 0)));
    println(show(fs.set_permissions("DIR/t", 0o700)));
    println(fs.metadata("DIR/t").unwrap().mode);
    println(show(fs.remove_file("DIR/t/l")));
    println(f"{fs.exists("DIR/t/l")} {fs.exists("DIR/t/a.txt")}");
    println(show(fs.remove_dir_all("DIR/t")));
}
"#;

#[cfg(unix)]
#[test]
fn file_system_links_modes_and_times_match_the_interpreter() {
    let dir = Scratch::new("fslinks");
    let src = FS_LINKS.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(
        interp,
        "ok\nexists\na.txt\nnot a link\n5 file=true dir=false link=false\n\
         file=false dir=false link=true\n5 file=true dir=false link=false\nok\nfalse\n\
         dangling\nlink=true\nmissing\nok\ntrue\nok\ntrue 416\ninvalid\ninvalid\n\
         notfound\nok\n1759311000123\nok\n-5000\nok\n1000\nnotfound\nok\n448\nok\n\
         false true\nok\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// `BufReader.fill_buf` (docs/library/io.md § Buffered I/O,
/// B-2026-10-14-72): a view of the reader's buffer, read in a scope that
/// ends before `consume`, then the rest, then an empty view at the end.
const FILL_BUF: &str = r#"
fn main() {
    fs.write("DIR/fb.txt", "HELLOWORLD").unwrap();
    let mut r = BufReader.with_capacity(File.open("DIR/fb.txt").unwrap(), 4);
    let n = match r.fill_buf() {
        Ok(b) => {
            println(f"{b.len()} {b[0]} {b[3]}");
            b.len() - 1
        }
        Err(_) => 0,
    };
    r.consume(n);
    match r.fill_buf() { Ok(b) => println(f"{b.len()} {b[0]}"), Err(_) => println("e") }
    r.consume(100);
    let mut s = String.new();
    let _ = r.read_to_string(mut s);
    println(s);
    match r.fill_buf() { Ok(b) => println(f"end {b.len()}"), Err(_) => println("e") }
}
"#;

#[test]
fn fill_buf_views_the_readers_buffer_on_both_backends() {
    let dir = Scratch::new("fillbuf");
    let src = FILL_BUF.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(interp, "4 72 76\n1 76\nOWORLD\nend 0\n");
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// The view `fill_buf` returns borrows the reader, so `consume` while it is
/// alive is a borrow error (B-2026-10-14-72).
#[test]
fn consume_while_a_fill_buf_view_lives_is_a_borrow_error() {
    let dir = Scratch::new("fillbuf_borrow");
    let file = dir.path().join("main.kara");
    let src = r#"
fn main() {
    let mut r = BufReader.new(File.open("x.txt").unwrap());
    let b = r.fill_buf().unwrap();
    r.consume(1);
    println(f"{b.len()}");
}
"#;
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "accepted: {err}");
    assert!(
        err.contains("while it is borrowed mutably") && err.contains("E0516"),
        "{err}"
    );
}

/// docs/library/concurrency.md's channel and `TaskGroup` surface
/// (B-2026-10-15-9, B-2026-10-15-11): `channel(cap)`, `send` returning
/// `Result[(), SendError[T]]`, `recv` returning `Option[T]` (`None` once
/// every sender is gone and the channel is drained), `Receiver.into_iter`,
/// `TaskGroup.new(limit: n)` and a `TaskHandle` returned from a function.
const CHANNELS: &str = r#"
fn start(group: ref TaskGroup, n: i64) -> TaskHandle[i64] {
    group.spawn(|| n * 2)
}

fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(2);
    {
        let t = tx;
        match t.send(1) {
            Ok(()) => println("sent 1"),
            Err(_) => println("err"),
        }
        t.send(2).unwrap();
        match t.try_send(3) {
            Err(SendError.Full(v)) => println(f"full {v}"),
            _ => println("?"),
        }
        match rx.recv() {
            Some(v) => println(f"got {v}"),
            None => println("none"),
        }
        println(rx.recv().unwrap());
    }
    match rx.recv() {
        Some(v) => println(f"got {v}"),
        None => println("closed"),
    }
    let (t2, r2) = channel(4);
    {
        let t = t2;
        t.send("a").unwrap();
        t.send("b").unwrap();
    }
    for s in r2.into_iter() {
        println(s);
    }
    let (t3, r3): (Sender[String], Receiver[String]) = channel(1);
    {
        let r = r3;
    }
    match t3.send("x") {
        Err(SendError.Closed(v)) => println(f"closed {v}"),
        _ => println("?"),
    }
    let g = TaskGroup.new(limit: 2);
    let h = start(g, 21);
    println(h.join());
}
"#;

#[test]
fn channel_and_task_group_natives_match_the_interpreter() {
    let dir = Scratch::new("channels");
    let (interp, built) = interp_and_build(dir.path(), CHANNELS);
    assert_eq!(
        interp,
        "sent 1\nfull 3\ngot 1\n2\nclosed\na\nb\nclosed x\n42\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// `File.options()` and its builder, `set_len`, `fs.read_bytes` /
/// `write_bytes` and the `InvalidInput` refusals (docs/library/io.md § Files,
/// B-2026-10-15-28): a page rewritten in place, a file cut short, a lock
/// file made with `create_new`, and the combinations that open nothing.
const OPEN_OPTIONS: &str = r#"
fn show(e: IoError) -> String {
    match e {
        IoError.NotFound => "not found",
        IoError.AlreadyExists => "already exists",
        IoError.InvalidInput => "invalid input",
        IoError.Unsupported => "unsupported",
        IoError.Other(m) => f"other {m}",
        _ => "else",
    }
}
fn main() {
    let p = "DIR/db.bin".to_string();
    let _ = fs.remove_file("DIR/lock");
    let data: Vec[u8] = vec![1, 2, 3, 4, 5, 6, 7, 8];
    match fs.write_bytes(p.clone(), data.as_slice()) { Ok(_) => println("wrote 8"), Err(e) => println(show(e)) }
    match fs.read_bytes(p.clone()) { Ok(b) => println(f"read {b.len()} {b[0]} {b[7]}"), Err(e) => println(show(e)) }
    match File.options().read(true).write(true).open(p.clone()) {
        Ok(f) => {
            let _ = f.seek(SeekFrom.Start, 2);
            let patch: Vec[u8] = vec![30, 40];
            let _ = f.write(patch.as_slice());
            match f.set_len(6) { Ok(_) => println("cut to 6"), Err(e) => println(show(e)) }
            match f.set_len(-1) { Ok(_) => println("negative ok?"), Err(e) => println(show(e)) }
            match f.seek(SeekFrom.Start, -1) { Ok(_) => println("seek ok?"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match fs.read_bytes(p.clone()) { Ok(b) => println(f"now {b.len()} {b[1]} {b[2]} {b[3]} {b[5]}"), Err(e) => println(show(e)) }
    match File.options().read(true).write(true).open("DIR/nope.bin") { Ok(_) => println("opened?"), Err(e) => println(show(e)) }
    match File.options().write(true).create_new(true).open(p.clone()) { Ok(_) => println("created?"), Err(e) => println(show(e)) }
    match File.options().write(true).create_new(true).open("DIR/lock") { Ok(_) => println("lock made"), Err(e) => println(show(e)) }
    match File.options().open(p.clone()) { Ok(_) => println("opened?"), Err(e) => println(show(e)) }
    match File.options().read(true).truncate(true).open(p.clone()) { Ok(_) => println("opened?"), Err(e) => println(show(e)) }
    match File.options().write(true).create_new(true).truncate(true).open(p.clone()) { Ok(_) => println("opened?"), Err(e) => println(show(e)) }
    match File.options().read(true).append(true).open(p.clone()) {
        Ok(f) => {
            let tail: Vec[u8] = vec![9];
            let _ = f.write(tail.as_slice());
            let mut buf: Array[u8, 16] = [0; 16];
            let _ = f.seek(SeekFrom.Start, 0);
            match f.read(mut buf) { Ok(n) => println(f"appended, {n} bytes, last {buf[n - 1]}"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match File.options().write(true).create(true).open(p.clone()) { Ok(_) => println("kept"), Err(e) => println(show(e)) }
    match fs.metadata(p.clone()) { Ok(m) => println(f"len {m.len}"), Err(e) => println(show(e)) }
    match fs.read_bytes("DIR/missing") { Ok(_) => println("read?"), Err(e) => println(show(e)) }
}
"#;

#[test]
fn open_options_set_len_and_bytes_match_the_interpreter() {
    let dir = Scratch::new("options");
    let src = OPEN_OPTIONS.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(
        interp,
        "wrote 8\nread 8 1 8\ncut to 6\ninvalid input\ninvalid input\nnow 6 2 30 40 6\n\
         not found\nalready exists\nlock made\ninvalid input\ninvalid input\ninvalid input\n\
         appended, 7 bytes, last 9\nkept\nlen 7\nnot found\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A user type with a library type's name is the user's: nested in an
/// aggregate it drops through its own fields, not the library's handle
/// (B-2026-10-15-48, and Thread A's `Pool` report).
#[test]
fn user_types_named_like_library_handles_drop_as_their_own() {
    let dir = Scratch::new("shadow");
    let src = r#"
struct Loud { id: i64 }
impl Drop for Loud {
    fn drop(mut ref self) { println(f"drop {self.id}"); }
}
struct Pool { n: Vec[i64], l: Loud }
struct File { name: String, l: Loud }
struct W { p: Pool, f: File }
struct Mutex { required: bool, l: Loud }
struct Atomic { l: Loud }
struct OnceLock { l: Loud }
struct Arena { l: Loud }
fn main() {
    let m = Mutex { required: true, l: Loud { id: 4 } };
    println(f"{m.required}");
    let a = Atomic { l: Loud { id: 5 } };
    let o = OnceLock { l: Loud { id: 6 } };
    let r = Arena { l: Loud { id: 7 } };
    let w = W { p: Pool { n: vec![1, 2], l: Loud { id: 1 } }, f: File { name: "x".to_string(), l: Loud { id: 2 } } };
    println(f"{w.p.n.len()} {w.f.name}");
    let v = vec![Pool { n: vec![3], l: Loud { id: 3 } }];
    println(f"{v[0].n[0]}");
}
"#;
    let (interp, built) = interp_and_build(dir.path(), src);
    // Each `Loud` drops once, through its user type's own fields.
    let mut drops: Vec<&str> = interp.lines().filter(|l| l.starts_with("drop")).collect();
    drops.sort();
    assert_eq!(
        drops,
        ["drop 1", "drop 2", "drop 3", "drop 4", "drop 5", "drop 6", "drop 7"],
        "{interp}"
    );
    assert!(interp.starts_with("true\n2 x\n3\n"), "{interp}");
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// B-2026-10-15-44 — a `BufReader` over a child's stdout or stderr and a
/// `BufWriter` over its stdin (docs/library/io.md § Buffered I/O), on both
/// backends: `lines`, `read_line` and `read_to_string` through a small
/// buffer, and a writer whose drop closes the child's stdin.
const PIPE_BUFFERS: &str = r#"
fn feed(child: ref Child) {
    let w = BufWriter.new(child.stdin().unwrap());
    let _ = w.write_all("zeta\nalpha\n".bytes());
    let _ = w.write_all("mid\n".bytes());
}

fn main() {
    let c = Command.new("sh").arg("-c").arg("printf 'a\nbb\n'; printf 'e1\ne2\n' >&2")
        .stdout(Stdio.piped()).stderr(Stdio.piped()).spawn().unwrap();
    for line in BufReader.new(c.stdout().unwrap()).lines() {
        println(f"out [{line.unwrap()}]");
    }
    for line in BufReader.new(c.stderr().unwrap()).lines() {
        println(f"err [{line.unwrap()}]");
    }
    println(f"sh {c.wait().unwrap().success()}");
    let sort = Command.new("sort").stdin(Stdio.piped()).stdout(Stdio.piped()).spawn().unwrap();
    feed(sort);
    let r = BufReader.with_capacity(sort.stdout().unwrap(), 4);
    let mut first = String.new();
    let n = r.read_line(mut first).unwrap();
    print(f"first {n} {first}");
    let mut rest = String.new();
    let _ = r.read_to_string(mut rest);
    print(rest);
    println(f"sort {sort.wait().unwrap().success()}");
}
"#;

#[test]
fn buffered_io_over_a_childs_pipes() {
    let dir = Scratch::new("pipe-buffers");
    let (interp, built) = interp_and_build(dir.path(), PIPE_BUFFERS);
    assert_eq!(
        interp,
        "out [a]\nout [bb]\nerr [e1]\nerr [e2]\nsh true\nfirst 6 alpha\nmid\nzeta\nsort true\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// The default providers of the program-rooted resources (core-semantics
/// §14.7, B-2026-10-20-11) ask the operating system, as the ambient natives
/// do, and a test double states only the methods it uses.
const DEFAULT_PROVIDERS: &str = r#"
struct FakeEnv {}
impl EnvProvider for FakeEnv {
    fn var(self, name: Str) -> Result[String, VarError] { Ok("fake") }
}
fn main() {
    let e = OsEnv {};
    match e.var("KARA_DEFINITELY_UNSET_XYZ") {
        Ok(s) => println(s),
        Err(_) => println("unset"),
    }
    match e.var("PATH") {
        Ok(s) => println(s.len() > 0),
        Err(_) => println("no path"),
    }
    println(e.args().len());
    let c = OsClock {};
    println(c.now() > 0);
    println(c.wall_ns() > 0);
    println(c.monotonic_ns() >= 0);
    let r = OsRandom {};
    println(r.next_u64() != r.next_u64());
    let f = FakeEnv {};
    match f.var("X") {
        Ok(s) => println(s),
        Err(_) => println("err"),
    }
}
"#;

/// A method the double leaves out panics, naming the resource and the method.
const UNIMPLEMENTED_METHOD: &str = r#"
struct FakeClock {}
impl ClockProvider for FakeClock {
    fn now(self) -> i64 { 42 }
}
fn main() {
    let c = FakeClock {};
    println(c.now());
    println(c.wall_ns());
}
"#;

#[test]
fn default_providers_and_panicking_trait_defaults_match_on_both_backends() {
    let dir = Scratch::new("providers");
    let (interp, built) = interp_and_build(dir.path(), DEFAULT_PROVIDERS);
    assert_eq!(interp, "unset\ntrue\n1\ntrue\ntrue\ntrue\ntrue\nfake\n");
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
    let file = dir.path().join("panics.kara");
    std::fs::write(&file, UNIMPLEMENTED_METHOD).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let want = "Clock.wall_ns called on a provider that does not implement it";
    for mode in ["__mir-run", "__mir-llvm-run"] {
        let out = Command::new(karac)
            .args([mode, file.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{mode} should panic");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n", "{mode}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(want), "{mode}: {err}");
    }
}

/// An HTTPS origin on loopback with a fresh self-signed certificate for
/// `localhost`, answering each request with `200` and `body` until the test
/// ends. Returns its port and the certificate's PEM, for
/// `KARAC_HTTP_EXTRA_CA`.
fn https_origin(body: &'static str) -> (u16, String) {
    use std::io::{Read, Write};
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let pem = cert.cert.pem();
    let der = cert.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = std::sync::Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der], key)
            .unwrap(),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(tcp) = tcp else { return };
            let conn = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let (mut got, mut chunk) = (Vec::new(), [0u8; 1024]);
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                match tls.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&chunk[..n]),
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nx-origin: tls\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tls.write_all(reply.as_bytes());
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
    });
    (port, pem)
}

const HTTPS_GET: &str = r#"
fn main() {
    let c = Client.new();
    match c.get("URL") {
        Ok(r) => {
            println(r.status());
            println(r.body());
            match r.header("X-Origin") {
                Some(v) => println(v),
                None => println("no header"),
            }
        }
        Err(e) => println(f"error: {e.message()}"),
    }
}
"#;

/// The HTTP client speaks HTTPS on both MIR backends, trusting the bundled
/// roots plus `KARAC_HTTP_EXTRA_CA` (docs/library/io.md § HTTP client).
#[test]
fn https_client_matches_on_both_backends() {
    let dir = Scratch::new("https");
    let (port, pem) = https_origin("over tls");
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, pem).unwrap();
    let file = dir.path().join("main.kara");
    let src = HTTPS_GET.replace("URL", &format!("https://localhost:{port}/x"));
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let run = |cmd: &mut Command| {
        let out = cmd
            .env("KARAC_HTTP_EXTRA_CA", &ca)
            .env("NO_PROXY", "localhost,127.0.0.1")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let want = "200\nover tls\ntls\n";
    assert_eq!(
        run(Command::new(karac).args(["__mir-run", file.to_str().unwrap()])),
        want
    );
    let exe = dir.path().join("main");
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
    if linked.is_some() {
        assert_eq!(run(&mut Command::new(&exe)), want);
    }
}
