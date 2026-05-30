//! Regex / HTTP / channel method-inference dispatch.
//!
//! Houses per-method return-type synthesizers for `Regex`, the
//! `http.Client` / `http.Response` / `http.Error` triple, and
//! `Sender[T]` / `Receiver[T]` channel ends.

use crate::ast::*;
use crate::token::Span;

use super::types::{IntSize, Type, UIntSize};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Infer the return type of a method call on `Regex`.
    /// Regex is interpreter-only (no codegen). All methods are effect-free.
    pub(super) fn infer_regex_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let match_ty = Type::Named {
            name: "Match".to_string(),
            args: vec![],
        };
        match method {
            "is_match" => {
                if args.len() != 1 {
                    self.type_error(
                        "Regex.is_match() takes 1 argument".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                Type::Bool
            }
            "find" => {
                if args.len() != 1 {
                    self.type_error(
                        "Regex.find() takes 1 argument".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![match_ty],
                }
            }
            "find_all" => {
                if args.len() != 1 {
                    self.type_error(
                        "Regex.find_all() takes 1 argument".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                Type::Named {
                    name: "Vec".to_string(),
                    args: vec![match_ty],
                }
            }
            "replace_all" => {
                if args.len() != 2 {
                    self.type_error(
                        "Regex.replace_all() takes 2 arguments (s, replacement)".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                Type::Str
            }
            _ => self.handle_unknown_method(
                "Regex",
                method,
                &["find", "find_all", "is_match", "replace_all"],
                args,
                span,
            ),
        }
    }

    pub(super) fn infer_http_client_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let response_ty = Type::Named {
            name: "Response".to_string(),
            args: vec![],
        };
        let http_error_ty = Type::Named {
            name: "HttpError".to_string(),
            args: vec![],
        };
        let result_response = Type::Named {
            name: "Result".to_string(),
            args: vec![response_ty, http_error_ty],
        };
        match method {
            "get" => {
                if args.len() != 1 {
                    self.type_error(
                        "Client.get() takes 1 argument (url: str)".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                result_response
            }
            "post" => {
                if args.len() != 2 {
                    self.type_error(
                        "Client.post() takes 2 arguments (url: str, body: str)".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                result_response
            }
            "request" => {
                // Phase-8 line 24 — chained-builder entrypoint.
                if args.len() != 2 {
                    self.type_error(
                        "Client.request() takes 2 arguments (method: str, url: str)".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                Type::Named {
                    name: "RequestBuilder".to_string(),
                    args: vec![],
                }
            }
            _ => self.handle_unknown_method(
                "Client",
                method,
                &["get", "post", "request"],
                args,
                span,
            ),
        }
    }

    /// Phase-8 line 24 — chained-builder method dispatch. `header` /
    /// `body` / `timeout` return `RequestBuilder` (owned-self chain);
    /// `send` returns `Result[Response, HttpError]` matching the eager
    /// `Client.get` / `Client.post` shape.
    pub(super) fn infer_http_request_builder_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let rb_ty = Type::Named {
            name: "RequestBuilder".to_string(),
            args: vec![],
        };
        match method {
            "header" => {
                if args.len() != 2 {
                    self.type_error(
                        "RequestBuilder.header() takes 2 arguments (name: str, value: str)"
                            .to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                rb_ty
            }
            "body" => {
                if args.len() != 1 {
                    self.type_error(
                        "RequestBuilder.body() takes 1 argument (body: str)".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                rb_ty
            }
            "timeout" => {
                if args.len() != 1 {
                    self.type_error(
                        "RequestBuilder.timeout() takes 1 argument (ms: i64)".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Int(IntSize::I64));
                }
                rb_ty
            }
            "send" => {
                if !args.is_empty() {
                    self.type_error(
                        "RequestBuilder.send() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Named {
                    name: "Result".to_string(),
                    args: vec![
                        Type::Named {
                            name: "Response".to_string(),
                            args: vec![],
                        },
                        Type::Named {
                            name: "HttpError".to_string(),
                            args: vec![],
                        },
                    ],
                }
            }
            _ => self.handle_unknown_method(
                "RequestBuilder",
                method,
                &["header", "body", "timeout", "send"],
                args,
                span,
            ),
        }
    }

    pub(super) fn infer_http_response_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        match method {
            "status" => {
                if !args.is_empty() {
                    self.type_error(
                        "Response.status() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            // `body()` and `text()` share semantics (string view of the
            // entity); `bytes()` is the raw-byte view (phase-8 line 32).
            "body" | "text" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("Response.{method}() takes no arguments"),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Str
            }
            "bytes" => {
                if !args.is_empty() {
                    self.type_error(
                        "Response.bytes() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Named {
                    name: "Vec".to_string(),
                    args: vec![Type::UInt(UIntSize::U8)],
                }
            }
            "header" => {
                if args.len() != 1 {
                    self.type_error(
                        "Response.header() takes 1 argument (name: str)".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![Type::Str],
                }
            }
            _ => self.handle_unknown_method(
                "Response",
                method,
                &["body", "bytes", "header", "status", "text"],
                args,
                span,
            ),
        }
    }

    pub(super) fn infer_http_error_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        match method {
            "message" => {
                if !args.is_empty() {
                    self.type_error(
                        "HttpError.message() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Str
            }
            _ => self.handle_unknown_method("HttpError", method, &["message"], args, span),
        }
    }

    /// Infer the return type of a method call on `Sender[T]` or `Receiver[T]`.
    /// `is_sender` distinguishes the two ends; `element` is the channel's `T`.
    pub(super) fn infer_channel_method(
        &mut self,
        is_sender: bool,
        element: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let elem = element.clone();
        let sender_elem = Type::Named {
            name: "Sender".to_string(),
            args: vec![elem.clone()],
        };
        let option_elem = Type::Named {
            name: "Option".to_string(),
            args: vec![elem.clone()],
        };

        if is_sender {
            match method {
                "send" => {
                    // Phase 6 line 218 slice 2 — ScopeLocal escape
                    // check. If the channel's element type names a
                    // type with `impl ScopeLocal for T {}` in scope
                    // (stdlib's `TaskHandle[T]` at v1), reject the
                    // send: ScopeLocal handles cannot be transferred
                    // across the channel boundary. The outer-type
                    // name match is sufficient (TaskHandle[i64] /
                    // TaskHandle[String] all key off the bare
                    // "TaskHandle" name); the parallel walker in
                    // `items.rs::check_type_expr_scope_local` applies
                    // the same rule for (a) function return and (b)
                    // struct/enum field positions.
                    //
                    // v1 ships with a hardcoded `TaskHandle` entry.
                    // `ScopeLocal` is sealed (users cannot `impl
                    // ScopeLocal for MyType` per design.md), so the
                    // set is closed and known to the compiler — when
                    // a second stdlib ScopeLocal type lands (RAII
                    // critical-section guards, scope-bound
                    // iterators), it joins this match. The
                    // collect_scope_local_types walker in items.rs
                    // is the dynamic surface for the same set; the
                    // hardcoded match here is its v1 mirror at the
                    // call-site dispatch point.
                    if let Type::Named { name, .. } = &elem {
                        let is_scope_local = matches!(name.as_str(), "TaskHandle");
                        if is_scope_local {
                            self.type_error(
                                format!(
                                    "ScopeLocal type '{}' cannot be sent across a channel; the value \
                                     is bound to the scope that created it",
                                    name
                                ),
                                span.clone(),
                                TypeErrorKind::ScopeLocalEscape,
                            );
                        }
                    }
                    for arg in args {
                        let at = self.infer_expr(&arg.value);
                        self.check_assignable(&elem, &at, arg.value.span.clone());
                    }
                    Type::Unit
                }
                "clone" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.clone() takes no arguments".to_string(),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    sender_elem
                }
                _ => self.require_known_method("Sender", method, &["clone", "send"], args, span),
            }
        } else {
            // Receiver
            match method {
                "recv" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Receiver.recv() takes no arguments".to_string(),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    elem
                }
                "try_recv" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Receiver.try_recv() takes no arguments".to_string(),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    option_elem
                }
                _ => {
                    self.require_known_method("Receiver", method, &["recv", "try_recv"], args, span)
                }
            }
        }
    }
}
