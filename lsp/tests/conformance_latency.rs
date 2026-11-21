//! B-2026-11-21-90: every keystroke was checked as it came, and a request
//! queued behind a check waited for it and was answered even when the
//! client had cancelled it. Edits are now checked once a burst of them ends,
//! and a request cancelled before it is read is answered
//! `RequestCancelled` (-32800) unrun.

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::json;
use std::time::Instant;

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

const URI: &str = "file:///latency_probe.kara";

fn open(client: &Connection, text: &str) {
    notify(
        client,
        "textDocument/didOpen",
        json!({"textDocument": {"uri": URI, "languageId": "kara", "version": 1, "text": text}}),
    );
}

/// The diagnostics published for [`URI`] within `wait`.
fn published(client: &Connection, wait: Duration) -> Vec<serde_json::Value> {
    let end = Instant::now() + wait;
    let mut out = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match client.receiver.recv_timeout(left) {
            Ok(Message::Notification(n))
                if n.method == "textDocument/publishDiagnostics" && n.params["uri"] == URI =>
            {
                out.push(n.params["diagnostics"].clone())
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}

#[test]
fn latency_a_burst_of_edits_is_checked_once_at_its_end() {
    let (client, t) = start();
    open(&client, "fn main() {\n}\n");
    assert_eq!(published(&client, Duration::from_secs(5)).len(), 1);
    // Five keystrokes, the last of which leaves an unknown name.
    for (k, ch) in ["l", "e", "t", " ", "q"].iter().enumerate() {
        notify(
            &client,
            "textDocument/didChange",
            json!({"textDocument": {"uri": URI, "version": 2 + k},
                   "contentChanges": [{"range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0}}, "text": ch}]}),
        );
    }
    let got = published(&client, Duration::from_secs(5));
    assert_eq!(got.len(), 1, "one check for the burst: {got:?}");
    assert!(
        !got[0].as_array().unwrap().is_empty(),
        "the burst's end, `let q` with no `=`, is an error: {got:?}"
    );
    req(&client, 9, "shutdown", json!(null));
    let _ = response(&client, 9);
    notify(&client, "exit", json!(null));
    assert_eq!(t.join().unwrap(), 0);
}

#[test]
fn latency_a_request_cancelled_before_it_is_read_is_request_cancelled() {
    let (client, t) = start();
    // A check long enough that the requests behind it are queued: the
    // library's serde surface is checked with the program.
    open(
        &client,
        "#[derive(Serialize)]\nstruct P {\n    x: i64,\n}\nfn main() {\n    let p = P { x: 1 };\n}\n",
    );
    let at = json!({"textDocument": {"uri": URI}, "position": {"line": 5, "character": 9}});
    req(&client, 2, "textDocument/hover", at.clone());
    notify(&client, "$/cancelRequest", json!({"id": 2}));
    req(&client, 3, "textDocument/hover", at);
    let cancelled = response(&client, 2);
    assert_eq!(cancelled.error.map(|e| e.code), Some(-32800));
    // The one not cancelled is answered.
    let answered = response(&client, 3);
    assert!(answered.error.is_none(), "{answered:?}");
    req(&client, 9, "shutdown", json!(null));
    let _ = response(&client, 9);
    notify(&client, "exit", json!(null));
    assert_eq!(t.join().unwrap(), 0);
}
