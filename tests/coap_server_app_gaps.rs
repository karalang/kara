//! Gaps the coap_server app (kara-katas apps/service/coap_server) found
//! porting a Rust-shaped draft: `handle.join().unwrap()` said only "no
//! method 'unwrap'" (B-2026-11-15-150), `collect::<Vec<_>>()` as a receiver
//! had no fix (B-2026-11-15-151), `use std::thread;` for a spawn became an
//! import of a module that does not exist (B-2026-11-15-152),
//! `set_read_timeout(Some(Duration::from_millis(n)))` mended to a
//! `Duration` where the milliseconds go, with no fix (B-2026-11-15-153), and
//! a `filter` predicate's capture was held against a `collect`ed
//! `Vec[ref T]` returned from the function (B-2026-11-15-154).
//! `Some(w)` with `w: u64` passed an `Option[i64]` slot with no `as`
//! (B-2026-11-15-155).

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-coap-gaps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac fix` until it has nothing left to apply, as the Mend loop runs it.
fn fix_to_fixpoint(path: &std::path::Path) -> String {
    let mut text_now = std::fs::read_to_string(path).unwrap();
    for _ in 0..8 {
        let _ = karac(&["fix"], path);
        let now = std::fs::read_to_string(path).unwrap();
        if now == text_now {
            return now;
        }
        text_now = now;
    }
    panic!("karac fix did not settle on {}", path.display());
}

fn run_stdout(path: &std::path::Path) -> String {
    let check = karac(&["check"], path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], path);
    assert!(run.status.success(), "{}", text(&run));
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[test]
fn a_task_handles_join_unwrap_is_dropped() {
    let path = scratch(
        "join.kara",
        "fn work(n: i64) -> i64 { n * 2 }

fn main() {
    let group = TaskGroup.new();
    let h = group.spawn(|| work(4));
    let v = h.join().unwrap();
    let r = group.spawn(|| if work(1) > 0 { Ok(1) } else { Err(\"no\") });
    let w = r.join().unwrap();
    println(f\"{v} {w}\");
}
",
    );
    let check = karac(&["check"], &path);
    let said = text(&check);
    assert!(said.contains("a `TaskHandle`'s `join()`"), "{said}");
    // The `Result` the second task returns keeps its `unwrap`.
    assert_eq!(said.matches("no method 'unwrap'").count(), 1, "{said}");
    let fixed = fix_to_fixpoint(&path);
    assert!(fixed.contains("let v = h.join();"), "{fixed}");
    assert!(fixed.contains("let w = r.join().unwrap();"), "{fixed}");
    assert_eq!(run_stdout(&path), "8 1\n");
}

#[test]
fn a_collect_with_holes_as_a_receiver_names_the_bare_collection() {
    let path = scratch(
        "holes.kara",
        "fn hex(b: ref Vec[u8]) -> String {
    b.iter().map(|x| f\"{x:02x}\").collect::<Vec<_>>().join(\"\")
}

fn main() {
    let words = vec![\"b\", \"a\", \"b\"];
    let n = words.iter().collect::<HashSet<_>>().len();
    println(f\"{hex(vec![1u8, 171])} {n}\");
}
",
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed.contains("Vec.from_iter(b.iter().map(|x| f\"{x:02x}\")).join(\"\")"),
        "{fixed}"
    );
    assert!(
        fixed.contains("Set.from_iter(words.iter()).len()"),
        "{fixed}"
    );
    assert_eq!(run_stdout(&path), "01ab 2\n");
}

#[test]
fn use_std_thread_for_a_spawn_alone_is_deleted() {
    // Once the spawn is in a group, the line has nothing left to import.
    let path = scratch(
        "thread.kara",
        "use std::thread;

fn main() {
    let group = TaskGroup.new();
    let h = group.spawn(|| 3);
    println(f\"{h.join()}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(
        text(&check).contains("which needs no import"),
        "{}",
        text(&check)
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(!fixed.contains("thread"), "{fixed}");
    assert_eq!(run_stdout(&path), "3\n");

    // With the spawn still there, the line goes and the spawn keeps its own
    // diagnostic.
    let path = scratch(
        "thread-spawn.kara",
        "use std::thread;

fn main() {
    let h = thread::spawn(|| 3);
    println(f\"{h.join()}\");
}
",
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(!fixed.contains("import std.thread"), "{fixed}");
    assert!(!fixed.contains("use std::thread"), "{fixed}");
    let check = karac(&["check"], &path);
    assert!(
        text(&check).contains("Kāra has no free `spawn`"),
        "{}",
        text(&check)
    );
}

#[test]
fn a_duration_read_timeout_mends_to_milliseconds() {
    let path = scratch(
        "timeout.kara",
        "use std::net::UdpSocket;
use std::time::Duration;

fn main() {
    let sock = UdpSocket::bind(\"127.0.0.1:0\").unwrap();
    let wait: i64 = 20;
    sock.set_read_timeout(Some(Duration::from_millis(wait))).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(wait + 1))).unwrap();
    let d = Duration::from_millis(30);
    sock.set_read_timeout(Some(d)).unwrap();
    let mut buf = [0u8; 4];
    match sock.recv_from(&mut buf) {
        Ok(_) => println!(\"data\"),
        Err(_) => println!(\"timed out\"),
    }
}
",
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(fixed.contains("set_read_timeout(Some(wait))"), "{fixed}");
    assert!(
        fixed.contains("set_read_timeout(Some((wait + 1) * 1000))"),
        "{fixed}"
    );
    assert!(
        fixed.contains("set_read_timeout(Some(d.as_ms()))"),
        "{fixed}"
    );
    assert_eq!(run_stdout(&path), "timed out\n");
}

#[test]
fn a_filter_predicates_capture_is_not_held_by_the_collected_views() {
    let path = scratch(
        "filter.kara",
        "fn big(v: ref Vec[i64], k: i64) -> Vec[ref i64] {
    v.iter().filter(|x| *x > k).collect()
}

struct Msg {
    options: Vec[(u16, Vec[u8])],
}

impl Msg {
    fn option(self, num: u16) -> Vec[ref Vec[u8]] {
        self.options.iter().filter(|(n, _)| *n == num).map(|(_, v)| v).collect()
    }
}

fn main() {
    let v = vec![1, 5, 3, 9];
    let b = big(v, 2);
    println(f\"{b.len()}\");
    let m = Msg { options: vec![(11, vec![1u8, 2]), (15, vec![3u8]), (11, vec![4u8])] };
    let o = m.option(11);
    println(f\"{o.len()} {o[1][0]}\");
}
",
    );
    assert_eq!(run_stdout(&path), "3\n2 4\n");
}

#[test]
fn a_next_through_a_filter_adaptor_returns_a_view_of_the_source() {
    // The adaptor keeps the closure and its borrow of the local `k2`, but
    // `next`'s `Option[ref i64]` has no place for it.
    let path = scratch(
        "filter-adaptor.kara",
        "fn first_big(v: ref Vec[i64], k: i64) -> Option[ref i64] {
    let k2 = k * 2;
    let mut it = v.iter().filter(|x| *x > k2);
    it.next()
}

fn main() {
    let v = vec![1, 5, 3, 9];
    println(f\"{first_big(v, 2).unwrap()}\");
}
",
    );
    assert_eq!(run_stdout(&path), "5\n");
}

#[test]
fn a_some_payload_that_changes_sign_needs_as() {
    // `Some(w)` with `w: u64` into an `Option[i64]` slot passed and printed
    // -1 (B-2026-11-15-155); it is the coercion `let x: i64 = w` is.
    let path = scratch(
        "payload-sign.kara",
        "fn take(ms: Option[i64]) -> i64 { ms.unwrap() }

fn main() {
    let w: u64 = 18446744073709551615;
    println(f\"{take(Some(w))}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains("implicit coercion from 'u64' to 'i64'"),
        "{}",
        text(&check)
    );
    let path = scratch(
        "payload-widen.kara",
        "fn take(ms: Option[i64]) -> i64 { ms.unwrap() }

fn main() {
    let w: u32 = 7;
    let c: Option[i32] = Some(0 - 1);
    println(f\"{take(Some(w))} {take(Some(w as i64 * 2))} {c.unwrap()}\");
}
",
    );
    assert_eq!(run_stdout(&path), "7 14 -1\n");
}
