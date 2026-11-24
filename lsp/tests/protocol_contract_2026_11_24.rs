//! design.md § 17 Language server, the sentences the 2026-11-24 tooling
//! sentence map found without a pin (conformance/design/tooling/SENTENCE_MAP.md):
//! a request the server does not serve answers `MethodNotFound`;
//! `kara/renderCrashReport` takes the record `karac run --output=json`
//! carries; `didSave` re-checks and publishes; positions count UTF-16 code
//! units.

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::json;

fn req(client: &Connection, id: i32, method: &str, params: serde_json::Value) {
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(id),
            method: method.to_string(),
            params,
        }))
        .unwrap();
}

fn notify(client: &Connection, method: &str, params: serde_json::Value) {
    client
        .sender
        .send(Message::Notification(Notification {
            method: method.to_string(),
            params,
        }))
        .unwrap();
}

fn recv(client: &Connection) -> Message {
    client
        .receiver
        .recv_timeout(Duration::from_secs(20))
        .expect("timed out waiting for a server message")
}

/// The response to request `id`, skipping notifications.
fn response(client: &Connection, id: i32) -> Response {
    loop {
        if let Message::Response(r) = recv(client) {
            if r.id == RequestId::from(id) {
                return r;
            }
        }
    }
}

fn next_diagnostics(client: &Connection) -> Vec<serde_json::Value> {
    loop {
        if let Message::Notification(n) = recv(client) {
            if n.method == "textDocument/publishDiagnostics" {
                return n.params["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
        }
    }
}

fn start() -> (Connection, thread::JoinHandle<std::io::Result<()>>) {
    let (server, client) = Connection::memory();
    let handle = thread::spawn(move || {
        kara_lsp::serve(server).map_err(|e| std::io::Error::other(format!("{e:?}")))
    });
    req(&client, 1, "initialize", json!({"capabilities":{}}));
    response(&client, 1);
    notify(&client, "initialized", json!({}));
    (client, handle)
}

fn stop(client: Connection, handle: thread::JoinHandle<std::io::Result<()>>) {
    req(&client, 999, "shutdown", serde_json::Value::Null);
    response(&client, 999);
    notify(&client, "exit", serde_json::Value::Null);
    drop(client);
    assert!(handle.join().unwrap().is_ok());
}

#[test]
fn lsp_contract_unserved_request_is_method_not_found() {
    let (client, handle) = start();
    for (id, method) in [
        (2, "textDocument/typeDefinition"),
        (3, "textDocument/implementation"),
        (4, "workspace/symbol"),
        (5, "kara/noSuchThing"),
    ] {
        req(
            &client,
            id,
            method,
            json!({"textDocument":{"uri":"file:///m.kara"},"position":{"line":0,"character":0},"query":""}),
        );
        let r = response(&client, id);
        let err = r.error.unwrap_or_else(|| panic!("{method}: no error"));
        assert_eq!(
            err.code,
            lsp_server::ErrorCode::MethodNotFound as i32,
            "{method}: {err:?}"
        );
    }
    stop(client, handle);
}

#[test]
fn lsp_contract_render_crash_report_takes_the_run_record() {
    let (client, handle) = start();
    // What `karac run --output=json` prints for an `unwrap` on `None`, and
    // the JSONL event `--output=jsonl` prints for it.
    let record = r#"{"runtime_errors":[{"message":"called unwrap() on None","location":{"file":"p1.kara","line":3,"col":16},"kind":"unwrap"}]}"#;
    let event = r#"{"type":"runtime_error","message":"called unwrap() on None","location":{"file":"p1.kara","line":3,"col":16},"kind":"unwrap"}"#;
    for (id, text) in [(2, record), (3, event)] {
        req(
            &client,
            id,
            "kara/renderCrashReport",
            json!({ "text": text }),
        );
        let r = response(&client, id);
        let rendered = r.result.expect("a result")["rendered"]
            .as_str()
            .expect("rendered text")
            .to_string();
        assert!(rendered.contains("panic: unwrap"), "{rendered}");
        assert!(rendered.contains("p1.kara:3:16"), "{rendered}");
        assert!(rendered.contains("called unwrap() on None"), "{rendered}");
        assert!(!rendered.contains('\u{1b}'), "no ANSI: {rendered}");
    }
    // Not a record: InvalidParams, not a crash.
    req(
        &client,
        4,
        "kara/renderCrashReport",
        json!({ "text": "{nope" }),
    );
    let r = response(&client, 4);
    assert_eq!(
        r.error.expect("an error").code,
        lsp_server::ErrorCode::InvalidParams as i32
    );
    stop(client, handle);
}

#[test]
fn lsp_contract_did_save_checks_and_publishes() {
    let (client, handle) = start();
    let uri = "file:///s.kara";
    notify(
        &client,
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"kara","version":1,"text":"fn main() { let x: i32 = \"no\"; }"}}),
    );
    assert_eq!(next_diagnostics(&client).len(), 1);
    notify(
        &client,
        "textDocument/didSave",
        json!({"textDocument":{"uri":uri},"text":"fn main() { let x: i32 = \"no\"; }"}),
    );
    let diags = next_diagnostics(&client);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0]["code"], json!("E0200"));
    stop(client, handle);
}

#[test]
fn lsp_contract_positions_are_utf16() {
    let (client, handle) = start();
    let uri = "file:///u.kara";
    // Each emoji is 4 bytes, 2 UTF-16 code units, 1 char.
    let text = "fn main() { let s = \"😀😀\"; let x: i32 = \"no\"; }";
    notify(
        &client,
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"kara","version":1,"text":text}}),
    );
    let diags = next_diagnostics(&client);
    assert_eq!(diags.len(), 1, "{diags:?}");
    let byte = text.rfind("\"no\"").unwrap();
    let utf16 = text[..byte].encode_utf16().count() as u64;
    assert_ne!(utf16, byte as u64);
    assert_eq!(
        diags[0]["range"]["start"]["character"],
        json!(utf16),
        "{diags:?}"
    );
    stop(client, handle);
}
