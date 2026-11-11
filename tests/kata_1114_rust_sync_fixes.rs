//! Kata 1114's Rust arm keeps two `std::sync::mpsc` channels behind
//! `Mutex`es, as Rust writes them: `use std::sync::mpsc::{channel,
//! Receiver, Sender};`, `channel()`, `self.tx.lock().unwrap().send(())`,
//! and its polling arm sleeps with `Duration::from_millis`. `karac fix`
//! left an import of a module that does not exist and four errors with no
//! fix. Now the import goes (the prelude has the three names), `.unwrap()`
//! on a guard goes (a lock is never poisoned), a method of the guarded
//! value is reached through `.value`, `Duration.from_millis` and
//! `from_secs` become `Duration.ms` and `secs`, and a `channel()` with no
//! capacity says what to write, since no capacity can be picked for it.

use std::process::{Command, Stdio};

/// `src` after `karac fix` until nothing changes, and its `check` stderr.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1114-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (out, String::from_utf8_lossy(&check.stderr).into_owned())
}

fn clean(check: &str) -> bool {
    check.contains("All checks passed") || check.is_empty()
}

#[test]
fn kata_1114_rust_mpsc_behind_a_mutex_is_carried_over() {
    let src = "use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

struct Gate {
    tx: Mutex<Sender<i64>>,
    rx: Mutex<Receiver<i64>>,
}

fn main() {
    let (tx, rx) = channel(4);
    let g = Gate { tx: Mutex::new(tx), rx: Mutex::new(rx) };
    g.tx.lock().unwrap().send(7).unwrap();
    let n = g.rx.lock().unwrap().recv().unwrap();
    println(f\"{n}\");
}
";
    let (text, check) = fixed("mpsc", src);
    assert!(!text.contains("mpsc"), "{text}");
    assert!(!text.contains("lock().unwrap()"), "{text}");
    assert!(
        text.contains("g.tx.lock().value.send(7).unwrap();"),
        "{text}"
    );
    assert!(
        text.contains("g.rx.lock().value.recv().unwrap();"),
        "{text}"
    );
    assert!(clean(&check), "{check}\n{text}");
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "7\n");
}

#[test]
fn kata_1114_a_channel_with_no_capacity_says_what_to_write() {
    let src = "fn main() {
    let (tx, rx) = channel();
    let _ = tx.send(1);
    println(f\"{rx.recv()}\");
}
";
    let (text, check) = fixed("cap", src);
    assert_eq!(text, src, "no capacity is picked for it");
    assert!(check.contains("`channel` takes its capacity"), "{check}");
    assert!(check.contains("bounded"), "{check}");
}

#[test]
fn kata_1114_rusts_duration_constructors_become_karas() {
    let src = "import std.time.{Duration, sleep};

fn main() {
    sleep(Duration.from_millis(1));
    let d = Duration::from_secs(2);
    println(f\"{d.as_ms()}\");
}
";
    let (text, check) = fixed("duration", src);
    assert!(text.contains("sleep(Duration.ms(1));"), "{text}");
    assert!(text.contains("let d = Duration.secs(2);"), "{text}");
    assert!(clean(&check), "{check}\n{text}");
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "2000\n");
}

#[test]
fn kata_1114_a_sub_millisecond_duration_says_what_kara_has() {
    let src = "import std.time.{Duration, sleep};

fn main() {
    sleep(Duration.from_micros(10));
}
";
    let (text, check) = fixed("micros", src);
    assert_eq!(text, src);
    assert!(
        check.contains("`Duration.ms(n)` or `Duration.secs(n)`"),
        "{check}"
    );
}
