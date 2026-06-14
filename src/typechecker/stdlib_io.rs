//! Regex / HTTP / channel method-inference dispatch.
//!
//! Houses per-method return-type synthesizers for `Regex`, the
//! `http.Client` / `http.Response` / `http.Error` triple, and
//! `Sender[T]` / `Receiver[T]` channel ends.

use crate::ast::*;
use crate::cross_task_safe::is_cross_task_safe_with;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::inference::resolve_type_var_top;
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
            // `body()` is the string view of the entity; `bytes()` is the
            // raw-byte view (phase-8 line 32). The `text()` alias of
            // `body()` was dropped at the line-64 pre-lock surface freeze.
            "body" => {
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
            // `headers()` — full-map iteration, `Vec[(String, String)]`
            // (phase-8 line 39 follow-up; mirror of `Request.headers()`).
            "headers" => {
                if !args.is_empty() {
                    self.type_error(
                        "Response.headers() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Named {
                    name: "Vec".to_string(),
                    args: vec![Type::Tuple(vec![Type::Str, Type::Str])],
                }
            }
            _ => self.handle_unknown_method(
                "Response",
                method,
                &["body", "bytes", "header", "headers", "status"],
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
    /// `BoundedChannel[T]` method dispatch — `send(value) -> Result[Unit,
    /// ChannelError]` and `recv() -> Option[T]`. Caller gates on
    /// `send`/`recv`; `new` is an associated call typed by the stdlib
    /// signature.
    ///
    /// Mirrors `infer_channel_method`: intercepting here (before the
    /// generic-impl method resolution) takes the concrete element `T`
    /// straight from the receiver's `BoundedChannel[T]` type args — the
    /// generic-impl path doesn't bind `T` from the receiver for the
    /// `impl[T] Foo[T] { fn m() -> T }` shape (the same gap
    /// `TaskHandle.join` works around).
    ///
    /// Records two codegen side-tables:
    /// - `method_callee_types[span] = "BoundedChannel.{method}"` so codegen's
    ///   `dispatch_key` routes to the bounded-channel lowering (the
    ///   hardcoded-dispatch precedent — HTTP `Client`/`Response` do the same).
    /// - `channel_elem_types[span] = T` for `recv` ONLY, so codegen recovers
    ///   the out-slot shape + `elem_size` via the shared
    ///   `channel_elem_ty_and_size` helper. `send` is deliberately NOT
    ///   recorded there (codegen sizes `send` from its argument value) —
    ///   keeping `send` out of `channel_elem_types` also keeps it clear of
    ///   the unbounded-channel dispatch gate, which keys off that map.
    pub(super) fn infer_bounded_channel_method(
        &mut self,
        element: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        self.method_callee_types.insert(
            SpanKey::from_span(span),
            format!("BoundedChannel.{}", method),
        );
        let elem = resolve_type_var_top(element, &self.env.substitutions);
        match method {
            "send" => {
                if args.len() != 1 {
                    self.type_error(
                        "BoundedChannel.send expects exactly one argument".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                // `Result[Unit, ChannelError]`.
                Type::Named {
                    name: "Result".to_string(),
                    args: vec![
                        Type::Unit,
                        Type::Named {
                            name: "ChannelError".to_string(),
                            args: vec![],
                        },
                    ],
                }
            }
            "recv" => {
                if !args.is_empty() {
                    self.type_error(
                        "BoundedChannel.recv() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                // Record T so codegen's `recv` lowering sizes the out-slot
                // and builds `Option[T]` (shared `channel_elem_ty_and_size`).
                let te = Self::type_to_type_expr(&elem);
                self.channel_elem_types.insert(SpanKey::from_span(span), te);
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![elem],
                }
            }
            _ => unreachable!("infer_bounded_channel_method: caller gates on send/recv"),
        }
    }

    pub(super) fn infer_channel_method(
        &mut self,
        is_sender: bool,
        element: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let elem = element.clone();

        // Record the channel element `T` for codegen, keyed by the
        // MethodCall span (same no-collision rationale as
        // `method_unwrap_inner_types` — element type, not receiver type).
        // Dual purpose: (1) `send`/`recv`/`try_recv` read it for the
        // per-call `elem_size` + recv out-slot shape; (2) codegen's
        // channel-method *dispatch gate* keys off the mere presence of an
        // entry at the call span — a scope-stable signal that this is a
        // channel op, since only this function populates the table (the
        // `var_type_names`-based receiver-type lookup is too volatile: the
        // statement-hoisting pre-pass binds then resets it before the
        // method-call pass runs). `clone` is recorded too so it dispatches
        // through the same gate even though its lowering ignores the size.
        // The element `T` is statically known here (the typed
        // `Sender[T]`/`Receiver[T]` receiver) but NOT at `Channel.new()`,
        // so it travels per call site.
        if matches!(
            method,
            "send"
                | "recv"
                | "try_recv"
                | "clone"
                | "__schedule_after"
                | "__schedule_animation_frames"
                | "__schedule_pointer_moves"
        ) {
            let resolved = resolve_type_var_top(&elem, &self.env.substitutions);
            let te = Self::type_to_type_expr(&resolved);
            self.channel_elem_types.insert(SpanKey::from_span(span), te);
        }

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
                    // v1's hardcoded set is `TaskHandle` + `TaskGroup`
                    // (both `impl ScopeLocal` in task_group.kara — a
                    // group escaping via a channel joins its children
                    // too late, same UAF as a handle escaping).
                    // `ScopeLocal` is sealed (users cannot `impl
                    // ScopeLocal for MyType` per design.md), so the
                    // set is closed and known to the compiler — when
                    // a further stdlib ScopeLocal type lands (RAII
                    // critical-section guards, scope-bound
                    // iterators), it joins this match. The
                    // collect_scope_local_types walker in items.rs
                    // is the dynamic surface for the same set; the
                    // hardcoded match here is its v1 mirror at the
                    // call-site dispatch point.
                    if let Type::Named { name, .. } = &elem {
                        let is_scope_local = matches!(name.as_str(), "TaskHandle" | "TaskGroup");
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
                    // Phase 6 line 170 slice 3c — cross-task-safe check on
                    // the channel element type. A channel exists to transfer
                    // values to another task, so a not-cross-task-safe
                    // element type can never be sent safely — there is no
                    // sole-ownership carve-out (unlike a par-block branch),
                    // so the full unsafe set is rejected, shared struct/enum
                    // included. design.md line 1407 (`OnceCell` via
                    // `Channel[OnceCell[T]]`) + § Structured Concurrency
                    // Lifetime Guarantees (Channel.send is one of the five
                    // boundary sites).
                    if let Err(path) =
                        is_cross_task_safe_with(&elem, &self.env.structs, &self.env.enums)
                    {
                        self.emit_cross_task_unsafe_value(
                            "value sent across a channel",
                            &elem,
                            &path,
                            span,
                        );
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
                // Internal compiler builtin backing `std.web.time.after`
                // (phase-10 host-async timer producers). Borrows `self`,
                // takes the delay in milliseconds, returns Unit. Codegen
                // (`src/codegen/channel.rs`) clones the sender's channel
                // reference and hands it to the host `setTimeout`
                // registration; the surviving cloned reference keeps the
                // channel open after `after` returns. Not part of the
                // user-facing channel surface — the `__` prefix + the
                // `writes(Timer)` gating on `after` keep it out of reach of
                // ordinary code.
                "__schedule_after" => {
                    if args.len() != 1 {
                        self.type_error(
                            "Sender.__schedule_after expects exactly one argument (delay in ms)"
                                .to_string(),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    } else {
                        let at = self.infer_expr(&args[0].value);
                        self.check_assignable(
                            &Type::Int(IntSize::I64),
                            &at,
                            args[0].value.span.clone(),
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.time.
                // animation_frames` (phase-10 host-async frame loop). Borrows
                // `self`, takes no argument, returns Unit. Codegen clones the
                // sender's channel reference and hands it to a host
                // requestAnimationFrame loop that feeds the channel once per
                // frame; the surviving clone keeps the channel open for the
                // loop's life. Like `__schedule_after`, kept out of ordinary
                // reach by the `__` prefix + the `writes(Timer)` gating on the
                // `animation_frames` wrapper.
                "__schedule_animation_frames" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_animation_frames takes no arguments".to_string(),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.
                // pointer_moves` (phase-10 host-async event-data producer —
                // the `Channel[T]`, `T != ()` slice). Borrows `self`, takes
                // no argument, returns Unit. Codegen clones the sender's
                // channel reference and hands it to a host pointer listener
                // that marshals each move's coordinates into shared memory
                // and `channel_send`s the `PointerEvent` payload; the
                // surviving clone keeps the channel open for the listener's
                // life. Like the timer/frame builtins, kept out of ordinary
                // reach by the `__` prefix + the `writes(Input)` gating on
                // the `pointer_moves` wrapper.
                "__schedule_pointer_moves" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_pointer_moves takes no arguments".to_string(),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
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
