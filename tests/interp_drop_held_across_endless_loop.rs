//! B-2026-11-25-56: a coroutine that suspends inside an endless loop after
//! the last use of an owned local. The local's drop sits at its scope's end,
//! which the loop never reaches, so liveness left it out of the frame, and
//! every resume that returned `Pending` left it in a dead activation: the
//! interpreter stopped with "_3: String still owns a value at return; a drop
//! is missing" (examples/ws_idle_holder). The frame holds such a local now,
//! and the program runs on until it exits.

use std::process::Command;

const EXITS: &str = r#"import std.time.{Duration, sleep};
import std.process;
fn main() {
    let s: String = "abc".to_string();
    println(s);
    let mut n = 0;
    loop {
        sleep(Duration.ms(1));
        n = n + 1;
        if n == 3 { println("done"); process.exit(0); }
    }
}
"#;

/// The same shape in a coroutine that a `timeout` drops while it waits: the
/// held value is dropped once, by the frame's drop.
const CANCELLED: &str = r#"import std.time.{Duration, sleep, timeout};
struct N { s: String }
impl Drop for N { fn drop(mut ref self) { println(f"drop {self.s}"); } }
fn w() -> i64 {
    let n = N { s: "held".to_string() };
    println(n.s);
    loop { sleep(Duration.ms(1)); }
}
fn main() { let r = timeout(Duration.ms(20), || w()); println(r.is_err()); println("end"); }
"#;

fn karac(cmd: &str, name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-held-endless-{}-{cmd}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("source.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .arg(cmd)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn held_across_endless_loop_runs_on_the_interpreter() {
    let (ok, out, err) = karac("__mir-run", "exits", EXITS);
    assert!(ok, "{err}");
    assert_eq!(out, "abc\ndone\n", "{err}");
    let (ok, out, err) = karac("__mir-run", "cancelled", CANCELLED);
    assert!(ok, "{err}");
    assert_eq!(out, "held\ndrop held\ntrue\nend\n", "{err}");
}

/// The ASan lane: nothing leaks or is freed twice, and the cancelled
/// coroutine's value drops once.
#[cfg(feature = "llvm")]
#[test]
fn held_across_endless_loop_is_clean_under_asan() {
    for (name, src, want) in [
        ("exits", EXITS, "abc\ndone\n"),
        ("cancelled", CANCELLED, "held\ndrop held\ntrue\nend\n"),
    ] {
        let (ok, out, err) = karac("__mir-llvm-asan", name, src);
        if !ok && (err.contains("Linker failed") || err.contains("not found; set KARAC_RUNTIME")) {
            eprintln!("{name}: skipping the ASan lane: {err}");
            continue;
        }
        assert!(ok, "{name}: {err}");
        assert_eq!(out, want, "{name}: {err}");
    }
}
