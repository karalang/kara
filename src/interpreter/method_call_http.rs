//! HTTP-method dispatch — the bodies of the `post`/`path`/`method`/
//! `status`/`body`/`header`/`message` arms lifted out of
//! `eval_method_call`. These handle Client, Request, Response, and
//! HttpError receiver shapes.

use crate::ast::*;
use crate::token::Span;

use super::helpers::{eval_http_builder_send, eval_http_get, eval_http_post};
use super::value::{EnumData, Value};

/// Fresh `RequestBuilder` for `Client.request(method, url)` — the inline
/// counterpart to codegen's runtime-side `HTTP_BUILDERS` entry. Defaults
/// match `karac_runtime_http_builder_new`: no headers, empty body, and a
/// zero timeout meaning "unset" (the send path applies a timeout only when
/// positive, so 0 leaves ureq's default in place rather than requesting an
/// instant deadline).
fn new_request_builder(method: String, url: String) -> Value {
    let mut fields = std::collections::HashMap::new();
    fields.insert("method".to_string(), Value::String(method));
    fields.insert("url".to_string(), Value::String(url));
    fields.insert("headers".to_string(), headers_to_value(&[]));
    fields.insert("body".to_string(), Value::String(String::new()));
    fields.insert("timeout_ms".to_string(), Value::Int(0));
    Value::Struct {
        name: "RequestBuilder".to_string(),
        fields,
    }
}

/// Read the builder's ordered header list back out of its struct field.
/// Stored as an array of 2-tuples (not a `Map`) so repeated names keep both
/// their order and their multiplicity, matching the runtime's
/// `Vec<(String, String)>`; `Set-Cookie` is the everyday case where
/// collapsing to a map would silently drop values.
fn builder_headers(fields: &std::collections::HashMap<String, Value>) -> Vec<(String, String)> {
    match fields.get("headers") {
        Some(Value::Array(rc)) => rc
            .read()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match item {
                        Value::Tuple(kv) if kv.len() == 2 => match (&kv[0], &kv[1]) {
                            (Value::String(k), Value::String(v)) => Some((k.clone(), v.clone())),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn headers_to_value(pairs: &[(String, String)]) -> Value {
    let items: Vec<Value> = pairs
        .iter()
        .map(|(k, v)| Value::Tuple(vec![Value::String(k.clone()), Value::String(v.clone())]))
        .collect();
    Value::Array(std::sync::Arc::new(std::sync::RwLock::new(items)))
}

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_http_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            // ── Client method dispatch ────────────────────────────────────────
            // Phase-8 line 17 — wire `Client.get(url)` to the existing
            // `eval_http_get` helper. The helper has been present in
            // `interpreter/helpers.rs` since the post path landed, but
            // was never dispatched (so user calls to `Client.get(url)`
            // ran the stdlib stub returning `Err`). Symmetric to the
            // `post` arm below.
            "get" => {
                if let Value::Struct { ref name, .. } = obj {
                    if name == "Client" {
                        let url = args
                            .first()
                            .map(|a| match self.eval_expr_inner(&a.value) {
                                Value::String(s) => s,
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        return Some(eval_http_get(&url));
                    }
                }
            }
            "post" => {
                if let Value::Struct { ref name, .. } = obj {
                    if name == "Client" {
                        let mut arg_iter = args.iter();
                        let url = arg_iter
                            .next()
                            .map(|a| match self.eval_expr_inner(&a.value) {
                                Value::String(s) => s,
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        let body = arg_iter
                            .next()
                            .map(|a| match self.eval_expr_inner(&a.value) {
                                Value::String(s) => s,
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        return Some(eval_http_post(&url, &body));
                    }
                }
            }
            // ── RequestBuilder dispatch (phase-8 line 24) ─────────────────────
            // `Client.request(method, url)` opens the chained-builder form.
            // Codegen has backed this since phase-8 (`compile_client_request_
            // builder` + the `karac_runtime_http_builder_*` externs) but the
            // interpreter had NO arm for it, so `karac check` passed, AOT and
            // JIT ran, and `karac run --interp` died with "method 'request'
            // not found on type 'Client'" — the same check-green/run-red split
            // as the `File.sync_all` durability gap (B-2026-07-30-16).
            //
            // The builder state lives inline on the struct value rather than
            // in a handle side table: `headers` is an ordered `Vec` of
            // (name, value) pairs, matching the runtime's `Vec<(String,
            // String)>` so multiple values for one name survive in order (a
            // `Map` would collapse them). The stdlib declares `header` / `body`
            // / `timeout` as taking `self` BY VALUE (owned-self chain), so
            // returning a fresh builder per step is the correct semantics —
            // the ownership checker rejects reuse of the moved receiver, which
            // is what makes this indistinguishable from codegen's
            // mutate-the-handle-in-place approach.
            "request" => {
                if let Value::Struct { ref name, .. } = obj {
                    if name == "Client" {
                        let mut arg_iter = args.iter();
                        let mut next_string = |it: &mut std::slice::Iter<'_, CallArg>| {
                            it.next()
                                .map(|a: &CallArg| match self.eval_expr_inner(&a.value) {
                                    Value::String(s) => s,
                                    _ => String::new(),
                                })
                                .unwrap_or_default()
                        };
                        let http_method = next_string(&mut arg_iter);
                        let url = next_string(&mut arg_iter);
                        return Some(new_request_builder(http_method, url));
                    }
                }
            }
            "timeout" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "RequestBuilder" {
                        let ms = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                            Some(Value::Int(i)) => i,
                            _ => 0,
                        };
                        let mut next = fields.clone();
                        next.insert("timeout_ms".to_string(), Value::Int(ms));
                        return Some(Value::Struct {
                            name: "RequestBuilder".to_string(),
                            fields: next,
                        });
                    }
                }
            }
            "send" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "RequestBuilder" {
                        let get_str = |k: &str| match fields.get(k) {
                            Some(Value::String(s)) => s.clone(),
                            _ => String::new(),
                        };
                        let timeout_ms = match fields.get("timeout_ms") {
                            Some(Value::Int(i)) => *i,
                            _ => 0,
                        };
                        let headers = builder_headers(fields);
                        return Some(eval_http_builder_send(
                            &get_str("method"),
                            &get_str("url"),
                            &headers,
                            &get_str("body"),
                            timeout_ms,
                        ));
                    }
                }
            }
            // ── Request method dispatch (HTTP handler ABI trampoline, 2026-05-09) ──
            // F2 owned-String contract: each call returns a freshly-cloned
            // `Value::String`, so multiple calls to `req.path()` / `.method()`
            // never collide on a borrowed buffer. v1 returns an empty String
            // — the interpreter doesn't run a real HTTP server, so there's
            // no real path/method to surface. Pinned by
            // `tests/interpreter.rs::test_server_serve_handler_request_path_returns_owned_string`.
            "path" | "method" | "body" if matches!(&obj, Value::Struct { name, .. } if name == "Request") =>
            {
                return Some(Value::String(String::new()));
            }
            // `Request.headers()` / `.query()` — full-map iteration. The
            // interpreter doesn't run a real HTTP server, so the stub
            // Request carries no data; both return an empty
            // `Vec[(String, String)]`. What this pins is the shape (an
            // array value, method dispatches at all) and interpreter
            // parity with the codegen path; real iteration happens in
            // codegen via the `karac_runtime_http_request_*` accessors.
            "headers" | "query" if matches!(&obj, Value::Struct { name, .. } if name == "Request") =>
            {
                return Some(Value::Array(std::sync::Arc::new(std::sync::RwLock::new(
                    Vec::new(),
                ))));
            }
            // ── Response / HttpError method dispatch ──────────────────────────
            "status" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "Response" {
                        if let Some(v) = fields.get("status") {
                            return Some(v.clone());
                        }
                        return Some(Value::Int(0));
                    }
                }
            }
            // `body` / `text` are the String view of the entity (phase-8
            // line 32); they alias each other. `Request.body` is handled
            // by the earlier `path | method | body` arm, so this arm only
            // sees Response receivers for `body`. (`text()` alias dropped
            // at the line-64 pre-lock surface freeze.)
            "body" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    // `body` is overloaded across three receivers: it READS the
                    // entity on a `Response`, is handled for `Request` by the
                    // earlier `path | method | body` arm, and SETS the payload
                    // on a `RequestBuilder`. Dispatch on the receiver's struct
                    // name, not the method name alone.
                    if name == "RequestBuilder" {
                        let b = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                            Some(Value::String(s)) => s,
                            _ => String::new(),
                        };
                        let mut next = fields.clone();
                        next.insert("body".to_string(), Value::String(b));
                        return Some(Value::Struct {
                            name: "RequestBuilder".to_string(),
                            fields: next,
                        });
                    }
                    if name == "Response" {
                        if let Some(v) = fields.get("body") {
                            return Some(v.clone());
                        }
                        return Some(Value::String(String::new()));
                    }
                }
            }
            // `bytes` is the raw-byte view of the entity (phase-8 line 32),
            // returned as a `Vec[u8]` (array of int-valued bytes). The
            // interpreter captures the body as a String (`into_string`), so
            // it surfaces that string's UTF-8 bytes — best-effort parity
            // with codegen, which preserves true binary payloads. Empty
            // array when the Response carries no body field.
            "bytes" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "Response" {
                        let bytes: Vec<Value> = match fields.get("body") {
                            Some(Value::String(s)) => {
                                s.as_bytes().iter().map(|b| Value::Int(*b as i64)).collect()
                            }
                            _ => Vec::new(),
                        };
                        return Some(Value::Array(std::sync::Arc::new(std::sync::RwLock::new(
                            bytes,
                        ))));
                    }
                }
            }
            "header" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    // Same overload split as `body`: a one-arg LOOKUP on a
                    // `Response`, a two-arg APPEND on a `RequestBuilder`.
                    if name == "RequestBuilder" {
                        let mut arg_iter = args.iter();
                        let mut next_string = |it: &mut std::slice::Iter<'_, CallArg>| {
                            it.next()
                                .map(|a: &CallArg| match self.eval_expr_inner(&a.value) {
                                    Value::String(s) => s,
                                    _ => String::new(),
                                })
                                .unwrap_or_default()
                        };
                        let hname = next_string(&mut arg_iter);
                        let hvalue = next_string(&mut arg_iter);
                        let mut pairs = builder_headers(fields);
                        pairs.push((hname, hvalue));
                        let mut next = fields.clone();
                        next.insert("headers".to_string(), headers_to_value(&pairs));
                        return Some(Value::Struct {
                            name: "RequestBuilder".to_string(),
                            fields: next,
                        });
                    }
                    if name == "Response" {
                        let header_name = args
                            .first()
                            .map(|a| match self.eval_expr_inner(&a.value) {
                                Value::String(s) => s,
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        // Headers are stored as a Map field (key → value strings).
                        if let Some(Value::Map(ref pairs)) = fields.get("headers") {
                            for (k, v) in pairs {
                                if let (Value::String(k_str), Value::String(v_str)) = (k, v) {
                                    if k_str.eq_ignore_ascii_case(&header_name) {
                                        return Some(Value::EnumVariant {
                                            enum_name: "Option".to_string(),
                                            variant: "Some".to_string(),
                                            data: EnumData::Tuple(vec![Value::String(
                                                v_str.clone(),
                                            )]),
                                        });
                                    }
                                }
                            }
                        }
                        return Some(Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        });
                    }
                    // Request side mirrors the path/method/body convention:
                    // the interpreter doesn't run a real HTTP server, so
                    // there's no header map to inspect. Always return
                    // `None`; what the test pins is the *shape* (Option
                    // payload, owned String on Some) and that the method
                    // dispatches at all. Real header lookup happens through
                    // the codegen path via `karac_runtime_http_request_header`.
                    if name == "Request" {
                        // Eagerly evaluate the name arg so any side effects
                        // (or type-checker pinning) still fire.
                        let _ = args.first().map(|a| self.eval_expr_inner(&a.value));
                        return Some(Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        });
                    }
                }
            }
            // `headers()` — full-map iteration, `Vec[(String, String)]`
            // (phase-8 line 39 follow-up). Best-effort interpreter parity:
            // builds the Vec from the Response's `headers` Map field (the
            // same field `header(name)` inspects), or an empty Vec when
            // absent — the interpreter does no real HTTP, so what this pins
            // is the shape (a Vec of (String, String) tuples) and that the
            // method dispatches. Real iteration is codegen-only via the
            // `karac_runtime_http_response_header_{key,val}_at` accessors.
            "headers" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "Response" {
                        let mut pairs: Vec<Value> = Vec::new();
                        if let Some(Value::Map(ref map_pairs)) = fields.get("headers") {
                            for (k, v) in map_pairs {
                                if let (Value::String(k_str), Value::String(v_str)) = (k, v) {
                                    pairs.push(Value::Tuple(vec![
                                        Value::String(k_str.clone()),
                                        Value::String(v_str.clone()),
                                    ]));
                                }
                            }
                        }
                        return Some(Value::Array(std::sync::Arc::new(std::sync::RwLock::new(
                            pairs,
                        ))));
                    }
                }
            }
            "message" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "HttpError" {
                        if let Some(v) = fields.get("message") {
                            return Some(v.clone());
                        }
                        return Some(Value::String(String::new()));
                    }
                }
            }
            _ => return None,
        }
        None
    }
}
