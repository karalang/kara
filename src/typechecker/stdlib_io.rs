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
                        *span,
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
                        *span,
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
                        *span,
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    self.check_expr(&arg.value, &Type::Str);
                }
                Type::Str
            }
            // B-2026-07-31-1 — `Regex` dispatches through this hardcoded arm
            // (not the impl-table path the other baked handle types use), and
            // `handle_unknown_method` is SILENT unless the typo is edit-distance
            // close, so `re.totally_bogus()` typechecked clean. The four arms
            // above are the complete `Regex` surface — enumerated identically in
            // the interpreter and codegen (B-2026-07-14-19) — so an unknown
            // method here is genuinely absent: use `require_known_method`, which
            // always emits `NoMethodFound` (with a `did you mean` when close).
            _ => self.require_known_method(
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
                        *span,
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
                        *span,
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
                        *span,
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
            // B-2026-07-31-1 follow-up — the four HTTP types (`Client`,
            // `RequestBuilder`, `Response`, `HttpError`) dispatch through
            // these hardcoded arms rather than the impl-table path, so the
            // baked-nominal check that fixed the other handle types never
            // sees them. They were the last four `handle_unknown_method`
            // call sites in the compiler, and that helper is SILENT unless
            // the name is edit-distance close: `c.gett(u)` was caught,
            // `c.totally_bogus_xyz()` typechecked clean and fell through to
            // `Type::Error` (universally assignable), detonating later as a
            // codegen "no handler for method" or an interpreter dispatch
            // miss. The enumerations here are at parity with codegen — see
            // `codegen/method_call.rs` (`Client` get/post/request,
            // `RequestBuilder` header/body/timeout/send, `Response`
            // status/body/bytes/header/headers, `HttpError` message) — so an
            // unknown method reaching these arms is genuinely absent and
            // `require_known_method` (always emits, `did you mean` when
            // close) is correct. With these four converted,
            // `handle_unknown_method` had no callers left and was removed
            // along with its `maybe_emit_method_typo` helper, so the
            // silent-fall-through pattern is now structurally absent rather
            // than merely unused.
            _ => {
                self.require_known_method("Client", method, &["get", "post", "request"], args, span)
            }
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
                        *span,
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
                        *span,
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
                        *span,
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
                        *span,
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
            _ => self.require_known_method(
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
                        *span,
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Str
            }
            "bytes" => {
                if !args.is_empty() {
                    self.type_error(
                        "Response.bytes() takes no arguments".to_string(),
                        *span,
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
                        *span,
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Named {
                    name: "Vec".to_string(),
                    args: vec![Type::Tuple(vec![Type::Str, Type::Str])],
                }
            }
            _ => self.require_known_method(
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Str
            }
            _ => self.require_known_method("HttpError", method, &["message"], args, span),
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    if !pin_channel_elem_from_arg(&mut self.env, &elem, &at) {
                        self.check_assignable(&elem, &at, arg.value.span);
                    }
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
                        *span,
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
                // B-2026-08-22-21 — `recv_blocking` must pass this gate too.
                // The entry in `channel_elem_types` is what codegen's
                // method-call dispatch keys on to recognise a channel op at
                // all, so without it the call reaches the user-impl path and
                // dies with "no handler for method 'recv_blocking'" — which is
                // how this was found, since the typecheck and interpreter arms
                // were already in place by then.
                | "recv_blocking"
                | "try_send"
                | "try_recv"
                | "clone"
                | "__schedule_after"
                | "__schedule_every"
                | "__schedule_animation_frames"
                | "__schedule_pointer_moves"
                | "__schedule_wheel"
                | "__schedule_keydown"
                | "__schedule_keyup"
                | "__schedule_clicks"
                | "__schedule_dblclick"
                | "__schedule_resize"
                | "__schedule_contextmenu"
                | "__schedule_focus"
                | "__schedule_blur"
                | "__schedule_touchstart"
                | "__schedule_touchmove"
                | "__schedule_touchend"
                | "__schedule_input"
        ) {
            let resolved = resolve_type_var_top(&elem, &self.env.substitutions);
            let te = Self::type_to_type_expr(&resolved);
            self.channel_elem_types.insert(SpanKey::from_span(span), te);
        }

        // B-2026-08-22-21 — RE-RECORD the element type after the arm below has
        // run, and read the doc comment on `pin_channel_elem_from_arg` for why
        // that is not redundant with the write above.
        //
        // On an UNANNOTATED `Channel.new()` the element is still an unsolved
        // `?T0` when the dispatch-gate write happens, and only the arm's
        // `pin_channel_elem_from_arg` solves it — from the FIRST send's
        // argument. So the first send recorded `?T0`, which
        // `type_to_type_expr` renders as a 1-word placeholder, and codegen
        // sized that call's `elem_size` at 8 bytes for a 24-byte `String`.
        //
        // MEASURED on clean `main`, so this is a pre-existing `send` defect
        // that `try_send` merely inherits: `let (tx, rx) = Channel.new(); let
        // a = "alpha"; tx.send(a); println(f"got {rx.recv()}")` printed
        // `got alpha` under `--interp` and `got ` under `karac build` — a
        // silently dropped payload — while `karac run` aborted in the runtime
        // with `receiver elem_size 24 exceeds sent blob 8`. An ANNOTATED
        // `(Sender[String], Receiver[String])` pair was unaffected, which is
        // why every existing String-channel test passes: they all annotate.
        //
        // The gate write above STAYS, unconditionally and even when `T` is
        // unsolved: codegen's channel dispatch keys on an entry merely being
        // PRESENT at this span, so removing or deferring it would change which
        // calls are recognised as channel ops at all. This second write only
        // upgrades the recorded type once the arm has pinned it, so the
        // dispatch behaviour is untouched and only the size is corrected.
        let result = self.infer_channel_method_arm(is_sender, &elem, method, args, span);
        if matches!(
            method,
            "send" | "recv" | "recv_blocking" | "try_send" | "try_recv" | "clone"
        ) {
            let resolved = resolve_type_var_top(&elem, &self.env.substitutions);
            let te = Self::type_to_type_expr(&resolved);
            self.channel_elem_types.insert(SpanKey::from_span(span), te);
        }
        result
    }

    /// The per-method body of [`infer_channel_method`], split out so the
    /// caller can re-record the (now-pinned) element type afterwards.
    fn infer_channel_method_arm(
        &mut self,
        is_sender: bool,
        elem: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let elem = elem.clone();
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
                                *span,
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
                        if !pin_channel_elem_from_arg(&mut self.env, &elem, &at) {
                            self.check_assignable(&elem, &at, arg.value.span);
                        }
                    }
                    Type::Unit
                }
                "clone" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.clone() takes no arguments".to_string(),
                            *span,
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
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    } else {
                        let at = self.infer_expr(&args[0].value);
                        self.check_assignable(&Type::Int(IntSize::I64), &at, args[0].value.span);
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.time.every`
                // (phase-10 host-async interval producer). Borrows `self`,
                // takes the period in milliseconds, returns Unit. Like
                // `__schedule_after` but *multi-shot*: codegen hands the host a
                // `setInterval` that re-feeds the channel every period and
                // never drops its sender. Kept out of ordinary reach by the
                // `__` prefix + the `writes(Timer)` gating on `every`.
                "__schedule_every" => {
                    if args.len() != 1 {
                        self.type_error(
                            "Sender.__schedule_every expects exactly one argument (period in ms)"
                                .to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    } else {
                        let at = self.infer_expr(&args[0].value);
                        self.check_assignable(&Type::Int(IntSize::I64), &at, args[0].value.span);
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
                            *span,
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
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.wheel`
                // (sibling of `__schedule_pointer_moves`; non-unit `WheelEvent`
                // payload). Borrows `self`, takes no argument, returns Unit;
                // codegen clones the sender and hands it to the host wheel
                // listener. Kept out of ordinary reach by the `__` prefix +
                // the `writes(Input)` gating on the `wheel` wrapper.
                "__schedule_wheel" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_wheel takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.keydown`
                // (sibling of `__schedule_wheel`; non-unit `KeyEvent` payload).
                // Borrows `self`, takes no argument, returns Unit; codegen clones
                // the sender and hands it to the host keydown listener. Kept out
                // of ordinary reach by the `__` prefix + the `writes(Input)`
                // gating on the `keydown` wrapper.
                "__schedule_keydown" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_keydown takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.keyup`
                // (key-release sibling of `__schedule_keydown`; the same
                // `KeyEvent` payload). Borrows `self`, takes no argument,
                // returns Unit; codegen clones the sender and hands it to the
                // host keyup listener. Kept out of ordinary reach by the `__`
                // prefix + the `writes(Input)` gating on the `keyup` wrapper.
                "__schedule_keyup" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_keyup takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.input`
                // (a DOM-element value channel; non-unit `InputEvent` = one
                // `f64` payload). Borrows `self`, takes no argument, returns
                // Unit; codegen clones the sender and hands it to the host input
                // listener. Kept out of ordinary reach by the `__` prefix + the
                // `writes(Input)` gating on the `input` wrapper.
                "__schedule_input" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_input takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.clicks`
                // (discrete click-position sibling of `__schedule_pointer_moves`;
                // non-unit `ClickEvent` = two `f64`s payload). Borrows `self`,
                // takes no argument, returns Unit; codegen clones the sender and
                // hands it to the host click listener. Kept out of ordinary reach
                // by the `__` prefix + the `writes(Input)` gating on the `clicks`
                // wrapper.
                "__schedule_clicks" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_clicks takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.dblclick`
                // (double-press sibling of `__schedule_clicks`; the same 16-byte
                // `ClickEvent` payload). Borrows `self`, takes no argument,
                // returns Unit; codegen clones the sender and hands it to the
                // host dblclick listener. Kept out of ordinary reach by the `__`
                // prefix + the `writes(Input)` gating on the `dblclick` wrapper.
                "__schedule_dblclick" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_dblclick takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.resize`
                // (window-dimension producer; non-unit `ResizeEvent` = two
                // `i64`s payload). Borrows `self`, takes no argument, returns
                // Unit; codegen clones the sender and hands it to the host resize
                // listener. Kept out of ordinary reach by the `__` prefix + the
                // `writes(Input)` gating on the `resize` wrapper.
                "__schedule_resize" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_resize takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtin backing `std.web.events.contextmenu`
                // (right-click sibling of `__schedule_clicks`; the same 16-byte
                // `ClickEvent` payload). Borrows `self`, takes no argument,
                // returns Unit; codegen clones the sender and hands it to the
                // host contextmenu listener (which preventDefaults the native
                // menu). Kept out of ordinary reach by the `__` prefix + the
                // `writes(Input)` gating on the `contextmenu` wrapper.
                "__schedule_contextmenu" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_contextmenu takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtins backing `std.web.events.focus` /
                // `std.web.events.blur` — the first unit-payload `events.*`
                // producers (channel element `()`, a 0-byte send; sibling of
                // `__schedule_animation_frames` but driven by a focus/blur
                // listener). Borrow `self`, take no argument, return Unit; codegen
                // clones the sender and hands it to the host listener. Kept out of
                // ordinary reach by the `__` prefix + the `writes(Input)` gating on
                // the `focus`/`blur` wrappers.
                "__schedule_focus" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_focus takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                "__schedule_blur" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_blur takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // Internal compiler builtins backing `std.web.events.touchstart`
                // / `.touchmove` / `.touchend` — the touch (finger/pen) gesture
                // family; each carries the same 16-byte `TouchEvent` (two `f64`s
                // — `x`, `y`) payload as `ClickEvent` (sibling of
                // `__schedule_pointer_moves`/`clicks`, single primary touch).
                // Borrow `self`, take no argument, return Unit; codegen clones
                // the sender and hands it to the host touch listener (touchmove
                // additionally `{ passive: false }` + preventDefaults the page
                // scroll during a drag). Kept out of ordinary reach by the `__`
                // prefix + the `writes(Input)` gating on the wrappers.
                "__schedule_touchstart" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_touchstart takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                "__schedule_touchmove" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_touchmove takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                "__schedule_touchend" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Sender.__schedule_touchend takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    Type::Unit
                }
                // B-2026-08-22-21 — design.md:6083's `try_send`, the
                // NON-PANICKING send. `send` returns unit, so a full bounded
                // channel has nowhere to report failure and panics
                // (`runtime/src/channel.rs` documents that fail-fast choice);
                // `try_send` returns a `Result`, which is the whole reason the
                // spec gives it one.
                //
                // Both `SendError` variants carry the REJECTED VALUE back:
                // `send` consumes its argument, so a failure that dropped the
                // value would leave the caller unable to retry or recover it.
                //
                // The two element-safety checks `send` performs
                // (ScopeLocal escape, cross-task-safety) apply identically —
                // a value that cannot cross a channel boundary cannot cross it
                // fallibly either — so this arm runs the same pair rather than
                // being a laxer door to the same place.
                "try_send" => {
                    if let Type::Named { name, .. } = &elem {
                        if matches!(name.as_str(), "TaskHandle" | "TaskGroup") {
                            self.type_error(
                                format!(
                                    "ScopeLocal type '{}' cannot be sent across a channel; the \
                                     value is bound to the scope that created it",
                                    name
                                ),
                                *span,
                                TypeErrorKind::ScopeLocalEscape,
                            );
                        }
                    }
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
                    if args.len() != 1 {
                        self.type_error(
                            "Sender.try_send expects exactly one argument".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    for arg in args {
                        let at = self.infer_expr(&arg.value);
                        if !pin_channel_elem_from_arg(&mut self.env, &elem, &at) {
                            self.check_assignable(&elem, &at, arg.value.span);
                        }
                    }
                    // RE-RESOLVE, because `try_send` is the only channel
                    // method that both PINS the element from its argument and
                    // RETURNS a type mentioning it. On an unannotated
                    // `Channel.new()` / `.bounded(n)`, `elem` is still `?T0`
                    // when this arm is entered, and the loop above is what
                    // solves it — so `elem.clone()` here would hand the match
                    // a scrutinee of `Result[(), SendError[?T0]]`.
                    //
                    // MEASURED: the failure only showed when the match was on
                    // the FIRST `try_send` in the function (a preceding
                    // `let r = tx.try_send(x)` pins `?T0` and hides it), and it
                    // surfaced far downstream as `codegen: no handler for
                    // method 'len' on variable 'v'` — the payload binding had
                    // no recorded type, so `v.len()` on a `String` payload
                    // found no dispatcher. `send` cannot hit this (it returns
                    // unit) and `recv`/`try_recv` cannot either (no argument to
                    // pin from), which is why it is new with this method.
                    let pinned = resolve_type_var_top(&elem, &self.env.substitutions);
                    Type::Named {
                        name: "Result".to_string(),
                        args: vec![
                            Type::Unit,
                            Type::Named {
                                name: "SendError".to_string(),
                                args: vec![pinned],
                            },
                        ],
                    }
                }
                _ => self.require_known_method(
                    "Sender",
                    method,
                    &["clone", "send", "try_send"],
                    args,
                    span,
                ),
            }
        } else {
            // Receiver
            match method {
                "recv" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Receiver.recv() takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    elem
                }
                // B-2026-08-22-21 — design.md:6099's `recv_blocking`, the
                // `blocks`-flavoured sibling of `recv`. Same signature and
                // same value: the difference is the EXECUTION VERB it carries
                // (`blocks` vs `recv`'s `suspends`), which is what design.md's
                // two execution verbs exist to drive — scheduler placement.
                // It is not a synonym.
                //
                // The two lower identically today because the runtime's
                // `karac_runtime_channel_recv` already PARKS ON A CONDVAR on
                // every threads-target — i.e. `recv` blocks a thread in
                // practice, and `suspends` describes the scheduler that has
                // not landed yet (the `send` fail-fast comment in
                // `runtime/src/channel.rs` says as much). So `recv_blocking`
                // is honest about what happens now, and `recv` keeps the
                // forward-looking verb; when the suspending scheduler lands,
                // `recv` changes and this stays put.
                "recv_blocking" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Receiver.recv_blocking() takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    elem
                }
                "try_recv" => {
                    if !args.is_empty() {
                        self.type_error(
                            "Receiver.try_recv() takes no arguments".to_string(),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                    }
                    option_elem
                }
                _ => self.require_known_method(
                    "Receiver",
                    method,
                    &["recv", "recv_blocking", "try_recv"],
                    args,
                    span,
                ),
            }
        }
    }
}

/// Pin an as-yet-unsolved channel element type from the value being sent
/// (B-2026-08-21-29).
///
/// `Channel.new()` mints a fresh type variable shared by the `Sender[T]` and
/// `Receiver[T]` it returns, and the comment above that arm states that "a
/// later `tx.send(x)` / `rx.recv()` pins the same `T`". It did not. Both send
/// paths reached `check_assignable(&elem, &at, ..)`, which CHECKS rather than
/// UNIFIES, so an unsolved `?T0` against an `i64` argument reported
/// `expected '?T0', found 'i64'` instead of solving `?T0 := i64`.
///
/// The consequence was that the annotated spelling
/// (`let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();`) was the
/// only channel construction that worked — and design.md writes the
/// unannotated one (`let (tx, rx) = Channel.new();`, :6111).
///
/// Returns true when it bound the variable, so the caller can skip the
/// assignability check it would otherwise fail.
fn pin_channel_elem_from_arg(
    env: &mut crate::typechecker::env::TypeEnv,
    elem: &Type,
    arg_ty: &Type,
) -> bool {
    // Only an UNSOLVED variable is pinnable; a solved one must still be
    // checked, or `send` would accept any type after the first call.
    let resolved = resolve_type_var_top(elem, &env.substitutions);
    let Type::TypeVar(id) = resolved else {
        return false;
    };
    // Never pin to an error or to another unsolved variable — the first
    // propagates a bogus solution, the second solves nothing.
    if matches!(arg_ty, Type::Error | Type::TypeVar(_)) {
        return false;
    }
    env.substitutions.insert(id, arg_ty.clone());
    true
}
