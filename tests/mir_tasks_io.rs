//! Tasks that wait on I/O beyond `accept` and `read` (M4 item 2): a
//! `TcpStream.write` into a full buffer, a `UdpSocket.recv`, and a
//! `Child.wait`, each of which parks its task until the descriptor is
//! ready, so the other tasks run meanwhile. Each program runs on both MIR
//! backends under the coroutine transform, as `tests/mir_tasks.rs` does,
//! and under a deadline: before the fix the waiting call blocked the one
//! thread, and the task it waited on never ran.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a program may run before it counts as hung.
const DEADLINE: Duration = Duration::from_secs(20);

/// Runs `cmd` to its end, or kills it at [`DEADLINE`] and fails.
fn run(name: &str, mut cmd: Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "{name} hung: a waiting task kept the others from running\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn scratch(name: &str, tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-tasks-io-{tag}-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(file: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_MIR_COROUTINES", "1").args(args).arg(file);
    c
}

/// The program's stdout on the interpreter and, when the runtime archive
/// links, on the built binary, which must print the same lines.
fn stdout_on_both(name: &str, src: &str) -> String {
    let (dir, file) = scratch(name, "i", src);
    let out = run(name, karac(&file, &["__mir-run"]));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name} on the interpreter: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let interp = String::from_utf8(out.stdout).unwrap();

    let (dir, file) = scratch(name, "b", src);
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_COROUTINES", "1")
        .args(["build", "--mir", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        let out = run(name, Command::new(&exe));
        assert!(
            out.status.success(),
            "{name} built: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            interp,
            "{name}: the build and the interpreter differ"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    interp
}

/// A writer task fills the socket's buffer and keeps writing; it parks
/// until the reader task, which runs only meanwhile, drains it.
#[test]
fn mir_tasks_a_write_into_a_full_buffer_lets_the_reader_run() {
    let src = r#"
fn reader(conn: TcpStream, want: i64) -> i64 {
    let mut got = 0;
    let mut buf: Array[u8, 4096] = [0u8; 4096];
    while got < want {
        match conn.read(mut buf) {
            Ok(0) => return got,
            Ok(n) => got += n,
            Err(_) => return -1,
        }
    }
    got
}
fn main() {
    let listener = TcpListener.bind("127.0.0.1:47641".to_string()).unwrap();
    let out = TcpStream.connect("127.0.0.1:47641".to_string()).unwrap();
    let conn = listener.accept().unwrap();
    let total = 8 * 1024 * 1024;
    let group = TaskGroup.new();
    let r = group.spawn(|| reader(conn, total));
    let chunk: Vec[u8] = Vec.filled(65536, 7u8);
    let mut sent = 0;
    while sent < total {
        // A write may take part of the chunk; the last takes the rest.
        let left = total - sent;
        let end = if left < chunk.len() { left } else { chunk.len() };
        match out.write(chunk[0..end]) {
            Ok(n) => sent += n,
            Err(_) => { println("write failed"); return; }
        }
    }
    println(f"sent {sent}");
    println(f"read {r.join()}");
}
"#;
    assert_eq!(stdout_on_both("write", src), "sent 8388608\nread 8388608\n");
}

/// A task waiting for a datagram lets the task that sends it run.
#[test]
fn mir_tasks_a_recv_waits_for_a_later_sender() {
    let src = r#"
fn send(to: String) {
    sleep_ms(50);
    let s = UdpSocket.bind("127.0.0.1:0").unwrap();
    println("sending");
    match s.send_to("ping".to_string().bytes(), to) { Ok(n) => println(f"sent {n}"), Err(_) => println("send failed") }
}
fn main() {
    let r = UdpSocket.bind("127.0.0.1:0").unwrap();
    let at = r.local_addr().unwrap();
    let group = TaskGroup.new();
    let s = group.spawn(|| send(at));
    let mut buf: Array[u8, 8] = [0u8; 8];
    match r.recv(mut buf) {
        Ok(n) => println(f"got {n}"),
        Err(_) => println("recv failed"),
    }
    s.join();
}
"#;
    assert_eq!(stdout_on_both("recv", src), "sending\nsent 4\ngot 4\n");
}

/// A task waiting for a child to exit lets the other tasks run meanwhile.
#[test]
fn mir_tasks_a_child_wait_lets_other_tasks_run() {
    let src = r#"
fn ticks(n: i64) {
    let mut i = 0;
    while i < n {
        sleep_ms(50);
        println(f"tick {i}");
        i += 1;
    }
}
fn main() {
    let child = Command.new("sleep").arg("0.6").spawn().unwrap();
    let group = TaskGroup.new();
    let t = group.spawn(|| ticks(3));
    match child.wait() {
        Ok(st) => println(f"child exited {st.success()}"),
        Err(_) => println("wait failed"),
    }
    t.join();
}
"#;
    assert_eq!(
        stdout_on_both("child", src),
        "tick 0\ntick 1\ntick 2\nchild exited true\n"
    );
}

/// A task reading a child's output pipe lets the other tasks run meanwhile:
/// each reader runs first and waits for output its child prints only once
/// the writer spawned after it has run, through `read_to_string` (which
/// waits for the end) and then a `read` loop, one pair at a time so the
/// lines come in one order.
#[test]
fn mir_tasks_a_pipe_read_lets_the_writer_run() {
    let src = r#"
fn reader(out: own ChildStdout) {
    let mut s = String.new();
    match out.read_to_string(s) {
        Ok(n) => println(f"read_to_string [{s}] {n}"),
        Err(_) => println("read_to_string failed"),
    }
}
fn chunked(out: own ChildStdout) {
    let mut buf: Vec[u8] = vec![0, 0, 0, 0];
    let mut total = 0;
    loop {
        match out.read(mut buf) {
            Ok(0) => break,
            Ok(n) => total = total + n,
            Err(_) => break,
        }
    }
    println(f"read {total} bytes");
}
fn writer(inp: own ChildStdin, tag: own String, line: own String) {
    println(f"writing {tag}");
    match inp.write_all(line.bytes()) {
        Ok(_) => {}
        Err(_) => println("write failed"),
    }
    let _ = inp.close();
}
fn whole(out: own ChildStdout, inp: own ChildStdin) {
    let mut tg = TaskGroup.new();
    tg.spawn(|| reader(out));
    tg.spawn(|| writer(inp, "a", "hi\n"));
}
fn in_chunks(out: own ChildStdout, inp: own ChildStdin) {
    let mut tg = TaskGroup.new();
    tg.spawn(|| chunked(out));
    tg.spawn(|| writer(inp, "b", "there\n"));
}
fn main() {
    let a = Command.new("sh").arg("-c").arg("read x; printf \"got $x\"")
        .stdin(Stdio.piped()).stdout(Stdio.piped()).spawn().unwrap();
    let b = Command.new("sh").arg("-c").arg("read x; printf \"0123456789 $x\"")
        .stdin(Stdio.piped()).stdout(Stdio.piped()).spawn().unwrap();
    let (ao, ai) = (a.stdout().unwrap(), a.stdin().unwrap());
    let (bo, bi) = (b.stdout().unwrap(), b.stdin().unwrap());
    whole(ao, ai);
    in_chunks(bo, bi);
    let _ = a.wait();
    let _ = b.wait();
    println("done");
}
"#;
    assert_eq!(
        stdout_on_both("pipes", src),
        "writing a\nread_to_string [got hi] 6\nwriting b\nread 16 bytes\ndone\n"
    );
}

/// An HTTP server that answers in pairs: it reads two requests, answers
/// `/second`, then `/first`. A client that holds the one thread while its
/// request waits never lets the second request start. It goes by path, not
/// by the order it accepted them: the helpers that send the two requests
/// connect in either order.
fn pairs_server() -> u16 {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || loop {
        let mut pair = Vec::new();
        while pair.len() < 2 {
            let Ok((mut s, _)) = l.accept() else { return };
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => req.extend_from_slice(&buf[..n]),
                }
            }
            let line = String::from_utf8_lossy(&req);
            let path = line
                .split(' ')
                .nth(1)
                .unwrap_or("/")
                .trim_start_matches('/')
                .to_string();
            pair.push((s, path));
        }
        pair.sort_by(|a, b| b.1.cmp(&a.1));
        for (mut s, path) in pair {
            let body = format!("answer {path}");
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            drop(s);
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    port
}

/// A task whose HTTP request waits on its answer lets the other tasks run:
/// the first request's answer comes only once the second, sent by a task
/// spawned after it, has arrived, through `Client.get` and a built
/// `RequestBuilder.send`.
#[test]
fn mir_tasks_an_http_request_lets_the_next_task_send_its_own() {
    let port = pairs_server();
    let src = r#"
fn fetch(url: own String) {
    let c = Client.new();
    match c.get(url) {
        Ok(r) => println(f"get {r.status()} {r.body()}"),
        Err(e) => println(f"get failed {e.message()}"),
    }
}
fn built(url: own String) {
    let c = Client.new();
    match c.request("GET", url).header("X-Pair", "1").send() {
        Ok(r) => println(f"send {r.status()} {r.body()}"),
        Err(e) => println(f"send failed {e.message()}"),
    }
}
fn main() {
    let mut tg = TaskGroup.new();
    tg.spawn(|| fetch("http://127.0.0.1:PORT/first"));
    tg.spawn(|| built("http://127.0.0.1:PORT/second"));
}
"#
    .replace("PORT", &port.to_string());
    assert_eq!(
        stdout_on_both("http", &src),
        "send 200 answer second\nget 200 answer first\n"
    );
}

/// An HTTP server that takes each connection and never answers.
fn silent_server() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((s, _)) = l.accept() {
            held.push(s);
        }
    });
    port
}

/// A task cancelled while its HTTP request waits on a helper is torn down
/// at that wait; the request's answer, which never comes here, is not
/// waited for.
#[test]
fn mir_tasks_a_cancelled_http_request_tears_its_task_down() {
    let port = silent_server();
    let src = r#"
fn fetch(url: own String) -> i64 {
    let c = Client.new();
    match c.get(url) {
        Ok(r) => r.status(),
        Err(_) => -1,
    }
}
fn main() {
    let group = TaskGroup.new();
    let h = group.spawn(|| fetch("http://127.0.0.1:PORT/alone"));
    sleep_ms(50);
    println("cancelling");
    group.cancel();
    match h.try_join() {
        Ok(v) => println(f"fetch {v}"),
        Err(_) => println("fetch cancelled"),
    }
}
"#
    .replace("PORT", &port.to_string());
    assert_eq!(
        stdout_on_both("http-cancel", &src),
        "cancelling\nfetch cancelled\n"
    );
}
