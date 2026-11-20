//! `Mutex[T].try_lock` (B-2026-11-19-30, docs/library/concurrency.md
//! § `Mutex[T]`): the guard when no task holds the mutex, `None` when one
//! does, the caller included; it never waits, so it adds no effect. The
//! dining philosophers' back-off form (kata 1226) holds one fork, tries the
//! other, and lets the first go when the second is taken.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-mutex-try-lock-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn runs_as(tag: &str, src: &str, want: &str) {
    let check = karac(tag, &["check"], src);
    assert!(check.status.success(), "{tag}: check: {}", text(&check));
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac(tag, &[b], src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} on {b}: {}",
            text(&out)
        );
        assert!(out.status.success(), "{tag} on {b}: {:?}", out.status);
    }
}

const HELD: &str = r#"fn main() {
    let m = Mutex.new(5);
    {
        let g = m.lock();
        match m.try_lock() {
            Some(_) => println("got while held"),
            None => println("held"),
        }
        *g.value += 1;
    }
    match m.try_lock() {
        Some(g) => {
            *g.value += 10;
            println(f"free {g.value}");
        }
        None => println("held"),
    }
    let g = m.lock();
    println(g.value);
}
"#;

const PHILOSOPHERS: &str = r#"import std.time.{sleep, Duration};
pub fn peek(m: ref Mutex[i64]) -> bool {
    m.try_lock().is_some()
}
fn dine(forks: ref Vec[Mutex[i64]], i: i64) {
    let left = i;
    let right = (i + 1) % 5;
    loop {
        let l = forks[left].lock();
        if let Some(r) = forks[right].try_lock() {
            *l.value += 1;
            *r.value += 1;
            return;
        }
        drop(l);
        sleep(Duration.ms(0));
    }
}
fn main() {
    let forks: Vec[Mutex[i64]] = [Mutex.new(0), Mutex.new(0), Mutex.new(0), Mutex.new(0), Mutex.new(0)];
    {
        let g = TaskGroup.new();
        for i in 0..5 {
            g.spawn(|| dine(forks, i));
        }
    }
    let mut total = 0;
    for f in forks.iter() {
        let g = f.lock();
        total += g.value;
    }
    println(f"uses {total}");
    println(peek(forks[0]));
}
"#;

#[test]
fn try_lock_is_none_while_held_and_the_guard_once_free() {
    runs_as("held", HELD, "held\nfree 16\n16\n");
}

#[test]
fn try_lock_backs_off_in_the_dining_philosophers() {
    // Five philosophers each eat once, two forks a meal; `peek` is a public
    // function that declares no effect around a `try_lock`.
    runs_as("philosophers", PHILOSOPHERS, "uses 10\ntrue\n");
}
