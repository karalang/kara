//! B-2026-11-21-36: after `shutdown` the server stopped reading, so a
//! request sent before `exit` was never answered, and `exit` without
//! `shutdown` ended with status 0. LSP 3.17: requests after `shutdown`
//! error with `InvalidRequest`, and `exit` exits 0 after `shutdown`, 1
//! without it.

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
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

fn response(client: &Connection, id: i32) -> lsp_server::Response {
    loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
        {
            Message::Response(r) if r.id == RequestId::from(id) => return r,
            _ => continue,
        }
    }
}

fn start() -> (Connection, thread::JoinHandle<i32>) {
    let (server, client) = Connection::memory();
    let t = thread::spawn(move || kara_lsp::serve_until_exit(server).unwrap());
    req(
        &client,
        1,
        "initialize",
        json!({"capabilities": {}, "processId": null}),
    );
    let _ = response(&client, 1);
    notify(&client, "initialized", json!({}));
    (client, t)
}

#[test]
fn conformance_requests_after_shutdown_are_invalid_and_exit_is_zero() {
    let (client, t) = start();
    req(&client, 2, "shutdown", json!(null));
    assert!(response(&client, 2).error.is_none());
    req(
        &client,
        3,
        "textDocument/hover",
        json!({"textDocument": {"uri": "file:///x.kara"}, "position": {"line": 0, "character": 0}}),
    );
    let r = response(&client, 3);
    assert_eq!(r.error.map(|e| e.code), Some(-32600), "InvalidRequest");
    notify(&client, "exit", json!(null));
    assert_eq!(t.join().unwrap(), 0);
}

#[test]
fn conformance_exit_without_shutdown_is_one() {
    let (client, t) = start();
    notify(&client, "exit", json!(null));
    assert_eq!(t.join().unwrap(), 1);
}
