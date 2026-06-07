//! String / slice method-inference dispatch.
//!
//! Houses sort-comparator and sort-key-closure validation plus the
//! per-method return-type synthesizers for `String` and `Slice[T]`
//! (read-only and mutable surfaces both).

use crate::ast::*;
use crate::token::Span;

use super::types::{type_display, IntSize, Type, UIntSize};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Validate a `sort_by` / `sorted_by` comparator argument against the
    /// `Fn(elem, elem) -> Ordering` shape. Pushes the expected function
    /// type down into the closure via `check_expr` so closure-parameter
    /// types are inferred from the element type rather than left as fresh
    /// metavars (today's silent-fall-through path) — a wrong-shape
    /// comparator (`xs.sort_by(|a| a)`, `xs.sort_by(|a, b| a)`, or a
    /// `Fn` value of the wrong arity / return type) now produces a
    /// TypeMismatch at the closure expression instead of runtime-panicking
    /// when the interpreter invokes it with two args / consumes the
    /// non-Ordering return.
    pub(super) fn check_sort_comparator(
        &mut self,
        elem: &Type,
        arg: &CallArg,
        method: &str,
        span: &Span,
    ) {
        let expected = Type::Function {
            params: vec![elem.clone(), elem.clone()],
            return_type: Box::new(Type::Named {
                name: "Ordering".to_string(),
                args: Vec::new(),
            }),
        };
        let _ = (method, span); // method / span carried for future diagnostic refinement
        self.check_expr(&arg.value, &expected);
    }

    /// Validate a `sort_by_key` / `sorted_by_key` key-function argument
    /// against `Fn(elem) -> K` and verify the inferred `K` satisfies `Ord`.
    /// `K` is a fresh metavar pushed down through `check_expr`; once the
    /// closure body unifies it to a concrete type, an Ord bound check
    /// rejects key types (raw floats, function values, etc.) that lack
    /// total ordering. Generic `K` (still a TypeVar after resolution)
    /// flows through without an Ord assertion — the bound will be
    /// rechecked at monomorphization.
    pub(super) fn check_sort_key_closure(
        &mut self,
        elem: &Type,
        arg: &CallArg,
        method: &str,
        span: &Span,
    ) {
        // `Fn(elem) -> K` where K is a placeholder the closure body solves.
        // Use `Type::TypeParam` not `Type::TypeVar`: `types_compatible` treats
        // TypeParam permissively so the `check_assignable` step doesn't fire
        // a spurious "expected K, found <body_ty>" diagnostic. After
        // `check_expr` returns the inferred closure type, read the resolved
        // body type out of the Function shape and check the Ord bound on it.
        // Pattern lifted from `Iterator.map`'s pushdown at infer_iterator_method.
        let placeholder = Type::TypeParam("__sort_by_key_K".to_string());
        let expected = Type::Function {
            params: vec![elem.clone()],
            return_type: Box::new(placeholder),
        };
        let actual_ty = self.check_expr(&arg.value, &expected);
        let resolved_k = match actual_ty {
            Type::Function { return_type, .. } | Type::OnceFunction { return_type, .. } => {
                *return_type
            }
            _ => return,
        };
        // Float keys are accepted for `sort_by_key` specifically, even
        // though `type_supports_ord` returns false for them (floats fail Eq
        // under standard IEEE 754 semantics — NaN ≠ NaN). The codegen
        // lowering dispatches float keys to a `karac_float_cmp` runtime
        // call that uses bit-level total-order semantics (the equivalent
        // of Rust's `f64::total_cmp` / `f32::total_cmp`: sign-flip the bit
        // pattern, compare as i64) — that gives a well-defined ordering
        // for every float including NaNs without forcing the typechecker
        // to widen `Ord` for other Ord consumers (derive checks,
        // SortedSet, etc.). Documented as a sort_by_key-scoped concession
        // in docs/implementation_checklist/phase-7-codegen.md.
        let key_is_float = matches!(resolved_k, Type::Float(_));
        if !matches!(
            resolved_k,
            Type::TypeParam(_) | Type::TypeVar(_) | Type::Error
        ) && !key_is_float
            && !self.type_supports_ord(&resolved_k)
        {
            self.type_error(
                format!(
                    "{}: key closure return type '{}' does not implement Ord",
                    method,
                    type_display(&resolved_k)
                ),
                span.clone(),
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
    }

    /// Infer the return type of a method call on `String` (`Type::Str`).
    /// Called from `infer_method_call` when the object type is `Type::Str`.
    pub(super) fn infer_str_method(&mut self, method: &str, args: &[CallArg], span: &Span) -> Type {
        match method {
            // Length / emptiness predicates — runtime ships these and the
            // interpreter dispatches them; the typechecker enumeration was
            // catching up per the source comment below. Surfaced 2026-05-22
            // when the `resolve_path_type` rejection of unknown
            // `Type.method(...)` calls made the silent `Type::Error`
            // propagation from `String.from(...)` stop short-circuiting
            // these (downstream `s.len()` started hitting `require_known_method`
            // instead of inheriting Type::Error). Wired here so they pass
            // typecheck cleanly without lint noise.
            "len" => {
                if !args.is_empty() {
                    self.type_error(
                        "'len' takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "'is_empty' takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            "contains" => {
                // contains(substr: String) -> bool — runtime ships substring
                // search; the typechecker just enforces the arg shape.
                if args.len() != 1 {
                    self.type_error(
                        format!("'contains' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !matches!(arg_ty, Type::Str | Type::Error) {
                        self.type_error(
                            format!(
                                "'contains' expects a String substring, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Bool
            }
            "sorted" => {
                if !args.is_empty() {
                    self.type_error(
                        "'sorted' takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Str
            }
            "sorted_by" => {
                // sorted_by(cmp: Fn(Char, Char) -> Ordering) -> String
                if args.len() != 1 {
                    self.type_error(
                        format!("'sorted_by' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    self.check_sort_comparator(&Type::Char, &args[0], "sorted_by", span);
                }
                Type::Str
            }
            "chars" => {
                // chars() -> Iterator[char]. Peer of design.md § Character type
                // (line 2299): `for c in s` and `s.chars()` both iterate the
                // string's Unicode scalar values. Tree-walk interpreter
                // implements the same in eval_method_call's "chars" arm; a
                // for-loop on a bare String falls back through the same path.
                if !args.is_empty() {
                    self.type_error(
                        "'chars' takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Named {
                    name: "Iterator".to_string(),
                    args: vec![Type::Char],
                }
            }
            "bytes" => {
                // bytes() -> Slice[u8]. design.md § Character type:
                // `s[i]` is rejected with a help suggesting
                // `s.bytes()[i]` for O(1) byte-positional access, vs
                // `s.char_at(i)` for the O(n) Unicode-aware form.
                // Zero-copy view over the String's UTF-8 storage —
                // String's runtime layout is `{ptr, len, cap}`, so a
                // `Slice[u8]` is just the first two fields. Used by
                // ASCII-input katas (atoi #8) to drop the O(n)
                // `Vec[char]` snapshot pattern.
                if !args.is_empty() {
                    self.type_error(
                        "'bytes' takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Slice {
                    element: Box::new(Type::UInt(UIntSize::U8)),
                    mutable: false,
                }
            }
            "starts_with" => {
                // starts_with(prefix: String) -> bool. Returns true iff
                // the receiver's bytes begin with prefix's bytes.
                if args.len() != 1 {
                    self.type_error(
                        format!("'starts_with' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !matches!(arg_ty, Type::Str | Type::Error) {
                        self.type_error(
                            format!(
                                "'starts_with' expects a String prefix, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Bool
            }
            "substring" => {
                // substring(start: i64) -> String. Returns a fresh owned
                // String of the receiver's bytes from byte offset `start`
                // to the end. Out-of-range / negative starts saturate to
                // an empty String (route-prefix-friendly).
                if args.len() != 1 {
                    self.type_error(
                        format!("'substring' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !matches!(arg_ty, Type::Int(_) | Type::Error) {
                        self.type_error(
                            format!(
                                "'substring' expects an integer start index, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Str
            }
            "push_str" => {
                // push_str(other: String) -> (). Mutating append; receiver
                // must be a mutable binding (ownership.rs classifies this
                // as MutRef so the let-mut / mut-ref check fires there).
                // Codegen lives in src/codegen/vec_method.rs (`push_str` arm) —
                // the typechecker arm only validates the arg shape and
                // surfaces the unit return type.
                if args.len() != 1 {
                    self.type_error(
                        format!("'push_str' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !matches!(arg_ty, Type::Str | Type::Error) {
                        self.type_error(
                            format!(
                                "'push_str' expects a String argument, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Unit
            }
            "push" => {
                // push(c: char) -> (). Mutating append of a single Unicode
                // scalar value, UTF-8 encoded into the receiver's byte
                // buffer (1–4 bytes per call). Peer of `push_str` and
                // analog of Rust's `String::push`. Surfaced 2026-05-25
                // by kata-katas/leetcode/71-simplify-path, whose natural
                // shape is per-output-char append — using `f"{out}{c}"`
                // self-append was O(n²); push(c) is amortized O(1) per
                // call.
                if args.len() != 1 {
                    self.type_error(
                        format!("'push' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !matches!(arg_ty, Type::Char | Type::Error) {
                        self.type_error(
                            format!(
                                "'push' expects a Char argument, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Unit
            }
            // Unknown string method — typo-suggestion diagnostic if close to
            // a known name. `len` / `is_empty` / `contains` joined the
            // enumerated list 2026-05-22; `push_str` joined 2026-05-23;
            // `push` joined 2026-05-25 (kata 71 follow-up).
            // Further runtime-only surface (e.g. `to_uppercase`, `split`)
            // still falls through to the typo-suggestion path until
            // per-method typechecker arms land — design.md § Method
            // Resolution Step 7.
            _ => self.require_known_method(
                "String",
                method,
                &[
                    "bytes",
                    "chars",
                    "contains",
                    "is_empty",
                    "len",
                    "push",
                    "push_str",
                    "sorted",
                    "sorted_by",
                    "starts_with",
                    "substring",
                ],
                args,
                span,
            ),
        }
    }

    /// Infer the return type of a method call on `CStr` (receiver `ref CStr`
    /// — the type of a `c"..."` literal). The borrowed surface per
    /// design.md § C-String Literals: `as_ptr` is the language's first safe
    /// pointer-producer (`*const u8` into the literal's rodata bytes), and
    /// the introspection trio (`len` / `is_empty` / `as_bytes`) reports the
    /// source bytes excluding the trailing NUL. The owning `CString` type
    /// and the `to_string` / `to_string_slice` conversions are the
    /// remaining Phase-8 surface (tracked in phase-8-stdlib-floor.md).
    pub(super) fn infer_cstr_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let require_no_args = |s: &mut Self, name: &str| {
            if !args.is_empty() {
                s.type_error(
                    format!("'{}' takes no arguments", name),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
            }
        };
        match method {
            "as_ptr" => {
                require_no_args(self, "as_ptr");
                Type::Pointer {
                    is_mut: false,
                    inner: Box::new(Type::UInt(UIntSize::U8)),
                }
            }
            "len" => {
                require_no_args(self, "len");
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                require_no_args(self, "is_empty");
                Type::Bool
            }
            "as_bytes" => {
                require_no_args(self, "as_bytes");
                Type::Slice {
                    element: Box::new(Type::UInt(UIntSize::U8)),
                    mutable: false,
                }
            }
            _ => self.require_known_method(
                "CStr",
                method,
                &["as_bytes", "as_ptr", "is_empty", "len"],
                args,
                span,
            ),
        }
    }

    /// Infer the return type of a method call on a `Slice[T]` or `mut Slice[T]`.
    /// Handles the full read-only surface and the mutation-only surface for
    /// `mut Slice[T]`. Called from `infer_method_call` when the object type is
    /// `Type::Slice`.
    pub(super) fn infer_slice_method(
        &mut self,
        element: &Type,
        mutable: bool,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let elem = element.clone();
        let option_elem = Type::Named {
            name: "Option".to_string(),
            args: vec![elem.clone()],
        };
        let option_i64 = Type::Named {
            name: "Option".to_string(),
            args: vec![Type::Int(IntSize::I64)],
        };
        let slice_elem = Type::Slice {
            element: Box::new(elem.clone()),
            mutable: false,
        };
        let vec_slice = Type::Named {
            name: "Vec".to_string(),
            args: vec![slice_elem.clone()],
        };

        match method {
            // Read-only methods (available on both Slice[T] and mut Slice[T])
            "len" => {
                if !args.is_empty() {
                    self.type_error(
                        "Slice.len() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "Slice.is_empty() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            "first" | "last" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("Slice.{}() takes no arguments", method),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                option_elem
            }
            "get" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                option_elem
            }
            // `Slice[T].get_unchecked(i: i64) -> T` — unsafe direct-index read,
            // returns `T` by value (no `Option` wrap, no bounds check). The
            // escape hatch for hot scanners (e.g. KMP `needle[j]`, where `j`
            // rewinds via the LPS table — provably in-range but not
            // compiler-provable). UB on out-of-range; must be called in an
            // `unsafe` block — enforced via the `("Slice", "get_unchecked")`
            // seed in `unsafe_lint::build_unsafe_fn_registry`. Mirrors
            // `Vec.get_unchecked`. See phase-7-codegen.md § BCE table-range tier.
            "get_unchecked" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                elem
            }
            "contains" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "binary_search" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                option_i64
            }
            "split_at" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                Type::Tuple(vec![slice_elem.clone(), slice_elem])
            }
            "chunks" | "windows" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                vec_slice
            }
            // Mutation methods (require mut Slice[T])
            "sort" | "reverse" => {
                if !mutable {
                    self.type_error(
                        format!(
                            "Slice.{}() requires a mutable slice (`mut Slice[T]`)",
                            method
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                if !args.is_empty() {
                    self.type_error(
                        format!("Slice.{}() takes no arguments", method),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Unit
            }
            "sort_by" => {
                if !mutable {
                    self.type_error(
                        "Slice.sort_by() requires a mutable slice (`mut Slice[T]`)".to_string(),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Slice.sort_by() expects 1 argument (comparator closure), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    self.check_sort_comparator(&elem, &args[0], "sort_by", span);
                }
                Type::Unit
            }
            "sort_by_key" => {
                if !mutable {
                    self.type_error(
                        "Slice.sort_by_key() requires a mutable slice (`mut Slice[T]`)".to_string(),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Slice.sort_by_key() expects 1 argument (key closure), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    self.check_sort_key_closure(&elem, &args[0], "sort_by_key", span);
                }
                Type::Unit
            }
            "fill" => {
                if !mutable {
                    self.type_error(
                        "Slice.fill() requires a mutable slice (`mut Slice[T]`)".to_string(),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Unit
            }
            "swap" => {
                if !mutable {
                    self.type_error(
                        "Slice.swap() requires a mutable slice (`mut Slice[T]`)".to_string(),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                Type::Unit
            }
            // `Slice[T]` IS `Iterator[T]` — `.iter()` / `.into_iter()` route
            // through the same Iterator dispatch as `Vec.iter()` so chained
            // adaptors (`s.iter().map(f).filter(p).collect()`) compose. The
            // receiver-type match in `infer_method_call` lands here before
            // the generic `iter` / `into_iter` arm, so the registration
            // duplicates that arm shape (no-args, returns `Iterator[T]`).
            "iter" | "into_iter" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("Slice.{}() takes no arguments", method),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                }
                Type::Named {
                    name: "Iterator".to_string(),
                    args: vec![elem],
                }
            }
            _ => self.require_known_method(
                "Slice",
                method,
                &[
                    "binary_search",
                    "chunks",
                    "contains",
                    "fill",
                    "first",
                    "get",
                    "into_iter",
                    "is_empty",
                    "iter",
                    "last",
                    "len",
                    "reverse",
                    "sort",
                    "sort_by",
                    "sort_by_key",
                    "split_at",
                    "swap",
                    "windows",
                ],
                args,
                span,
            ),
        }
    }

    /// `Vec[T]` / `VecDeque[T]` read-accessor and in-place-mutator method
    /// dispatch — the seq-surface counterpart to `infer_slice_method`.
    ///
    /// `Vec` carries no stdlib impl block, so any method not caught by the
    /// scattered intercepts in `infer_method_call` (`push` / `pop` /
    /// `remove` / `iter` / `clone` / `sort_by` / …) fell through to the
    /// bottom-of-function `Type::Error` silent-prelude path. For read
    /// accessors that *return a used value* (`len`, `get`, `first`, …) that
    /// poison `Error` is universally `check_assignable`-compatible, so e.g.
    /// `let s: String = v.len()` typechecked clean and only failed (or
    /// silently misbehaved) downstream — a real soundness hole. This routes
    /// those methods to their true return types, mirroring the `Slice[T]`
    /// surface so `Vec` and `Slice` type identically.
    ///
    /// Returns `None` for any method this dispatcher doesn't own, so the
    /// caller falls through to the generic impl-search / prelude path
    /// unchanged (preserving Vec's partially-implicit method surface — a
    /// user trait impl on `Vec[T]`, a typo, etc.). Mutability for the
    /// in-place mutators (`sort` / `reverse` / `fill` / `swap`) is enforced
    /// at the binding layer, not here — same rule the `sort_by` intercept
    /// relies on — so there is no `mut`-gate, unlike `infer_slice_method`.
    pub(super) fn infer_vec_method(
        &mut self,
        element: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Type> {
        let elem = element.clone();
        let option_elem = Type::Named {
            name: "Option".to_string(),
            args: vec![elem.clone()],
        };
        let option_i64 = Type::Named {
            name: "Option".to_string(),
            args: vec![Type::Int(IntSize::I64)],
        };
        let slice_elem = Type::Slice {
            element: Box::new(elem.clone()),
            mutable: false,
        };
        let vec_slice = Type::Named {
            name: "Vec".to_string(),
            args: vec![slice_elem.clone()],
        };
        let vec_elem = Type::Named {
            name: "Vec".to_string(),
            args: vec![elem.clone()],
        };

        let result = match method {
            // ── Read accessors (return a used value) ──────────────
            "len" => {
                self.expect_no_args("Vec.len", args, span);
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                self.expect_no_args("Vec.is_empty", args, span);
                Type::Bool
            }
            "first" | "last" => {
                self.expect_no_args(&format!("Vec.{}", method), args, span);
                option_elem
            }
            "get" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                option_elem
            }
            "contains" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "binary_search" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                option_i64
            }
            "split_at" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                Type::Tuple(vec![slice_elem.clone(), slice_elem])
            }
            "chunks" | "windows" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                vec_slice
            }
            // ── In-place mutators (binding-layer mut-checked) ─────
            "sort" | "reverse" => {
                self.expect_no_args(&format!("Vec.{}", method), args, span);
                Type::Unit
            }
            "sorted" => {
                self.expect_no_args("Vec.sorted", args, span);
                vec_elem
            }
            "fill" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Unit
            }
            "swap" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&Type::Int(IntSize::I64), &at, arg.value.span.clone());
                }
                Type::Unit
            }
            // Any other method: fall through to the generic dispatch.
            _ => return None,
        };
        Some(result)
    }

    /// Emit a `WrongNumberOfArgs` diagnostic if `args` is non-empty, for a
    /// method that takes none. Shared by the no-arg arms of `infer_vec_method`.
    fn expect_no_args(&mut self, what: &str, args: &[CallArg], span: &Span) {
        if !args.is_empty() {
            self.type_error(
                format!("{}() takes no arguments", what),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
        }
    }
}
