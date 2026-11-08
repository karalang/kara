//! `Network` is not provided on `wasm_wasi` (design.md § Resources each
//! target provides, B-2026-11-08-37): WASI preview 1 has no socket `connect`
//! or `bind`. A program that opens a socket or makes an HTTP request passed
//! `karac check --target=wasm_wasi` and then failed in `wasm-ld` on raw
//! runtime symbols (`karac_mir_tcp`); the target gate now refuses it with
//! the call chain, and native still accepts it.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn both(tag: &str, src: &str) -> (std::process::Output, std::process::Output) {
    let path = std::env::temp_dir().join(format!(
        "karac-target-gate-wasi-net-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let wasi = karac(&["check", "--target=wasm_wasi", file]);
    let native = karac(&["check", file]);
    let _ = std::fs::remove_file(&path);
    (wasi, native)
}

fn text(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned() + &String::from_utf8_lossy(&o.stdout)
}

#[test]
fn wasm_wasi_refuses_a_tcp_connect_with_its_call_chain() {
    let (wasi, native) = both(
        "tcp",
        "import std.net.{TcpStream};
fn dial() -> i64 { match TcpStream.connect(\"127.0.0.1:1\") { Ok(_) => 1, Err(_) => 0 } }
fn main() { println(dial()); }
",
    );
    let err = text(&wasi);
    assert!(!wasi.status.success(), "{err}");
    assert!(
        err.contains(":2:26: target `wasm_wasi` does not provide resource 'Network'"),
        "{err}"
    );
    assert!(err.contains("main \u{2192} dial"), "{err}");
    assert!(native.status.success(), "{}", text(&native));
}

#[test]
fn wasm_wasi_refuses_an_http_client_request() {
    let (wasi, native) = both(
        "http",
        "fn main() {
    let c = Client.new();
    match c.get(\"http://127.0.0.1:1/\".to_string()) { Ok(r) => println(r.status()), Err(_) => println(\"get failed\") }
}
",
    );
    let err = text(&wasi);
    assert!(!wasi.status.success(), "{err}");
    assert!(
        err.contains("target `wasm_wasi` does not provide resource 'Network'"),
        "{err}"
    );
    assert!(native.status.success(), "{}", text(&native));
}
