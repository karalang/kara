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
//! (`textDocument/references`, slice 5), completion (`textDocument/completion`)
//! and rename (`textDocument/rename`) — each a thin [`analysis`] call over a
//! `karac` library query. No user code is ever executed — feedback is purely
//! static.

pub mod analysis;
pub mod workspace;

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;

use lsp_server::{Connection, Message};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
    InlayHintRequest, References, Rename, Request as _, SignatureHelpRequest,
};
use lsp_types::{
    CodeActionOptions, CodeActionProviderCapability, CompletionOptions, DocumentSymbolResponse,
    GotoDefinitionResponse, HoverProviderCapability, InitializeParams, Location, OneOf,
    PublishDiagnosticsParams, ServerCapabilities, SignatureHelpOptions, TextDocumentSyncCapability,
    TextDocumentSyncKind, Uri,
};

type LspResult = Result<(), Box<dyn Error + Sync + Send>>;

/// Perform the `initialize` handshake on `connection`, then run the message
/// pump until the client shuts the server down. Consumes the connection so it
/// is dropped on return (closing the writer channel — see [`main_loop`]).
pub fn serve(connection: Connection) -> LspResult {
    serve_until_exit(connection).map(|_| ())
}

/// [`serve`], returning the process's exit code once the client sends
/// `exit`: 0 after a `shutdown`, 1 without one (LSP 3.17, `exit`).
pub fn serve_until_exit(connection: Connection) -> Result<i32, Box<dyn Error + Sync + Send>> {
    // Full-document sync (so every edit hands us the whole buffer to
    // re-analyze) drives the diagnostics loop; hover is served from the same
    // per-document text.
    let capabilities = ServerCapabilities {
        // Range edits (B-2026-11-21-33): a client sends what a
        // keystroke changed, not the whole buffer, which on a large file is
        // most of the traffic; [`apply_change`] splices it in.
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        // `.` asks for the receiver's members as it is typed; anywhere else
        // the client asks on its own (identifier characters, or Ctrl-Space).
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..Default::default()
        }),
        // Over the same resolver references `textDocument/references` uses.
        rename_provider: Some(OneOf::Left(true)),
        // A call's signature as its arguments are typed: `(` opens it, `,`
        // moves to the next parameter.
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
        // Inferred `let` types and private functions' inferred effects, read
        // off each document's last check.
        inlay_hint_provider: Some(OneOf::Left(true)),
        // Quick fixes. The machine-applicable edits `karac fix` applies from
        // the CLI had no editor path at all — the compiler computed them and
        // nothing but the terminal could act on them. Declaring the kind (not
        // just `true`) lets a client pre-filter and drive "fix all" without a
        // round trip.
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![
                lsp_types::CodeActionKind::QUICKFIX,
                lsp_types::CodeActionKind::REFACTOR_REWRITE,
            ]),
            ..Default::default()
        })),
        ..Default::default()
    };
    let init_params = connection.initialize(serde_json::to_value(capabilities)?)?;
    let _init_params: InitializeParams = serde_json::from_value(init_params)?;
    main_loop(connection)
}

/// The message pump. Runs until the client sends `exit`, then returns its
/// exit code — dropping `connection` (taken by value on purpose: holding it
/// across the caller's `io_threads.join()` would deadlock the writer thread,
/// which only exits once its `sender` is dropped). After `shutdown` every
/// request is answered `InvalidRequest` (B-2026-11-21-36): the server had
/// stopped reading, so a request sent then was never answered.
fn main_loop(connection: Connection) -> Result<i32, Box<dyn Error + Sync + Send>> {
    // Latest known text per open document, keyed by the URI's string form
    // (`Uri` itself carries an internal hash-cache cell, so it is a poor map
    // key — `clippy::mutable_key_type`).
    let mut docs: HashMap<String, String> = HashMap::new();
    // Each open document's last check, so an edit inside one function body
    // re-checks that body (B-2026-11-09-33).
    let mut checkers: HashMap<String, karac::DocumentChecker> = HashMap::new();
    // Documents edited since their diagnostics were published, and when the
    // check is due (B-2026-11-21-90): an edit waits [`debounce`] for the next
    // keystroke, so a burst of typing is checked once, at its end, and a
    // request that arrives meanwhile is answered first.
    let mut pending: HashMap<String, Uri> = HashMap::new();
    let mut due: Option<Instant> = None;
    // Messages read ahead of the one being handled, so a request a later
    // `$/cancelRequest` cancels is answered `RequestCancelled` unrun.
    let mut queue: VecDeque<Message> = VecDeque::new();

    let mut shut_down = false;
    loop {
        while let Ok(m) = connection.receiver.try_recv() {
            queue.push_back(m);
        }
        let msg = match queue.pop_front() {
            Some(m) => m,
            None => {
                let next = match due {
                    Some(at) => connection
                        .receiver
                        .recv_timeout(at.saturating_duration_since(Instant::now())),
                    None => connection
                        .receiver
                        .recv()
                        .map_err(|_| RecvTimeoutError::Disconnected),
                };
                match next {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => {
                        flush(&connection, &docs, &mut checkers, &mut pending, None);
                        due = None;
                        continue;
                    }
                    // The client went away without `exit`.
                    Err(RecvTimeoutError::Disconnected) => return Ok(1),
                }
            }
        };
        match msg {
            Message::Request(req) => {
                let resp = if shut_down {
                    lsp_server::Response::new_err(
                        req.id,
                        INVALID_REQUEST,
                        "the server is shutting down".to_string(),
                    )
                } else if req.method == "shutdown" {
                    shut_down = true;
                    lsp_server::Response::new_ok(req.id, ())
                } else if cancelled(&queue, &req.id) {
                    lsp_server::Response::new_err(
                        req.id,
                        REQUEST_CANCELLED,
                        "cancelled".to_string(),
                    )
                } else {
                    // What reads the document's last check sees this
                    // revision's.
                    if matches!(
                        req.method.as_str(),
                        InlayHintRequest::METHOD | CodeActionRequest::METHOD
                    ) {
                        let uri = req
                            .params
                            .pointer("/textDocument/uri")
                            .and_then(|u| u.as_str())
                            .map(str::to_string);
                        if let Some(uri) = uri {
                            flush(&connection, &docs, &mut checkers, &mut pending, Some(&uri));
                        }
                    }
                    handle_request(&docs, &checkers, req)
                };
                connection.sender.send(Message::Response(resp))?;
            }
            Message::Notification(not) if not.method == "exit" => {
                return Ok(if shut_down { 0 } else { 1 });
            }
            Message::Notification(_) if shut_down => {}
            // Answered (or dropped) where its request is read.
            Message::Notification(not) if not.method == "$/cancelRequest" => {}
            Message::Notification(not) => {
                // Whatever a position query kept from the text before is
                // stale now.
                karac::forget_checked_buffers();
                if not.method == DidChangeTextDocument::METHOD {
                    let params: lsp_types::DidChangeTextDocumentParams =
                        serde_json::from_value(not.params)?;
                    let uri = params.text_document.uri;
                    // Each change applies to the text the one before it left;
                    // one without a range is the whole buffer.
                    if !params.content_changes.is_empty() {
                        let text = docs.entry(uri.to_string()).or_default();
                        for change in params.content_changes {
                            apply_change(text, change);
                        }
                        pending.insert(uri.to_string(), uri);
                        due = Some(Instant::now() + debounce());
                    }
                } else {
                    if let Some(uri) = not
                        .params
                        .pointer("/textDocument/uri")
                        .and_then(|u| u.as_str())
                    {
                        // Opening, saving or closing checks (or clears) the
                        // document now.
                        pending.remove(uri);
                    }
                    handle_notification(&connection, &mut docs, &mut checkers, not)?;
                }
            }
            Message::Response(_) => {
                // We issue no server→client requests, so there are no
                // responses to correlate.
            }
        }
    }
}

/// `$/cancelRequest`'s `RequestCancelled` (LSP 3.17).
const REQUEST_CANCELLED: i32 = -32800;

/// How long an edited document waits for the next edit before it is
/// checked: `KARA_LSP_DEBOUNCE_MS`, else 100 ms.
fn debounce() -> Duration {
    static MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    Duration::from_millis(*MS.get_or_init(|| {
        std::env::var("KARA_LSP_DEBOUNCE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100)
    }))
}

/// Whether a `$/cancelRequest` read ahead names `id`.
fn cancelled(queue: &VecDeque<Message>, id: &lsp_server::RequestId) -> bool {
    queue.iter().any(|m| match m {
        Message::Notification(n) if n.method == "$/cancelRequest" => match n.params.get("id") {
            Some(serde_json::Value::Number(k)) => {
                k.as_i64().map(|k| lsp_server::RequestId::from(k as i32)) == Some(id.clone())
            }
            Some(serde_json::Value::String(k)) => lsp_server::RequestId::from(k.clone()) == *id,
            _ => false,
        },
        _ => false,
    })
}

/// Check and publish the pending documents (only `only`, when given).
fn flush(
    connection: &Connection,
    docs: &HashMap<String, String>,
    checkers: &mut HashMap<String, karac::DocumentChecker>,
    pending: &mut HashMap<String, Uri>,
    only: Option<&str>,
) {
    let keys: Vec<String> = match only {
        Some(k) if pending.contains_key(k) => vec![k.to_string()],
        Some(_) => Vec::new(),
        None => pending.keys().cloned().collect(),
    };
    for key in keys {
        let Some(uri) = pending.remove(&key) else {
            continue;
        };
        if let Some(text) = docs.get(&key) {
            let checker = checkers.entry(key.clone()).or_default();
            publish(connection, checker, &uri, text, false);
        }
    }
}

fn handle_notification(
    connection: &Connection,
    docs: &mut HashMap<String, String>,
    checkers: &mut HashMap<String, karac::DocumentChecker>,
    not: lsp_server::Notification,
) -> LspResult {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams = serde_json::from_value(not.params)?;
            let uri = params.text_document.uri;
            let text = params.text_document.text;
            let checker = checkers.entry(uri.to_string()).or_default();
            publish(connection, checker, &uri, &text, true);
            docs.insert(uri.to_string(), text);
        }
        DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(not.params)?;
            let uri = params.text_document.uri;
            // Each change applies to the text the one before it left; one
            // without a range is the whole buffer.
            if !params.content_changes.is_empty() {
                let mut text = docs.get(&uri.to_string()).cloned().unwrap_or_default();
                for change in params.content_changes {
                    apply_change(&mut text, change);
                }
                let checker = checkers.entry(uri.to_string()).or_default();
                publish(connection, checker, &uri, &text, false);
                docs.insert(uri.to_string(), text);
            }
        }
        DidSaveTextDocument::METHOD => {
            let params: lsp_types::DidSaveTextDocumentParams = serde_json::from_value(not.params)?;
            let uri = params.text_document.uri;
            // Prefer the text carried on save if present; otherwise re-analyze
            // the last-known revision.
            // A save checks the whole buffer.
            if let Some(text) = params.text.or_else(|| docs.get(&uri.to_string()).cloned()) {
                let checker = checkers.entry(uri.to_string()).or_default();
                publish(connection, checker, &uri, &text, true);
            }
        }
        DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams = serde_json::from_value(not.params)?;
            let uri = params.text_document.uri;
            docs.remove(&uri.to_string());
            checkers.remove(&uri.to_string());
            // Clear the editor's squiggles for a closed document.
            publish_diagnostics(connection, uri, Vec::new());
        }
        _ => {}
    }
    Ok(())
}

/// Splice one `didChange` content change into `text`: its range, in
/// UTF-16 positions, replaced by its text, or the whole buffer when it has
/// no range. A position past a line's end or the buffer's clamps to it.
pub fn apply_change(text: &mut String, change: lsp_types::TextDocumentContentChangeEvent) {
    match change.range {
        None => *text = change.text,
        Some(range) => {
            let index = analysis::LineIndex::new(text);
            let start = index.offset(range.start);
            let end = index.offset(range.end).max(start);
            text.replace_range(start..end, &change.text);
        }
    }
}

// JSON-RPC error codes (LSP inherits these from JSON-RPC 2.0).
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_REQUEST: i32 = -32600;
const INVALID_PARAMS: i32 = -32602;

/// Answer a client→server request. Every request must get exactly one
/// response, so unhandled methods return a `MethodNotFound` error rather than
/// silently dropping (which would hang the client).
fn handle_request(
    docs: &HashMap<String, String>,
    checkers: &HashMap<String, karac::DocumentChecker>,
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
                let path = p.text_document_position_params.text_document.uri.clone();
                let path = analysis::file_path(&path);
                let hover = docs
                    .get(&uri)
                    .and_then(|text| analysis::hover_in(text, path.as_deref(), pos));
                lsp_server::Response::new_ok(id, hover)
            }
            Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
        },
        GotoDefinition::METHOD => {
            match serde_json::from_value::<lsp_types::GotoDefinitionParams>(params) {
                Ok(p) => {
                    let pos = p.text_document_position_params.position;
                    let uri = p.text_document_position_params.text_document.uri;
                    let path = analysis::file_path(&uri);
                    let resp = docs.get(&uri.to_string()).and_then(|text| {
                        analysis::definition_site_in(text, path.as_deref(), pos).map(
                            |(at, range)| {
                                GotoDefinitionResponse::Scalar(Location {
                                    uri: at.unwrap_or_else(|| uri.clone()),
                                    range,
                                })
                            },
                        )
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
                // A `pub` item of a package module: its references in every
                // module of the package.
                let across = analysis::file_path(&uri).and_then(|path| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        workspace::references(&path, docs, pos, include_decl)
                    }))
                    .ok()
                    .flatten()
                });
                let locs: Vec<Location> = across.unwrap_or_else(|| {
                    docs.get(&uri.to_string())
                        .map(|text| {
                            analysis::references(text, pos, include_decl)
                                .into_iter()
                                .map(|range| Location {
                                    uri: uri.clone(),
                                    range,
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                });
                lsp_server::Response::new_ok(id, locs)
            }
            Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
        },
        Completion::METHOD => match serde_json::from_value::<lsp_types::CompletionParams>(params) {
            Ok(p) => {
                let pos = p.text_document_position.position;
                let path = analysis::file_path(&p.text_document_position.text_document.uri);
                let uri = p.text_document_position.text_document.uri.to_string();
                let items = docs
                    .get(&uri)
                    .map(|text| analysis::completion_in(text, path.as_deref(), pos))
                    .unwrap_or_default();
                lsp_server::Response::new_ok(id, lsp_types::CompletionResponse::Array(items))
            }
            Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
        },
        SignatureHelpRequest::METHOD => {
            match serde_json::from_value::<lsp_types::SignatureHelpParams>(params) {
                Ok(p) => {
                    let pos = p.text_document_position_params.position;
                    let uri = p
                        .text_document_position_params
                        .text_document
                        .uri
                        .to_string();
                    let path =
                        analysis::file_path(&p.text_document_position_params.text_document.uri);
                    // `None` (not inside a call) → null: no signature to show.
                    let help = docs
                        .get(&uri)
                        .and_then(|text| analysis::signature_help_in(text, path.as_deref(), pos));
                    lsp_server::Response::new_ok(id, help)
                }
                Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
            }
        }
        InlayHintRequest::METHOD => {
            match serde_json::from_value::<lsp_types::InlayHintParams>(params) {
                Ok(p) => {
                    let uri = p.text_document.uri.to_string();
                    // The hints of the check the last edit already ran.
                    let hints = match (docs.get(&uri), checkers.get(&uri)) {
                        (Some(text), Some(c)) => {
                            analysis::inlay_hints(text, c.inlay_hints(), p.range)
                        }
                        _ => Vec::new(),
                    };
                    lsp_server::Response::new_ok(id, hints)
                }
                Err(e) => lsp_server::Response::new_err(id, INVALID_PARAMS, e.to_string()),
            }
        }
        Rename::METHOD => match serde_json::from_value::<lsp_types::RenameParams>(params) {
            Ok(p) => {
                let pos = p.text_document_position.position;
                let uri = p.text_document_position.text_document.uri;
                // `None` (not on a renameable name) → a null result, which
                // the client reports as "cannot rename here"; a new name
                // that is no name → an error that says why (B-2026-11-15-123).
                //
                // A `pub` item of a package module is renamed in every
                // module (B-2026-11-21-34), and no rename is
                // offered that would add an error.
                let path = analysis::file_path(&uri);
                let across = path.as_deref().map(|path| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        workspace::rename(path, docs, pos, &p.new_name)
                    }))
                    .unwrap_or(Ok(None))
                });
                #[allow(clippy::mutable_key_type)]
                let renamed = match across {
                    Some(Err(why)) => Err(why),
                    Some(Ok(Some(changes))) => Ok(Some(changes)),
                    _ => match docs.get(&uri.to_string()) {
                        Some(text) => {
                            analysis::rename(text, pos, &p.new_name).and_then(|edits| match edits {
                                Some(edits) => {
                                    workspace::checked_single_file(
                                        text,
                                        path.as_deref(),
                                        &edits,
                                        &p.new_name,
                                    )?;
                                    Ok(Some(std::iter::once((uri.clone(), edits)).collect()))
                                }
                                None => Ok(None),
                            })
                        }
                        None => Ok(None),
                    },
                };
                match renamed {
                    Ok(changes) => {
                        let edit = changes.map(|changes| lsp_types::WorkspaceEdit {
                            changes: Some(changes),
                            ..Default::default()
                        });
                        lsp_server::Response::new_ok(id, edit)
                    }
                    Err(why) => lsp_server::Response::new_err(id, INVALID_PARAMS, why),
                }
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
        CodeActionRequest::METHOD => {
            match serde_json::from_value::<lsp_types::CodeActionParams>(params) {
                Ok(p) => {
                    let key = p.text_document.uri.to_string();
                    // Keyed off `docs` like every other position request, so
                    // actions describe the revision the editor last sent —
                    // never a stale on-disk copy whose offsets have moved.
                    let actions = docs
                        .get(&key)
                        .map(|text| analysis::code_actions(text, &p.text_document.uri, p.range));
                    lsp_server::Response::new_ok(id, actions)
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

/// Analyze `text` and publish the resulting diagnostics for `uri`: from
/// what changed since `checker`'s last result, or, when `full`, the whole
/// buffer.
fn publish(
    connection: &Connection,
    checker: &mut karac::DocumentChecker,
    uri: &Uri,
    text: &str,
    full: bool,
) {
    let diags = analysis::diagnostics_with(checker, text, uri, full);
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
