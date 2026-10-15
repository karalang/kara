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
         read 3 66 10 67\nend 4\ncur 4\nother invalid seek to a negative position\n\
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
    let echo = Command.new("sh".to_string())
        .arg("-c".to_string())
        .arg("printf \"$GREETING\" ; exit 3".to_string())
        .env("GREETING".to_string(), "hi there".to_string())
        .stdout(Stdio.Piped);
    match echo.spawn() {
        Ok(child) => {
            match child.stdout() {
                Some(out) => match out.read_to_string() {
                    Ok(s) => println(f"read [{s}]"),
                    Err(e) => println(show(e)),
                },
                None => println("no stdout"),
            }
            match child.stdout() { Some(_) => println("taken twice"), None => println("taken once") }
            match child.wait() {
                Ok(st) => println(f"exit {st.code} {st.success}"),
                Err(e) => println(show(e)),
            }
            match child.wait() { Ok(_) => println("waited twice"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    let cat = Command.new("cat".to_string()).stdin(Stdio.Piped).stdout(Stdio.Piped);
    match cat.spawn() {
        Ok(child) => {
            match child.stdin() {
                Some(w) => {
                    match w.write("piped through".to_string()) { Ok(_) => println("wrote"), Err(e) => println(show(e)) }
                    match w.close() { Ok(_) => println("closed"), Err(e) => println(show(e)) }
                }
                None => println("no stdin"),
            }
            match child.stdout() {
                Some(out) => match out.read_to_string() {
                    Ok(s) => println(f"cat [{s}]"),
                    Err(e) => println(show(e)),
                },
                None => println("no stdout"),
            }
            match child.wait() { Ok(st) => println(f"cat exit {st.code}"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    let sleeper = Command.new("sleep".to_string()).arg("5".to_string());
    match sleeper.spawn() {
        Ok(child) => {
            match child.try_wait() {
                Ok(Some(_)) => println("done early"),
                Ok(None) => println("still running"),
                Err(e) => println(show(e)),
            }
            match child.kill() { Ok(_) => println("killed"), Err(e) => println(show(e)) }
            match child.wait() { Ok(st) => println(f"killed exit {st.code} {st.success}"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match Command.new("/nonexistent/program".to_string()).spawn() {
        Ok(_) => println("unexpected"),
        Err(e) => println(show(e)),
    }
}
"#;

/// Spawn with an environment and piped streams, read and write the pipes,
/// wait, poll, kill, and spawn a missing program, on both backends.
#[test]
fn process_natives_match_the_interpreter() {
    let dir = Scratch::new("procs");
    let (interp, built) = interp_and_build(dir.path(), PROCESSES);
    assert_eq!(
        interp,
        "read [hi there]\ntaken once\nexit 3 false\nnot found\nwrote\nclosed\n\
         cat [piped through]\ncat exit 0\nstill running\nkilled\nkilled exit -1 false\n\
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
         got [ping!]\nrefused\nother -1\nbound again after the drop\n"
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
/// `BOUND_PORT`, and stops it: the transcript of what came back.
fn exchange(mut cmd: Command, requests: fn(u16) -> String) -> String {
    use std::io::{BufRead, BufReader, Read};
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the server");
    let mut out = BufReader::new(child.stdout.take().expect("stdout"));
    let mut line = String::new();
    let _ = out.read_line(&mut line);
    let Some(port) = line.trim().strip_prefix("BOUND_PORT=") else {
        let _ = child.kill();
        let mut err = String::new();
        let _ = child
            .stderr
            .take()
            .expect("stderr")
            .read_to_string(&mut err);
        panic!("the server printed {line:?}, not its port: {err}");
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
