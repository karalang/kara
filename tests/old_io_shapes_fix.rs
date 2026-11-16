//! Pre-v1 I/O call shapes the fix-coverage sweep found with no machine fix
//! (review/fixgap-io-2026-11-16.md):
//!
//! - `r.read_to_string().unwrap_or(x)` on a child's pipe, where
//!   `read_to_string` now appends to a `String` it is given and returns the
//!   count: one error whose fix spells the buffer out, where it was
//!   "expects 1 argument(s), found 0" and a cascade of `i64` errors
//!   (B-2026-11-16-203);
//! - `ws.recv_text(mut buf)`, where a `WebSocket` now reads whole messages:
//!   the error names `recv()` and `WsMessage` (B-2026-11-16-204).

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
    let dir = std::env::temp_dir().join(format!("karac-old-io-{}", std::process::id()));
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

const OLD_READ: &str = r#"fn main() {
    let child = match Command.new("sh").arg("-c").arg("printf hi; printf oops >&2")
        .stdout(Stdio.piped()).stderr(Stdio.piped()).spawn() {
        Ok(c) => c,
        Err(_) => return,
    };
    let text = "unused";
    let out = match child.stdout() {
        Some(r) => r.read_to_string().unwrap_or(String.new()),
        None => String.new(),
    };
    let err = match child.stderr() {
        Some(r) => r.read_to_string().unwrap_or(text.to_string()),
        None => String.new(),
    };
    let _ = child.wait();
    println(f"out={out.trim()} err={err.len()}");
}
"#;

#[test]
fn old_read_to_string_unwrap_or_is_one_error_with_a_fix() {
    let path = scratch("unwrap_or.kara", OLD_READ);
    let check = text(&karac(&["check"], &path));
    assert_eq!(
        check.matches("error[").count(),
        2,
        "one error per call, no i64 cascade:\n{check}"
    );
    assert!(
        check.contains("it appends to the `String` it is given"),
        "{check}"
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed.contains(
            "Some(r) => { let mut text = String.new(); match r.read_to_string(mut text) \
             { Ok(_) => text, Err(_) => String.new() } },"
        ),
        "{fixed}"
    );
    // The fallback names `text`, so the buffer takes another name.
    assert!(
        fixed.contains(
            "{ let mut read_text = String.new(); match r.read_to_string(mut read_text) \
             { Ok(_) => read_text, Err(_) => text.to_string() } }"
        ),
        "{fixed}"
    );
    assert_eq!(run_stdout(&path), "out=hi err=4\n");
}

#[test]
fn old_read_to_string_unwrap_and_expect_keep_their_panic() {
    let path = scratch(
        "unwrap.kara",
        r#"fn main() {
    let child = Command.new("sh").arg("-c").arg("printf one; printf two >&2")
        .stdout(Stdio.piped()).stderr(Stdio.piped()).spawn().unwrap();
    let a = match child.stdout() { Some(r) => r.read_to_string().unwrap(), None => String.new() };
    let b = match child.stderr() { Some(r) => r.read_to_string().expect("stderr"), None => String.new() };
    let _ = child.wait();
    println(f"{a} {b}");
}
"#,
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed.contains(
            "Some(r) => { let mut text = String.new(); r.read_to_string(mut text).unwrap(); text }"
        ),
        "{fixed}"
    );
    assert!(
        fixed.contains(
            "Some(r) => { let mut text = String.new(); r.read_to_string(mut text).expect(\"stderr\"); text }"
        ),
        "{fixed}"
    );
    assert_eq!(run_stdout(&path), "one two\n");
}

#[test]
fn current_read_to_string_is_left_alone() {
    let path = scratch(
        "current.kara",
        r#"fn main() {
    let child = Command.new("sh").arg("-c").arg("printf ok")
        .stdout(Stdio.piped()).spawn().unwrap();
    let mut s = String.new();
    match child.stdout() { Some(r) => { let _ = r.read_to_string(mut s); }, None => {} }
    let _ = child.wait();
    println(s);
}
"#,
    );
    assert_eq!(run_stdout(&path), "ok\n");
}

#[test]
fn old_websocket_recv_text_names_recv_and_ws_message() {
    let path = scratch(
        "ws.kara",
        r#"fn echo(ws: own WebSocket) {
    let mut buf: Array[u8, 64] = [0u8; 64];
    match ws.recv_text(mut buf) {
        Ok(n) => println(n),
        Err(_) => {}
    }
}

fn main() {
    let listener = TcpListener.bind("127.0.0.1:0").unwrap();
    match WebSocket.accept(listener) {
        Ok(ws) => echo(ws),
        Err(_) => {}
    }
}
"#,
    );
    let out = karac(&["check"], &path);
    let check = text(&out);
    assert!(!out.status.success(), "{check}");
    assert!(
        check.contains(
            "no method 'recv_text' on type 'WebSocket': a `WebSocket` reads whole messages, \
             not into a buffer; `ws.recv()` returns `Result[WsMessage, NetError]`, and the \
             message is `Ok(WsMessage.Text(t))`"
        ),
        "{check}"
    );
    assert_eq!(check.matches("error[").count(), 1, "{check}");
}
