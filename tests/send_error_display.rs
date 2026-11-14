//! `SendError`'s `Display` is "channel full" / "channel closed"
//! (concurrency.md; runtime/stdlib/channel.kara), through `f"{e}"`,
//! `e.to_string()`, a `ref SendError` parameter and a `?` into `AnyError`,
//! on the MIR interpreter and the compiled backend (B-2026-11-14-30). They
//! printed the derived `Full(2)` / `Closed(3)`: the MIR builder lowers no
//! baked impl, and the MIR library had no copy of this one.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn run(mode: &str, src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-send-error-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const SRC: &str = r#"import std.sync.{channel, Sender, Receiver, SendError};

fn show(e: ref SendError[i64]) -> String {
    f"{e}"
}

fn forward(tc: ref Sender[i64]) -> Result[(), AnyError] {
    tc.try_send(2)?;
    Ok(())
}

fn main() {
    let (tc, rc): (Sender[i64], Receiver[i64]) = channel(1);
    tc.send(1).unwrap();
    match tc.try_send(2) {
        Err(e) => {
            println(f"{e}");
            println(e.to_string());
            println(show(e));
        }
        Ok(_) => println("ok"),
    }
    match forward(tc) {
        Err(e) => println(e.to_string()),
        Ok(_) => println("ok"),
    }
    let first = rc.recv();
    println(first.unwrap());
    drop(rc);
    match tc.try_send(3) {
        Err(e) => println(f"{e} / {e.to_string()}"),
        Ok(_) => println("ok"),
    }
}
"#;

const WANT: &str = "channel full\nchannel full\nchannel full\nchannel full\n1\n\
                    channel closed / channel closed\n";

fn check(mode: &str) {
    let out = run(mode, SRC);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT, "{mode}");
}

#[test]
fn interp_send_error_display() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_send_error_display() {
    check("__mir-llvm-run");
}
