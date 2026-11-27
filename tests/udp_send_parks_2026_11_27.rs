//! A `UdpSocket.send_to` whose socket's send buffer is full parks its task
//! until there is room, as a `TcpStream.write` into a full buffer does
//! (design.md §11.8: a socket send `suspends`). It held the worker: the
//! compiled call waited in `poll` inside the send, and the interpreter's
//! socket blocked, so with one worker no other task ran until the burst
//! was out.
//!
//! The send buffer fills only on a route that leaves the machine: loopback
//! hands each datagram to its receiver (or drops it) inside the send, so
//! its buffer never fills. The program sends to 192.0.2.1 (TEST-NET-1,
//! RFC 5737, which nothing answers), and the test first asks the host
//! whether a non-blocking burst there ever finds the buffer full; a host
//! with no such route skips, saying so.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(60);

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
            panic!("{name} hung\n{}", String::from_utf8_lossy(&out.stdout));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn scratch(name: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-udp-send-parks-{name}-{}",
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

/// Whether a non-blocking burst to the program's address finds the send
/// buffer full on this host.
fn host_fills_a_udp_send_buffer() -> bool {
    let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") else {
        return false;
    };
    if s.set_nonblocking(true).is_err() {
        return false;
    }
    let chunk = vec![7u8; 60000];
    for _ in 0..20000 {
        match s.send_to(&chunk, "192.0.2.1:9") {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    false
}

// The ticker is spawned first and sleeps; main then sends 800 datagrams
// of 60000 bytes, more than the default 208 KiB send buffer holds.
const BURST: &str = r#"
import std.sync.{TaskGroup};
fn ticker() {
    sleep_ms(1);
    println("tick");
}
fn main() {
    let s = UdpSocket.bind("0.0.0.0:0").unwrap();
    let group = TaskGroup.new();
    let t = group.spawn(|| ticker());
    println("burst");
    let chunk: Vec[u8] = Vec.filled(60000, 7u8);
    let mut sent = 0;
    let mut i = 0;
    while i < 800 {
        match s.send_to(chunk.as_slice(), "192.0.2.1:9".to_string()) {
            Ok(n) => sent += n,
            Err(_) => {
                println("send failed");
                return;
            }
        }
        i += 1;
    }
    println(f"sent {sent}");
    t.join();
}
"#;

/// With one worker, the ticker runs while the burst waits for room, so it
/// prints before the burst ends. Before the fix it printed after: `burst`,
/// `sent 48000000`, `tick`.
#[test]
fn a_udp_send_into_a_full_buffer_parks_and_lets_another_task_run() {
    if !host_fills_a_udp_send_buffer() {
        eprintln!(
            "SKIP: no UDP send to 192.0.2.1 finds its buffer full on this host \
             (no route off it, or one that drains faster than a burst)"
        );
        return;
    }
    let (dir, file) = scratch("built", BURST);
    let exe = dir.join("main");
    let built = karac(&file, &["build", "--mir"])
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "does not build: {err}"
    );
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    })
    .is_some()
    {
        let mut cmd = Command::new(&exe);
        cmd.env("KARAC_THREADS", "1");
        let out = run("built", cmd);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "burst\ntick\nsent 48000000\n"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The interpreter sends each datagram through the send's readiness check,
/// and reports what it came to. It produces the bytes far slower than a
/// route drains them, so its buffer never fills and the order of `tick`
/// is the scheduler's; the lines are compared as a set.
#[test]
fn a_udp_burst_through_the_readiness_check_sends_every_datagram_interpreted() {
    if !host_fills_a_udp_send_buffer() {
        eprintln!("SKIP: no UDP route off this host fills a send buffer");
        return;
    }
    let (dir, file) = scratch("interp", BURST);
    let out = run("interp", karac(&file, &["__mir-run"]));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    assert_eq!(lines, ["burst", "sent 48000000", "tick"]);
}

/// A bad address is still `send_to`'s to report: the readiness check sends
/// nothing for it, on both backends.
#[test]
fn a_udp_send_to_a_bad_address_still_fails_after_the_readiness_check() {
    let src = r#"
fn main() {
    let s = UdpSocket.bind("127.0.0.1:0").unwrap();
    match s.send_to([1u8].as_slice(), "not an address".to_string()) {
        Ok(_) => println("sent"),
        Err(_) => println("refused"),
    }
}
"#;
    let (dir, file) = scratch("bad-addr", src);
    let out = run("bad-addr", karac(&file, &["__mir-run"]));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "refused\n");
    let exe = dir.join("main");
    let built = karac(&file, &["build", "--mir"])
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    })
    .is_some()
    {
        let out = run("bad-addr-built", Command::new(&exe));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "refused\n");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
