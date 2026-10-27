//! The executor's reactor (M4 item 2, `runtime/src/mir_task.rs`): tasks
//! waiting on descriptors park off the run queue and wake when the reactor
//! names their descriptor, sleepers wake in deadline order, and a
//! `connect` in a task is two-phase, so the task waits for the socket to
//! be writable instead of blocking the thread. Each program runs on both
//! MIR backends under the coroutine transform, and under a deadline.
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
        "karac-mir-reactor-{tag}-{name}-{}",
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
    stdout_on_both_env(name, src, &[])
}

/// [`stdout_on_both`], with `env` set for the built binary's run.
fn stdout_on_both_env(name: &str, src: &str, env: &[(&str, &str)]) -> String {
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
        let mut cmd = Command::new(&exe);
        cmd.envs(env.iter().copied());
        let out = run(name, cmd);
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

/// Hundreds of tasks each park on a connection's read; the writes reach
/// them from the last to the first, and every reader wakes for its own.
#[test]
fn mir_reactor_many_parked_readers_each_wake_for_their_own_socket() {
    let src = r#"
fn serve(conn: TcpStream) -> i64 {
    let mut buf: Array[u8, 8] = [0u8; 8];
    match conn.read(mut buf) {
        Ok(n) => n,
        Err(_) => -1,
    }
}
fn main() {
    let listener = TcpListener.bind("127.0.0.1:0".to_string()).unwrap();
    let at = listener.local_addr().unwrap();
    let n = 300;
    let mut outs: Vec[TcpStream] = Vec.new();
    let group = TaskGroup.new();
    let mut handles = Vec.new();
    let mut i = 0;
    while i < n {
        outs.push(TcpStream.connect(at.clone()).unwrap());
        let conn = listener.accept().unwrap();
        handles.push(group.spawn(|| serve(conn)));
        i += 1;
    }
    // Every reader runs, finds nothing and parks before the first write.
    sleep_ms(20);
    let two: Vec[u8] = Vec.filled(2, 7u8);
    let mut j = n - 1;
    while j >= 0 {
        match outs[j].write(two[0..2]) {
            Ok(_) => {}
            Err(_) => println("write failed"),
        }
        j -= 1;
    }
    let mut total = 0;
    while let Some(h) = handles.pop() {
        total += h.join();
    }
    println(f"read {total}");
}
"#;
    assert_eq!(stdout_on_both("parked", src), "read 600\n");
}

/// A task's `connect` waits for its socket while the task that accepts
/// runs, and a refused one comes back as an error.
#[test]
fn mir_reactor_a_connect_in_a_task_waits_while_the_acceptor_runs() {
    let src = r#"
fn client(at: String) -> i64 {
    let s = TcpStream.connect(at).unwrap();
    let msg: Vec[u8] = Vec.filled(3, 1u8);
    match s.write(msg[0..3]) {
        Ok(n) => n,
        Err(_) => -1,
    }
}
fn refused() -> bool {
    match TcpStream.connect("127.0.0.1:1".to_string()) {
        Ok(_) => false,
        Err(_) => true,
    }
}
fn main() {
    let listener = TcpListener.bind("127.0.0.1:0".to_string()).unwrap();
    let at = listener.local_addr().unwrap();
    let group = TaskGroup.new();
    let c = group.spawn(|| client(at));
    let r = group.spawn(|| refused());
    let conn = listener.accept().unwrap();
    let mut got = 0;
    let mut buf: Array[u8, 8] = [0u8; 8];
    while got < 3 {
        match conn.read(mut buf) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(_) => break,
        }
    }
    println(f"got {got} wrote {c.join()} refused {r.join()}");
}
"#;
    assert_eq!(
        stdout_on_both("connect", src),
        "got 3 wrote 3 refused true\n"
    );
}

/// Sleepers wake in the order of their deadlines, and in the order they
/// slept at the same deadline, while a task parked on a socket waits. On
/// one worker: on the pool, two sleepers due at once wake on two workers,
/// in an order core-semantics §11.1 leaves open; the pool's own deadline
/// order is the next test's.
#[test]
fn mir_reactor_sleepers_wake_in_deadline_order_beside_a_parked_task() {
    let src = r#"
fn nap(ms: i64, tag: i64) -> i64 {
    sleep_ms(ms);
    println(f"{tag}");
    tag
}
fn wait_byte(conn: TcpStream) -> i64 {
    let mut buf: Array[u8, 8] = [0u8; 8];
    match conn.read(mut buf) {
        Ok(n) => n,
        Err(_) => -1,
    }
}
fn main() {
    let listener = TcpListener.bind("127.0.0.1:0".to_string()).unwrap();
    let at = listener.local_addr().unwrap();
    let out = TcpStream.connect(at).unwrap();
    let conn = listener.accept().unwrap();
    let group = TaskGroup.new();
    let w = group.spawn(|| wait_byte(conn));
    let a = group.spawn(|| nap(60, 3));
    let b = group.spawn(|| nap(20, 1));
    let c = group.spawn(|| nap(40, 2));
    let d = group.spawn(|| nap(20, 11));
    sleep_ms(80);
    let one: Vec[u8] = Vec.filled(1, 9u8);
    match out.write(one[0..1]) {
        Ok(_) => {}
        Err(_) => println("write failed"),
    }
    println(f"woke {w.join()} sum {a.join() + b.join() + c.join() + d.join()}");
}
"#;
    assert_eq!(
        stdout_on_both_env("sleepers", src, &[("KARAC_THREADS", "1")]),
        "1\n11\n2\n3\nwoke 1 sum 17\n"
    );
}

/// On the pool, sleepers wake in the order of their deadlines while a task
/// parked on a socket waits. The first park opened the reactor's `epoll`
/// set under the executor's lock, and moving it to a high descriptor
/// number waited out an RCU grace period with every worker stalled: the
/// sleeper spawned first started 48 ms before the others and woke before
/// sleepers due 40 ms ahead of it (B-2026-10-27-5).
#[test]
fn mir_reactor_pool_sleepers_wake_in_deadline_order() {
    let src = r#"
fn nap(ms: i64, tag: i64) -> i64 {
    sleep_ms(ms);
    println(f"{tag}");
    tag
}
fn wait_byte(conn: TcpStream) -> i64 {
    let mut buf: Array[u8, 8] = [0u8; 8];
    match conn.read(mut buf) {
        Ok(n) => n,
        Err(_) => -1,
    }
}
fn main() {
    let listener = TcpListener.bind("127.0.0.1:0".to_string()).unwrap();
    let at = listener.local_addr().unwrap();
    let out = TcpStream.connect(at).unwrap();
    let conn = listener.accept().unwrap();
    let group = TaskGroup.new();
    let w = group.spawn(|| wait_byte(conn));
    let a = group.spawn(|| nap(140, 4));
    let b = group.spawn(|| nap(20, 1));
    let c = group.spawn(|| nap(100, 3));
    let d = group.spawn(|| nap(60, 2));
    sleep_ms(160);
    let one: Vec[u8] = Vec.filled(1, 9u8);
    match out.write(one[0..1]) {
        Ok(_) => {}
        Err(_) => println("write failed"),
    }
    println(f"woke {w.join()} sum {a.join() + b.join() + c.join() + d.join()}");
}
"#;
    assert_eq!(
        stdout_on_both_env("pool_sleepers", src, &[("KARAC_THREADS", "4")]),
        "1\n2\n3\n4\nwoke 1 sum 10\n"
    );
}
