//! `kara-lsp` — the Kāra language server (roadmap Track 3).
//!
//! The binary (`src/main.rs`) is a thin wrapper that wires stdio to
//! [`serve`]; all the protocol logic lives here so it can be driven in-process
//! over an in-memory connection ([`lsp_server::Connection::memory`]) by the
//! integration tests. Features so far: the `initialize`/`shutdown` handshake,
//! live diagnostics (`textDocument/publishDiagnostics`, slice 1), hover showing
//! type + effect signature (`textDocument/hover`, slices 2 & 6),
//! go-to-definition + document outline (`textDocument/definition` +
//! `textDocument/documentSymbol`, slice 3), whole-document formatting
//! (`textDocument/formatting`, slice 4), and find-references
//! (`textDocument/references`, slice 5) — each a thin [`analysis`] call over a
//! `karac` library query. No user code is ever executed — feedback is purely
//! static.

pub mod analysis;

use std::collections::HashMap;
use std::error::Error;

use lsp_server::{Connection, Message};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest, References, Request as _,
};
use lsp_types::{
    DocumentSymbolResponse, GotoDefinitionResponse, HoverProviderCapability, InitializeParams,
    Location, OneOf, PublishDiagnosticsParams, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, Uri,
};

type LspResult = Result<(), Box<dyn Error + Sync + Send>>;

/// Perform the `initialize` handshake on `connection`, then run the message
/// pump until the client shuts the server down. Consumes the connection so it
/// is dropped on return (closing the writer channel — see [`main_loop`]).
pub fn serve(connection: Connection) -> LspResult {
    // Full-document sync (so every edit hands us the whole buffer to
    // re-analyze) drives the diagnostics loop; hover is served from the same
    // per-document text. Further capabilities (definition, completion) are
    // turned on as their slices land.
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        ..Default::default()
    };
    let init_params = connection.initialize(serde_json::to_value(capabilities)?)?;
    let _init_params: InitializeParams = serde_json::from_value(init_params)?;
    main_loop(connection)
}

/// The message pump. Runs until the client sends `shutdown`, then returns —
/// dropping `connection` (taken by value on purpose: holding it across the
/// caller's `io_threads.join()` would deadlock the writer thread, which only
/// exits once its `sender` is dropped).
fn main_loop(connection: Connection) -> LspResult {
    // Latest known text per open document, keyed by the URI's string form
    // (`Uri` itself carries an internal hash-cache cell, so it is a poor map
    // key — `clippy::mutable_key_type`). FULL sync means open/change carry the
    // whole buffer; we keep it so `save` (which need not carry text) and future
    // position-based requests can re-analyze the current revision.
    let mut docs: HashMap<String, String> = HashMap::new();

    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                // `handle_shutdown` answers the shutdown request and returns
                // true; the loop then ends and the connection drops.
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                let resp = handle_request(&docs, req);
                connection.sender.send(Message::Response(resp))?;
            }
            Message::Notification(not) => {
                handle_notification(&connection, &mut docs, not)?;
            }
            Message::Response(_) => {
                // We issue no server→client requests in slice 1, so there are
                // no responses to correlate.
            }
        }
    }
    Ok(())
}

fn handle_notification(
    connection: &Connection,
    docs: &mut HashMap<String, String>,
    not: lsp_server::Notification,
) -> LspResult {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams = serde_json::from_value(not.params)?;
            let uri = params.text_document.uri;
            let text = params.text_document.text;
            publish(connection, &uri, &text);
            docs.insert(uri.to_string(), text);
        }
        DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(not.params)?;
            let uri = params.text_document.uri;
            // FULL sync: the last content change holds the entire new buffer.
            if let Some(change) = params.content_changes.into_iter().next_back() {
                let text = change.text;
                publish(connection, &uri, &text);
                docs.insert(uri.to_string(), text);
            }
        }
        DidSaveTextDocument::METHOD => {
            let params: lsp_types::DidSaveTextDocumentParams = serde_json::from_value(not.params)?;
            let uri = params.text_document.uri;
            // Prefer the text carried on save if present; otherwise re-analyze
            // the last-known revision.
            if let Some(text) = params.text.or_else(|| docs.get(&uri.to_string()).cloned()) {
                publish(connection, &uri, &text);
            }
        }
        DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams = serde_json::from_value(not.params)?;
            let uri = params.text_document.uri;
            docs.remove(&uri.to_string());
            // Clear the editor's squiggles for a closed document.
            publish_diagnostics(connection, uri, Vec::new());
        }
        _ => {}
    }
    Ok(())
}

// JSON-RPC error codes (LSP inherits these from JSON-RPC 2.0).
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;

/// Answer a client→server request. Every request must get exactly one
/// response, so unhandled methods return a `MethodNotFound` error rather than
/// silently dropping (which would hang the client).
fn handle_request(
    docs: &HashMap<String, String>,
    req: lsp_server::Request,
) -> lsp_server::Response {
    let lsp_server::Request { id, method, params } = req;
    match method.as_str() {
        HoverRequest::METHOD => match serde_json::from_value::<lsp_types::HoverParams>(params) {
            Ok(p) => {
                let pos = p.text_document_position_params.position;
                let uri = p
                    .text_document_position_params
                    .text_document
                    .uri
                    .to_string();
                // Hover the last-known revision of the document. `None` (no
                // typed expression under the cursor) serializes to a null
                // result, which the LSP spec accepts as "no hover".
                let hover = docs.get(&uri).and_then(|text| analysis::hover(text, pos));
                lsp_server::Response::new_ok(id, hover)
            }
            Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
        },
        GotoDefinition::METHOD => {
            match serde_json::from_value::<lsp_types::GotoDefinitionParams>(params) {
                Ok(p) => {
                    let pos = p.text_document_position_params.position;
                    let uri = p.text_document_position_params.text_document.uri;
                    let resp = docs.get(&uri.to_string()).and_then(|text| {
                        analysis::definition(text, pos).map(|range| {
                            GotoDefinitionResponse::Scalar(Location {
                                uri: uri.clone(),
                                range,
                            })
                        })
                    });
                    lsp_server::Response::new_ok(id, resp)
                }
                Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
            }
        }
        References::METHOD => match serde_json::from_value::<lsp_types::ReferenceParams>(params) {
            Ok(p) => {
                let pos = p.text_document_position.position;
                let uri = p.text_document_position.text_document.uri;
                let include_decl = p.context.include_declaration;
                let locs: Vec<Location> = docs
                    .get(&uri.to_string())
                    .map(|text| {
                        analysis::references(text, pos, include_decl)
                            .into_iter()
                            .map(|range| Location {
                                uri: uri.clone(),
                                range,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                lsp_server::Response::new_ok(id, locs)
            }
            Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
        },
        DocumentSymbolRequest::METHOD => {
            match serde_json::from_value::<lsp_types::DocumentSymbolParams>(params) {
                Ok(p) => {
                    let uri = p.text_document.uri.to_string();
                    let resp = docs.get(&uri).map(|text| {
                        DocumentSymbolResponse::Nested(analysis::document_symbols(text))
                    });
                    lsp_server::Response::new_ok(id, resp)
                }
                Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
            }
        }
        Formatting::METHOD => {
            match serde_json::from_value::<lsp_types::DocumentFormattingParams>(params) {
                Ok(p) => {
                    let uri = p.text_document.uri.to_string();
                    // `None` (parse error / unknown doc) → null response, which
                    // the client reads as "no formatting available".
                    let edits = docs.get(&uri).and_then(|text| analysis::formatting(text));
                    lsp_server::Response::new_ok(id, edits)
                }
                Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
            }
        }
        // Custom request: render a std.panic crash report to human-readable
        // text. Params: `{ "text": "<crash-report JSON>" }`. Response:
        // `{ "rendered": "<text>" }`. The LSP-shaped counterpart of
        // `karac debug` — a "Kāra: Render Crash Report" editor command calls
        // this and shows the result in a panel. (No source document involved,
        // so it does not key off `docs`.)
        "kara/renderCrashReport" => {
            let text = params.get("text").and_then(serde_json::Value::as_str);
            match text {
                Some(json_text) => match analysis::render_crash_report(json_text) {
                    Ok(rendered) => lsp_server::Response::new_ok(
                        id,
                        serde_json::json!({ "rendered": rendered }),
                    ),
                    Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e),
                },
                None => lsp_server::Response::new_err(
                    id,
                    INVALID_PARAMS,
                    "kara/renderCrashReport requires a string `text` field".to_string(),
                ),
            }
        }
        _ => lsp_server::Response::new_err(
            id,
            METHOD_NOT_FOUND,
            format!("unhandled method: {method}"),
        ),
    }
}

/// Analyze `text` and publish the resulting diagnostics for `uri`.
fn publish(connection: &Connection, uri: &Uri, text: &str) {
    let diags = analysis::diagnostics(text);
    publish_diagnostics(connection, uri.clone(), diags);
}

fn publish_diagnostics(connection: &Connection, uri: Uri, diagnostics: Vec<lsp_types::Diagnostic>) {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    let not = lsp_server::Notification {
        method: PublishDiagnostics::METHOD.to_string(),
        params: serde_json::to_value(params).expect("PublishDiagnosticsParams serializes"),
    };
    // A send failure means the client is gone; the loop ends on the next
    // closed-receiver iteration, so dropping the error here is fine.
    let _ = connection.sender.send(Message::Notification(not));
}
