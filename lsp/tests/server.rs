//! End-to-end tests of the `kara-lsp` server loop over an in-memory
//! connection (`lsp_server::Connection::memory`) — no real process / stdio.
//! Drives the full protocol the way an editor would: initialize handshake →
//! open/change documents → assert on the published diagnostics → shutdown.
//! Pins both the diagnostics wiring and the clean-shutdown path (the loop must
//! not deadlock on `shutdown` — the reason `serve` takes the connection by
//! value).

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::json;

/// Send a request from the client end.
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

/// Send a notification from the client end.
fn notify(client: &Connection, method: &str, params: serde_json::Value) {
    client
        .sender
        .send(Message::Notification(Notification {
            method: method.to_string(),
            params,
        }))
        .unwrap();
}

/// Block for the next message on the client end (with a timeout so a hang
/// fails the test instead of wedging CI).
fn recv(client: &Connection) -> Message {
    client
        .receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("timed out waiting for a server message")
}

/// Read messages until a `textDocument/publishDiagnostics` arrives; return its
/// `diagnostics` array. Skips any interleaved server messages.
fn next_diagnostics(client: &Connection) -> Vec<serde_json::Value> {
    loop {
        match recv(client) {
            Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                return n.params["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
            _ => continue,
        }
    }
}

fn did_open(client: &Connection, uri: &str, text: &str) {
    notify(
        client,
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"kara","version":1,"text":text}}),
    );
}

fn did_change_full(client: &Connection, uri: &str, version: i32, text: &str) {
    notify(
        client,
        "textDocument/didChange",
        json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":text}]}),
    );
}

#[test]
fn server_publishes_diagnostics_and_shuts_down_cleanly() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(move || kara_lsp::serve(server));

    // 1. initialize handshake.
    req(
        &client,
        1,
        "initialize",
        json!({"capabilities":{}, "processId":null}),
    );
    let init = recv(&client);
    let Message::Response(Response {
        result: Some(caps), ..
    }) = init
    else {
        panic!("expected initialize response, got {init:?}");
    };
    // FULL text sync == numeric kind 1.
    assert_eq!(caps["capabilities"]["textDocumentSync"], json!(1));
    notify(&client, "initialized", json!({}));

    // 2. open a document with a type error → one diagnostic on line 0.
    let uri = "file:///t.kara";
    did_open(&client, uri, "fn main() { let x: i32 = \"nope\"; }");
    let diags = next_diagnostics(&client);
    assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    assert_eq!(diags[0]["code"], json!("typecheck"));
    assert_eq!(diags[0]["source"], json!("kara"));
    assert_eq!(diags[0]["severity"], json!(1)); // ERROR
    assert_eq!(diags[0]["range"]["start"]["line"], json!(0));
    assert!(
        diags[0]["range"]["end"]["character"].as_u64().unwrap()
            > diags[0]["range"]["start"]["character"].as_u64().unwrap(),
        "range must be non-empty: {:?}",
        diags[0]["range"]
    );

    // 3. edit to a clean buffer → diagnostics clear to empty.
    did_change_full(&client, uri, 2, "fn f(a: i64) -> i64 { a }");
    assert!(
        next_diagnostics(&client).is_empty(),
        "clean buffer must clear diagnostics"
    );

    // 4. hover over the `a` in the body (line 0, char 22) → `i64`.
    req(
        &client,
        3,
        "textDocument/hover",
        json!({"textDocument":{"uri":uri},"position":{"line":0,"character":22}}),
    );
    let hv = recv(&client);
    let Message::Response(Response {
        result: Some(hover),
        ..
    }) = hv
    else {
        panic!("expected hover response, got {hv:?}");
    };
    assert_eq!(hover["contents"]["kind"], json!("markdown"));
    assert_eq!(hover["contents"]["value"], json!("```kara\ni64\n```"));

    // 5. hover where nothing typed sits (the `fn` keyword) → null result.
    req(
        &client,
        4,
        "textDocument/hover",
        json!({"textDocument":{"uri":uri},"position":{"line":0,"character":0}}),
    );
    let hv2 = recv(&client);
    assert!(
        matches!(&hv2, Message::Response(Response { result, error, .. })
            if result.as_ref().map(|v| v.is_null()).unwrap_or(true) && error.is_none()),
        "expected null hover result, got {hv2:?}"
    );

    // 6. shutdown / exit → the server thread returns Ok and the loop does not
    //    hang.
    req(&client, 2, "shutdown", serde_json::Value::Null);
    let sd = recv(&client);
    assert!(
        matches!(sd, Message::Response(Response { ref id, .. }) if *id == RequestId::from(2)),
        "expected shutdown response, got {sd:?}"
    );
    notify(&client, "exit", serde_json::Value::Null);
    drop(client); // reader EOF, as a real editor closing the pipe

    let joined = server_thread.join().expect("server thread panicked");
    assert!(joined.is_ok(), "serve returned an error: {joined:?}");
}

#[test]
fn server_clears_diagnostics_on_close() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(move || kara_lsp::serve(server));

    req(&client, 1, "initialize", json!({"capabilities":{}}));
    recv(&client);
    notify(&client, "initialized", json!({}));

    let uri = "file:///c.kara";
    did_open(&client, uri, "fn main() { let _ = undefined_name(); }");
    assert!(!next_diagnostics(&client).is_empty());

    // didClose must publish an empty diagnostic set to wipe the squiggles.
    notify(
        &client,
        "textDocument/didClose",
        json!({"textDocument":{"uri":uri}}),
    );
    assert!(next_diagnostics(&client).is_empty());

    req(&client, 2, "shutdown", serde_json::Value::Null);
    recv(&client);
    notify(&client, "exit", serde_json::Value::Null);
    drop(client);
    assert!(server_thread.join().unwrap().is_ok());
}
