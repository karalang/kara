//! A stored callback that sends or receives on a channel declares it with
//! `sends(Channel)` / `receives(Channel)` (B-2026-10-25-40). A function
//! type's parameters have no names, so its clause has no `tx` or `rx` to
//! root the effect at; the effect checker charges such a callback with the
//! collapsed channel resource, and the resolver used to refuse that name in
//! a clause (`'Channel' is not an effect resource (it is a prelude type or
//! trait)`), so no clause could hold it. Under any other verb `Channel`
//! still names the prelude type.

use std::process::Command;

use karac::mir::lower::run_source;

/// `karac check` on `src`: its errors, empty when it passes.
fn check(src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-channel-clause-{}-{:x}",
        std::process::id(),
        src.len() * 31 + src.bytes().map(usize::from).sum::<usize>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    let errors: Vec<&str> = err.lines().filter(|l| l.starts_with("error")).collect();
    assert_eq!(out.status.success(), errors.is_empty(), "{err}");
    errors.join("\n")
}

fn run(src: &str) -> String {
    assert_eq!(check(src), "", "{src}");
    let r = run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(r.stderr, "", "{src}");
    r.output
}

fn refused(src: &str) -> String {
    let err = check(src);
    assert!(!err.is_empty(), "{src}");
    err
}

const RECEIVES: &str = r#"
struct Job { run: Fn(ref Receiver[String]) -> String with writes(Stdout) receives(Channel) blocks }

fn take(rx: ref Receiver[String]) -> String {
    println("taking");
    match rx.recv() {
        Some(s) => s,
        None => "none",
    }
}

fn main() {
    let (tx, rx): (Sender[String], Receiver[String]) = channel(4);
    let _ = tx.send("hi");
    let j = Job { run: take };
    println((j.run)(rx));
}
"#;

const SENDS: &str = r#"
struct Out { put: Fn(ref Sender[i64], i64) with sends(Channel) blocks }

fn put(tx: ref Sender[i64], v: i64) {
    let _ = tx.send(v);
}

pub fn drain(rx: ref Receiver[i64], n: i64) -> i64 with receives(Channel) blocks {
    let mut s = 0;
    for _ in 0..n {
        if let Some(v) = rx.recv() {
            s += v;
        }
    }
    s
}

fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(8);
    let o = Out { put: put };
    (o.put)(tx, 3);
    (o.put)(tx, 4);
    println(f"{drain(rx, 2)}");
}
"#;

#[test]
fn channel_effect_clause_on_a_receiving_slot_runs() {
    assert_eq!(run(RECEIVES), "taking\nhi\n");
}

#[test]
fn channel_effect_clause_on_a_sending_slot_and_a_public_fn_runs() {
    assert_eq!(run(SENDS), "7\n");
}

#[test]
fn channel_effect_clause_left_out_names_what_to_write() {
    for (src, want) in [
        (
            RECEIVES.replace(" receives(Channel)", ""),
            "field `run` has effect receives(Channel) not declared in slot",
        ),
        (
            SENDS.replace("with sends(Channel) blocks", "with blocks"),
            "field `put` has effect sends(Channel) not declared in slot",
        ),
    ] {
        let err = refused(&src);
        assert!(err.contains(want), "{want} in {err}");
    }
}

#[test]
fn channel_effect_clause_names_the_type_under_other_verbs() {
    let err = refused("fn f() with writes(Channel) {}\nfn main() { f(); }\n");
    assert!(
        err.contains("'Channel' is not an effect resource (it is a prelude type or trait)"),
        "{err}"
    );
}

#[test]
fn channel_effect_clause_keeps_two_receives_apart() {
    // One channel the slot cannot name is every channel: two branches that
    // receive through it conflict.
    let src = RECEIVES.replace(
        "    println((j.run)(rx));\n",
        "    let (a, b) = par { (j.run)(rx), (j.run)(rx) };\n    println(a + b);\n",
    );
    let err = refused(&src);
    assert!(
        err.contains("branches 1 and 2 of the `par` conflict"),
        "{err}"
    );
}
