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
use super::types::{type_display, IntSize, SubstValue, Type, UIntSize};
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
                        self.warn_partial_move_of_drop_struct(&arg.value, param_ty);
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
                // B-2026-08-30-40 — the canonical primitive list, not a
                // longhand copy of it. This spelling stopped at `f32`/`f64` and
                // omitted `i128`, `u128`, `f16` and `bf16`, so a path call on
                // any of those four was never recognized as a TYPE receiver at
                // all: `f16.from(x)`, `f16.parse(s)` and `i128.parse(s)` fell
                // through to the identifier-in-value-position diagnostic and
                // were rejected with "'f16' is a type, not a function — a
                // numeric conversion is the cast `x as f16`", advice that has
                // nothing to do with what was written. The same call at `f32`
                // RESOLVES and then fails on its own merits ("no associated
                // function 'parse' on type 'f32'"), which is the contrast that
                // makes this a namespace gap rather than a missing member.
                //
                // `PRELUDE_PRIMITIVES` is the list every other module reads
                // (`is_known_type_name` among them) and it carries all four, so
                // taking it directly both fixes the omission and stops this copy
                // drifting again — the same recurrence B-2026-08-30-25 records
                // for the width list in `method_callee_type_name`.
                || crate::prelude::PRELUDE_PRIMITIVES.contains(&type_name.as_str());
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
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                            | "isize"
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
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                            | "isize"
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
                // `<C-like #[repr(intN)] enum>.try_from(v) ->
                // Result[Enum, DiscriminantError[intN]]` — design.md § Enum
                // Discriminant Runtime Surface (B-2026-08-21-26). The inbound
                // twin of `.discriminant()`: the same folded table read
                // backwards, value -> variant.
                //
                // Placed before the general associated-call lookup so the
                // generated conversion cannot be shadowed, matching where
                // `.discriminant()` sits on the instance chain. The primitive
                // `try_from` arms above key on primitive `type_name`s and the
                // `char` arm on `char`, so none of them can collide with an
                // enum name here.
                if method == "try_from" && self.env.enums.contains_key(type_name) {
                    return Some(self.infer_enum_try_from(type_name, args, span));
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
                        self.warn_partial_move_of_drop_struct(&arg.value, param);
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
                // B-2026-08-30-40 — the same canonical list as the gate
                // above, MINUS `String`. This is the arm that turns a
                // recognized type receiver with no such member into the clean
                // "no associated function 'X' on type 'T'" rejection, and the
                // comment above it says why `String` is excluded: struct / enum
                // / `String` receivers keep the fall-through because they may
                // still resolve downstream. `PRELUDE_PRIMITIVES` includes
                // `String`, so the exclusion is spelled out rather than left to
                // the reader to notice.
                //
                // Without the widening the four reduced-precision / 128-bit
                // widths never reached here either, so even once the gate above
                // admits them they would fall through to a worse message. The
                // two sites have to move together.
                if crate::prelude::PRELUDE_PRIMITIVES.contains(&type_name.as_str())
                    && type_name != "String"
                {
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
    /// Type of `<enum>.try_from(v)` — design.md § Enum Discriminant Runtime
    /// Surface's auto-generated `impl TryFrom[intN] for Foo`
    /// (B-2026-08-21-26).
    ///
    /// The spec grants this to C-like enums with a DECLARED `#[repr(intN)]`
    /// and to no others, so there are two distinct refusals rather than one
    /// generic "no method", and each names the reason the spec gives:
    ///
    ///   * a PAYLOAD enum is absent from the discriminant table by
    ///     construction (`record_enum_discriminant_surface` skips it), because
    ///     the compiler may elide or move its tag — same v1 restriction that
    ///     excludes it from `.discriminant()`;
    ///   * a C-like enum with NO `#[repr]` is in the table (its variants take
    ///     declaration positions) but `repr_declared` is false. `.discriminant()`
    ///     is still granted there — reading a local property is fine — but the
    ///     INBOUND direction is not, because the compiler-chosen discriminant
    ///     space is not stable across versions, so a value that maps today may
    ///     not tomorrow. That asymmetry is the spec's, and it is why the table
    ///     records `repr_declared` at all.
    pub(super) fn infer_enum_try_from(
        &mut self,
        type_name: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let Some(disc) = self.enum_discriminants.get(type_name).cloned() else {
            for arg in args {
                self.infer_expr(&arg.value);
            }
            self.type_error(
                format!(
                    "'{type_name}' has payload-carrying variants, and `try_from` is generated \
                     for C-like (payload-free) enums only — the compiler may elide, reorder or \
                     move a payload enum's tag, so there is no stable integer to convert from. \
                     Write an explicit `fn from_tag(v: u8) -> Option[{type_name}]` with a \
                     `match` instead"
                ),
                *span,
                TypeErrorKind::NoMethodFound,
            );
            return Type::Error;
        };
        if !disc.repr_declared {
            for arg in args {
                self.infer_expr(&arg.value);
            }
            self.type_error(
                format!(
                    "'{type_name}' has no `#[repr(intN)]`, and `try_from` is generated only for \
                     enums that declare one — without it the discriminant values are chosen by \
                     the compiler and may shift between versions, so converting an integer back \
                     to a variant would be reading a number nobody promised. Add \
                     `#[repr(u8)]` (or the width you mean) to '{type_name}', or write an \
                     explicit `match`. `.discriminant()` is still available: reading the current \
                     value is a local property, but round-tripping through it is not"
                ),
                *span,
                TypeErrorKind::NoMethodFound,
            );
            return Type::Error;
        }
        if args.len() != 1 {
            self.type_error(
                format!(
                    "{type_name}.try_from expects 1 argument, got {}",
                    args.len()
                ),
                *span,
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        let repr_ty = Self::int_type_named(&disc.repr);
        let arg_ty = self.infer_expr(&args[0].value);
        if arg_ty == Type::Error {
            return Type::Error;
        }
        // The argument must be the repr type EXACTLY, and this is checked here
        // rather than left to `check_assignable`, which admits the mismatch
        // silently. A narrower integer would widen and a wider one would
        // truncate, and either turns "is this a declared variant?" into a
        // question about a different value than the caller asked about — the
        // one thing a conversion whose entire job is range-checking must not
        // do quietly.
        let arg_bare = Self::peel_refs_for_try_from(&arg_ty);
        if arg_bare != repr_ty {
            self.type_error(
                format!(
                    "`{type_name}.try_from` expects its `#[repr]` type `{}`, got `{}` — an \
                     implicit conversion here would range-check a DIFFERENT value than the one \
                     passed. Write `{} as {}` if that is what you mean",
                    disc.repr,
                    type_display(&arg_ty),
                    type_display(&arg_ty),
                    disc.repr,
                ),
                args[0].value.span,
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }
        Type::Named {
            name: "Result".to_string(),
            args: vec![
                Type::Named {
                    name: type_name.to_string(),
                    args: Vec::new(),
                },
                Type::Named {
                    name: "DiscriminantError".to_string(),
                    args: vec![repr_ty],
                },
            ],
        }
    }

    /// `ref`/`mut ref` peeled off an argument type, so a borrowed integer is
    /// compared at its bare width rather than rejected for its binding mode.
    fn peel_refs_for_try_from(t: &Type) -> Type {
        match t {
            Type::Ref(inner) | Type::MutRef(inner) => Self::peel_refs_for_try_from(inner),
            other => other.clone(),
        }
    }

    /// The `Type` for a repr head name from the discriminant table. The table
    /// only ever holds the eight integer heads plus the conservative `u32`
    /// default, so the fallback is unreachable rather than lossy.
    fn int_type_named(repr: &str) -> Type {
        match repr {
            "i8" => Type::Int(IntSize::I8),
            "i16" => Type::Int(IntSize::I16),
            "i32" => Type::Int(IntSize::I32),
            "i64" => Type::Int(IntSize::I64),
            "u8" => Type::UInt(UIntSize::U8),
            "u16" => Type::UInt(UIntSize::U16),
            "u64" => Type::UInt(UIntSize::U64),
            _ => Type::UInt(UIntSize::U32),
        }
    }

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
                        self.warn_partial_move_of_drop_struct(&arg.value, param);
                    }
                    return Some(return_ty);
                }
                // No matching impl-table entry. A BUILT-IN container's
                // associated functions do not live in `env.impls` at all: the
                // unqualified `Vec.new()` parses as a CALL over a two-segment
                // path (`Path(["Vec", "new"])`) and is answered by
                // `infer_call`'s constructor arm, which hands back
                // `Vec[<fresh type var>]` and lets inference settle the
                // element. The qualified `Vec[i64].new()` parses as a
                // METHOD CALL whose receiver is a one-segment path carrying
                // generic args, so it never reached that arm and died here
                // instead (B-2026-08-22-17).
                //
                // That was harmless while the qualified form was a niche
                // spelling. B-2026-08-21-53 made it THE documented way to pin a
                // type, at which point the spec was telling users to write a
                // form that failed on `Vec`, `Map` and `Channel` — the types
                // most likely to need pinning, since their element cannot
                // always be inferred from context.
                //
                // So delegate rather than re-implement: rebuild the call in the
                // two-segment form the builtin arm already understands, infer
                // it, and then PIN the result's inferred args to the ones the
                // user wrote by unifying against the receiver's own type. That
                // keeps one implementation of every builtin constructor
                // (`new`, `with_capacity`, the fallible `try_*` companions,
                // `Channel`'s tuple return) instead of a second copy that
                // would drift from the first.
                if is_builtin_container_head(&type_name) {
                    let delegated_callee = Expr {
                        kind: ExprKind::Path {
                            segments: vec![type_name.clone(), method.to_string()],
                            generic_args: None,
                        },
                        span: object.span,
                    };
                    let ret = self.infer_call(&delegated_callee, args, span);
                    if ret == Type::Error {
                        // The builtin arm already reported against its own
                        // name; adding a second diagnostic for the same call
                        // would only be noise.
                        return Some(Type::Error);
                    }
                    // Pin the explicit args by UNIFYING, not by checking. The
                    // constructor hands back `Vec[?T0]`, and binding `?T0` to
                    // the `i64` the user wrote is the entire point of the
                    // qualified spelling — `check_assignable` would instead
                    // report `expected 'Vec[i64]', found 'Vec[?T0]'`, which is
                    // exactly the metavar it is supposed to be solving.
                    //
                    // A pin the constructor cannot satisfy (`String[i64].new()`,
                    // or the wrong arity) fails unification and is reported
                    // here, so a nonsense qualification is still rejected.
                    if !target_args.is_empty() {
                        let pinned = Type::Named {
                            name: type_name.clone(),
                            args: target_args.clone(),
                        };
                        // `Channel[i64].new()` is why this is two-step. Most
                        // builtin constructors return a value NAMED after the
                        // head (`Vec.new() -> Vec[?T]`), so unifying
                        // `Head[args]` against the result binds the metavars
                        // directly. `Channel.new()` does not: it returns
                        // `(Sender[?T], Receiver[?T])`, where the head name
                        // appears nowhere and the pinned element sits inside a
                        // tuple. Unifying against `Channel[i64]` there reports a
                        // mismatch on a call that is perfectly well-formed.
                        //
                        // So fall back to pairing the explicit args with the
                        // result's own free metavars in order. One distinct
                        // metavar shared by `Sender` and `Receiver` pairs with
                        // the single `i64`, and binding it once fixes both —
                        // which is exactly the meaning of `Channel[i64]`.
                        let head_named = matches!(
                            &ret,
                            Type::Named { name, .. } if *name == type_name
                        );
                        let pinned_ok = if head_named {
                            crate::typechecker::inference::unify_types(
                                &pinned,
                                &ret,
                                &mut self.env.substitutions,
                                &mut self.env.const_substitutions,
                            )
                        } else {
                            let vars = ordered_free_type_vars(&ret);
                            vars.len() == target_args.len()
                                && vars.iter().zip(target_args.iter()).all(|(v, t)| {
                                    crate::typechecker::inference::unify_types(
                                        v,
                                        t,
                                        &mut self.env.substitutions,
                                        &mut self.env.const_substitutions,
                                    )
                                })
                        };
                        if !pinned_ok {
                            self.type_error(
                                format!(
                                    "cannot construct `{}` as `{}`",
                                    type_display(&ret),
                                    type_display(&pinned)
                                ),
                                *span,
                                TypeErrorKind::TypeMismatch,
                            );
                            return Some(Type::Error);
                        }
                        // The expression's type is the CONSTRUCTOR's result, not
                        // the receiver spelling: `Channel[i64].new()` evaluates
                        // to `(Sender[i64], Receiver[i64])`, and handing back
                        // `Channel[i64]` would make the tuple destructuring
                        // below it fail. `pinned` is returned only when the head
                        // genuinely names the result type.
                        if head_named {
                            return Some(pinned);
                        }
                        // The name maps only supply display names for vars
                        // that stay UNRESOLVED; these came from
                        // `env.fresh_type_var()` and were just bound, so there
                        // are no names to carry and empty maps are correct.
                        return Some(crate::typechecker::inference::resolve_type_vars(
                            &ret,
                            &self.env.substitutions,
                            &HashMap::new(),
                            &self.env.const_substitutions,
                            &HashMap::new(),
                        ));
                    }
                    return Some(ret);
                }
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

/// Built-in container heads whose associated functions are answered by
/// `infer_call`'s constructor arms rather than by an `env.impls` entry, so a
/// qualified `Head[Args].method(..)` call has to be routed there instead of
/// resolved against the impl table (B-2026-08-22-17).
///
/// Kept as an explicit list rather than "try the delegation and see": a failed
/// delegation emits its own diagnostic, so speculatively delegating an unknown
/// head would report against a name the user did not write before this arm
/// could produce its own focused message.
fn is_builtin_container_head(name: &str) -> bool {
    matches!(
        name,
        "Vec"
            | "VecDeque"
            | "Map"
            | "SortedMap"
            | "Set"
            | "SortedSet"
            | "String"
            | "Channel"
            | "Option"
            | "Result"
    )
}

/// Free metavars of `ty` in first-appearance order, deduplicated.
///
/// Used to pin a qualified builtin constructor whose result is not named after
/// the head — `Channel.new() -> (Sender[?T], Receiver[?T])`. Order and dedup
/// both matter: `Channel[i64]` supplies ONE argument for ONE distinct metavar
/// that occurs twice, and pairing positionally without dedup would see two.
fn ordered_free_type_vars(ty: &Type) -> Vec<Type> {
    fn walk(t: &Type, out: &mut Vec<Type>) {
        match t {
            Type::TypeVar(id) => {
                if !out.iter().any(|s| matches!(s, Type::TypeVar(i) if i == id)) {
                    out.push(t.clone());
                }
            }
            Type::Named { args, .. } => args.iter().for_each(|a| walk(a, out)),
            Type::Ref(i) | Type::MutRef(i) => walk(i, out),
            Type::Tuple(ts) => ts.iter().for_each(|t| walk(t, out)),
            Type::Array { element, .. } | Type::Slice { element, .. } => walk(element, out),
            Type::Function {
                params,
                return_type,
            }
            | Type::OnceFunction {
                params,
                return_type,
            } => {
                params.iter().for_each(|p| walk(p, out));
                walk(return_type, out);
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ty, &mut out);
    out
}
