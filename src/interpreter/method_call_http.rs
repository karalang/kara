//! HTTP-method dispatch — the bodies of the `post`/`path`/`method`/
//! `status`/`body`/`header`/`message` arms lifted out of
//! `eval_method_call`. These handle Client, Request, Response, and
//! HttpError receiver shapes.

use crate::ast::*;
use crate::token::Span;

use super::helpers::{eval_http_get, eval_http_post};
use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_http_method(
        &mut self,
        method: &str,
        obj: Value,
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
            // sees Response receivers for `body`; `text` is Response-only.
            "body" | "text" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
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
