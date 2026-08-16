//! Identifier-receiver method dispatch — module-path and type-path calls.
//!
//! Seventh slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Handles the
//! calls whose receiver is a bare `Identifier` rather than a value, which
//! the regular receiver pipeline would misread as a variable:
//!
//! - the strict-provenance `ptr` module — `ptr.addr(p)`,
//!   `ptr.with_addr(p, a)`, `ptr.from_exposed(a)` (design.md § Pointer
//!   Provenance). These route through the generic-aware dispatch path,
//!   because every entry is parameterised over `T` and the non-generic
//!   `check_assignable` loop would silently accept any argument shape
//!   against a `*const T` slot;
//! - the remaining prelude-module surfaces reached by module name;
//! - type-receiver associated calls — `T.method(args)` where `T` names a
//!   struct, enum or primitive, including the `From` special case, where
//!   the argument's source type disambiguates between several
//!   `impl From[X] for T`.
//!
//! Each is skipped when a local binding shadows the name: the prelude
//! modules are registered as `SymbolKind::Module`, but local scope wins by
//! name resolution, mirroring the spec's prelude-shadow rule.
//!
//! The block order is load-bearing — `infer_method_call` is a
//! first-match-wins chain, so these guards keep the exact relative order
//! they had inline, and the function is called from the same position in
//! that chain.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::env::{FunctionSig, ImplInfo};
use super::inference::substitute_type_params;
use super::types::{type_display, SubstValue, Type};
use super::TypeErrorKind;
use std::collections::HashMap;

impl<'a> super::TypeChecker<'a> {
    /// Type a method call whose receiver is a bare identifier naming a
    /// module or a type.
    ///
    /// Returns `Some(ty)` when this surface claims the call (including
    /// `Some(Type::Error)` when it claims it but the call is ill-formed and
    /// a diagnostic has been emitted), and `None` when the receiver is an
    /// ordinary value expression that later links in the
    /// `infer_method_call` chain should handle.
    pub(super) fn try_identifier_receiver_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Type> {
        // Strict-provenance `ptr` module — `ptr.addr(p)`, `ptr.with_addr(p, a)`,
        // `ptr.from_exposed(a)`, etc. (design.md § Pointer Provenance, v60
        // item 20). Routes through the generic-aware dispatch path because
        // every entry is parameterised over `T`: the bare `infer_method_call`
        // arms below (the `env` arm) use a non-generic `check_assignable`
        // loop which would silently accept any argument shape against a
        // `*const T` slot — `(Type::TypeParam(_), _) => true` in
        // `types_compatible`. The `check_call_args_with_substitution_full`
        // path instantiates `T` to a fresh metavar so the outer `*const ?T`
        // shape unifies properly against the supplied argument's type.
        //
        // Skipped when a local binding shadows `ptr` — the prelude module
        // is registered with `SymbolKind::Module` but local-scope wins
        // by name resolution, mirroring the spec's prelude-shadow rule.
        if let ExprKind::Identifier(mod_name) = &object.kind {
            // `cpu.supports("avx2") -> bool` — the runtime CPU-feature probe
            // (design.md § Multiversioning; the `#[multiversion]` dispatch
            // primitive). One String feature-name argument. Recognised as a
            // namespace intrinsic only when no local binding shadows `cpu`
            // (same prelude-shadow rule as `ptr.const`).
            if mod_name == "cpu" && self.local_scope.lookup("cpu").is_none() && method == "supports"
            {
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "'cpu.supports' expects 1 argument (a feature-name string), found {}",
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                } else {
                    let at = self.infer_expr(&args[0].value);
                    if !matches!(at, Type::Str) {
                        self.type_error(
                            "'cpu.supports' expects a String feature name — e.g. \
                             `cpu.supports(\"avx2\")`"
                                .to_string(),
                            args[0].value.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                return Some(Type::Bool);
            }
            if mod_name == "ptr"
                && self.local_scope.lookup("ptr").is_none()
                && (method == "const" || method == "mut")
            {
                return Some(self.infer_ptr_construction(method, args, span));
            }
            if mod_name == "ptr" && self.local_scope.lookup("ptr").is_none() {
                let dotted = format!("ptr.{}", method);
                if let Some(sig) = self.env.functions.get(&dotted).cloned() {
                    if args.len() != sig.params.len() {
                        self.type_error(
                            format!(
                                "'{}.{}' expects {} argument(s), found {}",
                                mod_name,
                                method,
                                sig.params.len(),
                                args.len()
                            ),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return Some(sig.return_type);
                    }
                    return Some(self.check_call_args_with_substitution_full(
                        args,
                        &sig.params,
                        &sig.return_type,
                        span,
                        false,
                        None,
                        Some(&sig.generic_params),
                        sig.where_clause.as_ref(),
                        span,
                    ));
                }
            }
        }

        // Lowercase stdlib module aliases: `env.args()`, `clock.now()`,
        // `stdout.println(s)`, `fs.write(p, c)`, … These use lowercase module
        // names (design.md § I/O), distinct from the capitalized resource
        // names used by the effect system. Map each lowercase module to its
        // capitalized resource equivalent so the shared method signatures are
        // found — first in the baked-impl table (`env.impls`, where the
        // slice-2 migration moved the I/O resource methods), then in
        // `env.functions` for any future entries that can't be expressed as
        // impl methods. Resolving through the baked impl is what gives the
        // call its exact return type (e.g. `Result[String, IoError]`), which
        // flows into `pattern_binding_types` so a `match` arm binds the Ok
        // payload at the right width — mirrors the resolver `push`, the
        // interpreter alias map, and codegen's `ambient_resource_for_alias`.
        if let ExprKind::Identifier(mod_name) = &object.kind {
            let resource_name = match mod_name.as_str() {
                "env" => Some("Env"),
                "clock" => Some("Clock"),
                "rand" => Some("RandomSource"),
                "stdin" => Some("Stdin"),
                "stdout" => Some("Stdout"),
                "stderr" => Some("Stderr"),
                "fs" => Some("FileSystem"),
                _ => None,
            }
            .filter(|_| self.local_scope.lookup(mod_name).is_none());
            if let Some(resource) = resource_name {
                let impl_sig = self.env.impls.iter().find_map(|imp| {
                    // Lowercase-module dispatch (`env.args()`) targets
                    // ambient resource impls registered with empty
                    // target_args; specialized variants of these don't
                    // exist today.
                    if imp.target_type == resource && imp.target_args.is_empty() {
                        imp.methods.get(method).cloned()
                    } else {
                        None
                    }
                });
                let dotted = format!("{}.{}", resource, method);
                let sig_opt = impl_sig.or_else(|| self.env.functions.get(&dotted).cloned());
                if let Some(sig) = sig_opt {
                    if args.len() != sig.params.len() {
                        self.type_error(
                            format!(
                                "'{}.{}' expects {} argument(s), found {}",
                                mod_name,
                                method,
                                sig.params.len(),
                                args.len()
                            ),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return Some(sig.return_type);
                    }
                    for (arg, param_ty) in args.iter().zip(sig.params.iter()) {
                        let at = self.infer_expr(&arg.value);
                        self.check_assignable(param_ty, &at, arg.value.span);
                    }
                    return Some(sig.return_type);
                }
            }
        }

        // Type-receiver associated calls: `T.method(args)` where `T` is a
        // type name (struct, enum, or primitive). The parser produces a
        // MethodCall with `object = Identifier("T")`; the regular receiver
        // pipeline below would treat `T` as a value and fail.
        //
        // From dispatch is special-cased — the source type of the argument
        // disambiguates between multiple `impl From[X] for T` impls.
        if let ExprKind::Identifier(type_name) = &object.kind {
            let is_known_type = self.env.structs.contains_key(type_name)
                || self.env.enums.contains_key(type_name)
                || matches!(
                    type_name.as_str(),
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "usize"
                        | "f32"
                        | "f64"
                        | "bool"
                        | "char"
                        | "String"
                );
            if is_known_type {
                // Comptime `Type` reflection (substrate 2): `MyType.name()`,
                // `.fields()`, `.variants()`, `.is_struct()`, … The reflection
                // API is fixed by the language and only valid at comptime (a
                // `Type` value cannot exist at runtime). Reserve the reflection
                // method names when inside a comptime context; outside it, fall
                // through so an identically-named user associated fn still
                // resolves. Spec: deferred.md § Comptime — Reflection API.
                if Self::is_reflection_method(method) && self.comptime_depth > 0 {
                    let ty = self.infer_type_reflection_method(method, args, span);
                    self.record_expr_type(span, &ty);
                    return Some(ty);
                }
                // Cancel-narrowing side-table: record `Type.method` for this
                // call site so codegen can elide the par-branch cancel check
                // when the resolved callee is provably non-effectful.
                self.method_callee_types.insert(
                    SpanKey::from_span(span),
                    format!("{}.{}", type_name, method),
                );
                // `f64.parse(s: String) -> Option[f64]`. Unlike the integer
                // parses (which ride the untyped-primitive-assoc passthrough —
                // their payload is i64, so the Option element defaulting to i64
                // happens to be correct), float parse MUST be typed: the some-
                // payload holds the f64 bit pattern, and without an
                // `Option[f64]` element type the match binding extracts those
                // bits as an i64 and prints garbage. Phase-8 floor for the
                // self-hosting lexer's float literals (f32.parse is deferred —
                // its narrower payload width needs its own runtime path).
                if method == "parse" && type_name == "f64" {
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    let f64_ty = self
                        .primitive_type("f64")
                        .expect("f64 is a known primitive");
                    return Some(Type::Named {
                        name: "Option".to_string(),
                        args: vec![f64_ty],
                    });
                }
                // Integer `<int>.parse(s) -> Option[<int>]` and
                // `<int>.from_str_radix(s, radix) -> Option[<int>]`. These rode
                // the untyped-primitive-assoc passthrough (payload defaulting to
                // i64 — value-correct), but an UNANNOTATED `let o = i64.parse(s)`
                // then left the match-bound `Some(v)` without a concrete element
                // type, so `v.to_string()` / further method dispatch on `v` fell
                // through in codegen (the dispatch key is the typechecker-recorded
                // receiver type — blocker #11). Typing the result explicitly
                // (mirrors the `f64.parse` arm above) records `Option[<int>]` so
                // the binding's element type reaches dispatch; the annotated form
                // (`let o: Option[i64] = …`) already worked.
                if (method == "parse" || method == "from_str_radix")
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    )
                {
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    if let Some(int_ty) = self.primitive_type(type_name.as_str()) {
                        return Some(Type::Named {
                            name: "Option".to_string(),
                            args: vec![int_ty],
                        });
                    }
                }
                // `char.try_from(n: <int>) -> Result[char, i64]` — fallible
                // codepoint→char conversion (blocker #10; the
                // `E_INT_AS_CHAR` rejection of `n as char` points here). Not
                // every integer is a valid Unicode scalar (the surrogate range
                // `0xD800..=0xDFFF` and values above `0x10FFFF` are rejected),
                // so the result is a `Result`; the `Err` payload is the
                // offending codepoint value (`i64`) — no dedicated error enum
                // needed (the error type is unspecified at the language level).
                if method == "try_from" && type_name == "char" {
                    if args.len() != 1 {
                        self.type_error(
                            format!("char.try_from expects 1 argument, got {}", args.len()),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        return Some(Type::Error);
                    }
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !matches!(arg_ty, Type::Int(_) | Type::UInt(_) | Type::Error) {
                        self.type_error(
                            format!(
                                "char.try_from expects an integer codepoint, got `{}`",
                                type_display(&arg_ty)
                            ),
                            *span,
                            TypeErrorKind::TypeMismatch,
                        );
                        return Some(Type::Error);
                    }
                    let char_ty = self
                        .primitive_type("char")
                        .expect("char is a known primitive");
                    let i64_ty = self
                        .primitive_type("i64")
                        .expect("i64 is a known primitive");
                    return Some(Type::Named {
                        name: "Result".to_string(),
                        args: vec![char_ty, i64_ty],
                    });
                }
                if method == "from" && args.len() == 1 {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if arg_ty == Type::Error {
                        return Some(Type::Error);
                    }
                    if let Some(imp) = self.env.find_from_impl(&arg_ty, type_name, &[]) {
                        return Some(
                            imp.methods
                                .get("from")
                                .map(|sig| sig.return_type.clone())
                                .unwrap_or(Type::Error),
                        );
                    }
                    self.type_error(
                        format!(
                            "no `impl From[{}] for {}` is in scope",
                            type_display(&arg_ty),
                            type_name
                        ),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Some(Type::Error);
                }
                // Numeric narrowing / sign-changing `T.try_from(x: <int>) ->
                // Result[T, String]` for an integer target `T` (design.md
                // § Conversion Traits — "fails if out of range"). Dispatch
                // mirrors the `from` arm above: the registered built-in TryFrom
                // impls (env_build) disambiguate on the source type, and the
                // arm returns the impl's `Result[T, String]` return type. `char`
                // has its own `try_from` arm above; refinement / distinct-type
                // `try_from` target their own names, so this only fires for the
                // primitive integer targets and never shadows them.
                if method == "try_from"
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    )
                {
                    if args.len() != 1 {
                        self.type_error(
                            format!(
                                "{}.try_from expects 1 argument, got {}",
                                type_name,
                                args.len()
                            ),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        return Some(Type::Error);
                    }
                    let arg_ty = self.infer_expr(&args[0].value);
                    if arg_ty == Type::Error {
                        return Some(Type::Error);
                    }
                    if let Some(imp) = self.env.find_tryfrom_impl(&arg_ty, type_name, &[]) {
                        return Some(
                            imp.methods
                                .get("try_from")
                                .map(|sig| sig.return_type.clone())
                                .unwrap_or(Type::Error),
                        );
                    }
                    self.type_error(
                        format!(
                            "`{}.try_from` expects an integer argument, got `{}`",
                            type_name,
                            type_display(&arg_ty)
                        ),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Some(Type::Error);
                }
                // General associated call: look up the method on the target
                // type with inherent-beats-trait priority per design.md
                // § Method Resolution Step 3. Multi-inherent / multi-trait
                // ambiguity detection (Step 4) is deferred.
                if let Some(sig) = self.env.find_method(type_name, &[], method).cloned() {
                    if args.len() != sig.params.len() {
                        self.type_error(
                            format!(
                                "method '{}' expects {} argument(s), found {}",
                                method,
                                sig.params.len(),
                                args.len()
                            ),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return Some(sig.return_type);
                    }
                    for (arg, param) in args.iter().zip(sig.params.iter()) {
                        let arg_ty = self.infer_expr(&arg.value);
                        self.check_assignable(param, &arg_ty, arg.value.span);
                    }
                    return Some(sig.return_type);
                }
                // Unresolved associated call on a SCALAR PRIMITIVE — reject
                // here rather than fall through. A primitive type has no
                // user-extensible `impl` surface, and every valid primitive
                // associated fn (`parse` / `from_str_radix` / `from` /
                // `try_from`, plus the `.MAX` / `.MIN` field accesses handled
                // elsewhere) has an explicit arm above, so an unresolved one is
                // genuinely undefined. Without this, `i64.max_value()` (a
                // Rust-ism — Kāra spells it `i64.MAX`) passed `karac check`
                // clean and then panicked the tree-walk interpreter
                // (`eval_expr` treated the `i64` receiver as an undefined
                // variable → `unreachable!`) or failed codegen with a raw
                // "no handler for method" — a green-check-then-crash
                // (B-2026-07-22-10). Struct / enum / `String` receivers keep
                // the fall-through (they may still resolve downstream).
                if matches!(
                    type_name.as_str(),
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "usize"
                        | "f32"
                        | "f64"
                        | "bool"
                        | "char"
                ) {
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    self.type_error(
                        format!("no associated function '{method}' on type '{type_name}'"),
                        *span,
                        TypeErrorKind::NoMethodFound,
                    );
                    return Some(Type::Error);
                }
                // Known type but no matching method — fall through so the
                // existing "method not found" diagnostic fires below.
            }
        }
        None
    }

    /// Type a concrete-type UFCS call — `TypeName[Args].method(..)`.
    ///
    /// The parser disambiguates this to a single-segment
    /// `Path { generic_args: Some(..) }` receiver; dispatch routes through
    /// `find_methods_with_args` so impl-level bounds discharge against the
    /// explicit type-args, then substitutes each impl-level generic param
    /// with its concrete arg before validating the call arguments.
    ///
    /// Returns `None` when the receiver is not such a path, leaving the
    /// call to later links in the `infer_method_call` chain.
    pub(super) fn try_path_receiver_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Type> {
        // The parser disambiguates `TypeName[…].method(` to a single-segment
        // `Path { generic_args: Some(...) }` object; here we route through
        // `find_methods_with_args` so impl-level bounds discharge against
        // the explicit type-args, then substitute each impl-level generic
        // param with its concrete arg in the sig before validating call args.
        // (Sub-item 5B of `phase-4-interpreter.md` § method resolution;
        // canonical entry at `phase-2-parser-ast.md` § "Path expression with
        // generic args — concrete-type UFCS support".)
        if let ExprKind::Path {
            segments,
            generic_args: Some(generic_args),
        } = &object.kind
        {
            if segments.len() == 1 {
                let type_name = segments[0].clone();
                // Concrete-type UFCS at slice 1b widens generic_args to
                // `Vec<GenericArg>`; the dispatch surface still consumes
                // type args only — const-arg binding for UFCS calls
                // lands when slice 3's call-site solver threads the
                // substitution through. Const-args at this position are
                // ignored for dispatch but still parsed so the shape
                // round-trips cleanly.
                let target_args: Vec<Type> = generic_args
                    .iter()
                    .filter_map(|a| match a {
                        GenericArg::Type(t) => Some(self.lower_type_expr(t, &[])),
                        GenericArg::Const(_) => None,
                        // Shape args are ignored for dispatch (Dim/Shape
                        // kind system lands in Phase 11 Q1).
                        GenericArg::Shape(_) => None,
                    })
                    .collect();
                self.method_callee_types.insert(
                    SpanKey::from_span(span),
                    format!("{}.{}", type_name, method),
                );
                let candidates: Vec<(ImplInfo, FunctionSig)> = self
                    .env
                    .find_methods_with_args(&type_name, &target_args, method)
                    .into_iter()
                    .map(|(imp, sig)| (imp.clone(), sig.clone()))
                    .collect();
                // Slice 5C of the method-resolution CR — see
                // `phase-4-interpreter.md` § method-resolution sub-item 5C.
                // `find_methods_with_args` already applies the inherent-
                // beats-trait priority partition + bounds-discharge filter
                // (slices 1 + 3); a length-≥2 result here means multiple
                // candidates of the same priority tier survived. The
                // user must pick a specific UFCS form (`TraitName.method(...)`)
                // to disambiguate. Mirrors slice 3's receiver-form
                // `AmbiguousMethod` (E0239) but uses `AmbiguousAssocFn`
                // (E0233) to match slice 3.5 and slice 5A's framing —
                // type-prefixed dispatch is the natural disambiguation
                // form for UFCS.
                if candidates.len() > 1 {
                    let receiver_display = if target_args.is_empty() {
                        type_name.clone()
                    } else {
                        format!(
                            "{}[{}]",
                            type_name,
                            target_args
                                .iter()
                                .map(type_display)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let candidate_lines: Vec<String> = candidates
                        .iter()
                        .map(|(imp, sig)| {
                            let dispatcher = imp
                                .trait_name
                                .clone()
                                .unwrap_or_else(|| imp.target_type.clone());
                            let subs: HashMap<String, SubstValue> = imp
                                .generic_params
                                .as_ref()
                                .map(|gp| {
                                    gp.params
                                        .iter()
                                        .zip(target_args.iter())
                                        .map(|(p, t)| (p.name.clone(), SubstValue::Type(t.clone())))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let params_display = sig
                                .params
                                .iter()
                                .map(|p| type_display(&substitute_type_params(p, &subs)))
                                .collect::<Vec<_>>()
                                .join(", ");
                            let return_display =
                                type_display(&substitute_type_params(&sig.return_type, &subs));
                            format!(
                                "    `{}.{}({})` -> {}",
                                dispatcher, method, params_display, return_display,
                            )
                        })
                        .collect();
                    self.type_error(
                        format!(
                            "ambiguous method '{}' on `{}`: \
                             multiple candidates apply. Use UFCS to disambiguate:\n{}",
                            method,
                            receiver_display,
                            candidate_lines.join("\n"),
                        ),
                        *span,
                        TypeErrorKind::AmbiguousAssocFn,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Some(Type::Error);
                }
                if let Some((imp, sig)) = candidates.first() {
                    let subs: HashMap<String, SubstValue> = imp
                        .generic_params
                        .as_ref()
                        .map(|gp| {
                            gp.params
                                .iter()
                                .zip(target_args.iter())
                                .map(|(p, t)| (p.name.clone(), SubstValue::Type(t.clone())))
                                .collect()
                        })
                        .unwrap_or_default();
                    let param_types: Vec<Type> = sig
                        .params
                        .iter()
                        .map(|p| substitute_type_params(p, &subs))
                        .collect();
                    let return_ty = substitute_type_params(&sig.return_type, &subs);
                    if args.len() != param_types.len() {
                        self.type_error(
                            format!(
                                "method '{}' expects {} argument(s), found {}",
                                method,
                                param_types.len(),
                                args.len()
                            ),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return Some(return_ty);
                    }
                    for (arg, param) in args.iter().zip(param_types.iter()) {
                        let arg_ty = self.infer_expr(&arg.value);
                        self.check_assignable(param, &arg_ty, arg.value.span);
                    }
                    return Some(return_ty);
                }
                // No matching impl-table entry. Built-in types (Vec, Option,
                // etc.) whose methods dispatch through special-case infer
                // paths rather than `env.impls` are out of scope for this
                // slice; falling through to a focused diagnostic.
                self.type_error(
                    format!("no method '{}' on `{}[…]`", method, type_name),
                    *span,
                    TypeErrorKind::NoMethodFound,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Some(Type::Error);
            }
        }
        None
    }
}
