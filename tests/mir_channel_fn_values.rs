//! B-2026-11-04-4: a value received from a channel is moved out of it, so
//! a function that returns one does not return a borrow of the receiver.
//! A channel of marked function values (`Fn(i64) -> i64 + CrossTask`)
//! drained by `for f in rx.into_iter()` failed the borrow check of the
//! library's `ReceiverIter.next` with E0509, and so did a program's own
//! `Iterator.next` that hands back `try_recv`'s value.

use std::process::Command;

fn run(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-chan-fn-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .output()
        .expect("karac __mir-run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn mir_for_over_a_channel_of_function_values_passes_the_borrow_check() {
    let src = r#"
fn main() {
    let (tx, rx): (Sender[Fn(i64) -> i64 + CrossTask], Receiver[Fn(i64) -> i64 + CrossTask]) = channel(4);
    tx.send(|x| x * 3).unwrap();
    tx.send(|x| x + 1).unwrap();
    { let d = tx; }
    let mut t = 0;
    for f in rx.into_iter() {
        t += f(10);
    }
    println(t);
}
"#;
    assert_eq!(run("for", src), "41\n");
}

#[test]
fn mir_an_iterator_whose_next_is_try_recv_of_function_values_passes_the_borrow_check() {
    let src = r#"
struct Jobs {
    rx: Receiver[Fn(i64) -> i64 + CrossTask],
}

impl Iterator for Jobs {
    type Item = Fn(i64) -> i64 + CrossTask;

    fn next(mut ref self) -> Option[Self.Item] {
        self.rx.try_recv()
    }
}

fn main() {
    let (tx, rx): (Sender[Fn(i64) -> i64 + CrossTask], Receiver[Fn(i64) -> i64 + CrossTask]) = channel(4);
    tx.send(|x| x * 5).unwrap();
    let mut jobs = Jobs { rx: rx };
    let mut t = 0;
    while let Some(f) = jobs.next() {
        t += f(2);
    }
    println(t);
}
"#;
    assert_eq!(run("try_recv", src), "10\n");
}
