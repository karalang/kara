//! Two `par` branches that use two different channels do not conflict
//! (core semantics §11.2), but when one branch both sent on one channel and
//! received on another, E0418 named the two sends as one channel (B-2026-11-15-87).
//! The branch's closure captures `ref txa` and `ref rxb`, and the effect
//! checker's origins read a closure as something a value can be stored
//! through, so each capture took the other's origins and `txa` came to
//! share `rxb`'s `channel()` call with `txb`. A `ref` to a `Sender[i64]` or
//! `Receiver[i64]` takes nothing in that holds a resource, so it no longer
//! links them. One channel used in both branches, directly or through a
//! clone, still conflicts.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn check(name: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

const SEND_AND_RECEIVE: &str = r#"import std.sync.{Receiver, Sender};
fn main() {
    let (txa, rxa): (Sender[i64], Receiver[i64]) = channel(8);
    let (txb, rxb): (Sender[i64], Receiver[i64]) = channel(8);
    let _ = txa.send(5);
    let (a, b) = par {
        { let _ = txb.send(1); 0 },
        { let _ = txa.send(2); rxb.recv().unwrap() },
    };
    println(f"{a} {b} {rxa.recv().unwrap()} {rxa.recv().unwrap()}");
}
"#;
const RECEIVE_BOTH: &str = r#"
import std.sync.{Receiver, Sender};
fn main() {
    let (txa, rxa): (Sender[i64], Receiver[i64]) = channel(8);
    let (txb, rxb): (Sender[i64], Receiver[i64]) = channel(8);
    let _ = txa.send(5);
    let (a, b) = par {
        { rxb.recv().unwrap() },
        { let _ = txb.send(6); rxa.recv().unwrap() },
    };
    println(f"{a} {b} {0}");
}
"#;
const SAME_CHANNEL: &str = r#"import std.sync.{Receiver, Sender};
fn main() {
    let (txa, rxa): (Sender[i64], Receiver[i64]) = channel(8);
    let (txb, rxb): (Sender[i64], Receiver[i64]) = channel(8);
    let (a, b) = par {
        { let _ = txa.send(1); 0 },
        { let _ = txa.send(2); rxb.try_recv().unwrap_or(7) },
    };
    println(f"{a} {b} {rxa.recv().unwrap()} {txb.send(0).is_ok()}");
}
"#;
const CLONED_SENDER: &str = r#"import std.sync.{Receiver, Sender};
fn main() {
    let (txa, rxa): (Sender[i64], Receiver[i64]) = channel(8);
    let (txb, rxb): (Sender[i64], Receiver[i64]) = channel(8);
    let t2 = txa.clone();
    let (a, b) = par {
        { let _ = txa.send(1); 0 },
        { let _ = t2.send(2); rxb.try_recv().unwrap_or(7) },
    };
    println(f"{a} {b} {rxa.recv().unwrap()} {txb.send(0).is_ok()}");
}
"#;

#[test]
fn probe_pc1_a_branch_that_sends_and_receives_on_two_channels_is_no_conflict() {
    let (ok, stderr) = check("pc1-send-recv", SEND_AND_RECEIVE);
    assert!(ok, "{stderr}");
    let out = karac::mir::lower::run_source(SEND_AND_RECEIVE)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "0 1 5 2\n");
}

#[test]
fn probe_pc1_receives_on_two_channels_are_no_conflict() {
    let (ok, stderr) = check("pc1-recv-both", RECEIVE_BOTH);
    assert!(ok, "{stderr}");
    let out = karac::mir::lower::run_source(RECEIVE_BOTH)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "6 5 0\n");
}

#[test]
fn probe_pc1_one_channel_sent_on_in_both_branches_still_conflicts() {
    for (name, src) in [("pc1-same", SAME_CHANNEL), ("pc1-clone", CLONED_SENDER)] {
        let (ok, stderr) = check(name, src);
        assert!(!ok, "{name} checked");
        assert!(
            stderr.contains("branches 1 and 2 of the `par` conflict: `sends(Channel"),
            "{name}: {stderr}"
        );
    }
}
