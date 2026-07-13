//! String / slice method-inference dispatch.
//!
//! Houses sort-comparator and sort-key-closure validation plus the
//! per-method return-type synthesizers for `String` and `Slice[T]`
//! (read-only and mutable surfaces both).

use crate::ast::*;
use crate::token::Span;

use super::types::{type_display, IntSize, Type, UIntSize};
use super::TypeErrorKind;

/// A `String`-valued argument, possibly behind a `ref` / `mut ref` borrow.
///
/// String methods that read their argument's bytes (`push_str`, `contains`,
/// `starts_with`) accept an owned `String` *or* a borrow of one — the callee
/// only copies/scans the bytes, so there is no ownership reason to demand a
/// move. This is squarely the self-hosted lexer's shape: it appends borrowed
/// keyword/identifier text into an output buffer and prefix-checks against
/// borrowed source slices, so a bare `Type::Str`-only check rejected the
/// lexer's natural call sites (surfaced by kata-katas #722 remove-comments,
/// whose `buffer.push_str(name)` with `name: ref String` was rejected by
/// `karac build` while `karac run` only warned).
fn is_str_like(ty: &Type) -> bool {
    match ty {
        Type::Str | Type::Error => true,
        // `StringSlice` is a borrowed view over a `String`'s UTF-8 bytes
        // (same `{ptr,len,cap}` layout, `cap == 0`), so it is accepted
        // anywhere a String substring/separator argument is — `contains`,
        // `split`, `starts_with`, `find` (design.md § StringSlice).
        Type::Named { name, .. } if name == "StringSlice" => true,
        Type::Ref(inner) | Type::MutRef(inner) => is_str_like(inner),
        _ => false,
    }
}

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
            // `String.to_string()` / `.clone()` → owning copy (`String`). The
            // `Display` trait gives every type a `to_string`; on `String`
            // itself it is the identity copy. Both take no arguments.
            "to_string" | "clone" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("'{method}' takes no arguments"),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Str
            }
            // `String.to_cstring(ref self) -> Result[CString, NulError]`
            // (design.md § C-String Literals). Copies the receiver's bytes into
            // a fresh owning `CString` buffer with an appended trailing NUL —
            // unless the receiver carries an interior NUL byte, which C would
            // truncate at, in which case it is `Err(NulError.InteriorNul)`. The
            // outbound counterpart of `CStr.to_string()`; both cross the
            // UTF-8 ↔ C-bytes boundary explicitly (no coercion). Codegen lowers
            // it via `karac_runtime_string_to_cstring`; the interpreter scans for
            // an interior NUL and yields a `Value::CString`.
            "to_cstring" => {
                if !args.is_empty() {
                    self.type_error(
                        "'to_cstring' takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                // Record the `String.to_cstring` callee so codegen routes it
                // precisely (the same hardcoded-arm pattern as `CStr.<method>`),
                // rather than by method name alone — which would hijack a user
                // type's own `to_cstring` method.
                self.method_callee_types.insert(
                    crate::resolver::SpanKey::from_span(span),
                    "String.to_cstring".to_string(),
                );
                Type::Named {
                    name: "Result".to_string(),
                    args: vec![
                        Type::Named {
                            name: "CString".to_string(),
                            args: vec![],
                        },
                        Type::Named {
                            name: "NulError".to_string(),
                            args: vec![],
                        },
                    ],
                }
            }
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
            "char_count" => {
                // char_count() -> i64 — O(n) count of Unicode scalar values
                // (design.md § String), the Unicode-aware companion of `len()`'s
                // O(1) byte count. Runtime decodes + counts; interp uses Rust's
                // `chars().count()`.
                if !args.is_empty() {
                    self.type_error(
                        "'char_count' takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "char_at" => {
                // char_at(i: i64) -> Option[char] — the i-th Unicode scalar
                // value, O(n), `None` past the end (design.md § String:
                // `s[i]` is a compile error; the explicit method makes the O(n)
                // cost visible). The pair to `len()`/`bytes()`'s O(1) byte access.
                if args.len() != 1 {
                    self.type_error(
                        format!("'char_at' expects 1 argument, found {}", args.len()),
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
                                "'char_at' expects an integer index, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![Type::Char],
                }
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
                    if !is_str_like(&arg_ty) {
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
            "cmp" => {
                // cmp(other: String) -> Ordering — the total byte-lexicographic
                // order, i.e. the method form of the `<`/`>` operators (which
                // already lower through `karac_string_cmp`). `impl Ord for
                // String` IS registered in env_build, but String's method
                // dispatch is a closed list that never consulted the trait
                // impl, so `s.cmp(t)` hard-errored under `karac check`/`build`
                // while `karac run` merely warned-and-computed it — a run/check
                // divergence (bug-ledger B-2026-06-30-13). The interpreter and
                // codegen both compare via the same `karac_string_cmp` byte
                // order that `Vec[String].sort` / `binary_search` use, so the
                // method agrees with the operators and across backends.
                if args.len() != 1 {
                    self.type_error(
                        format!("'cmp' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !is_str_like(&arg_ty) {
                        self.type_error(
                            format!(
                                "'cmp' expects a String argument, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Named {
                    name: "Ordering".to_string(),
                    args: Vec::new(),
                }
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
            "split" => {
                // split(sep: String | char) -> Vec[String]. Splits the
                // receiver on every (non-overlapping) occurrence of the
                // separator and returns the pieces, including leading/trailing
                // empty pieces (Rust `str::split` semantics). An empty receiver
                // yields a single empty piece; the separator may be a one-char
                // literal (`line.split(',')`) or a String (`s.split("::")`).
                // Surfaced by examples/weave (CSV ETL) — the canonical
                // row-tokenizing primitive a real pipeline reaches for first;
                // word_count.kara already assumed it existed.
                if args.len() != 1 {
                    self.type_error(
                        format!("'split' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !is_str_like(&arg_ty) && !matches!(arg_ty, Type::Char | Type::Error) {
                        self.type_error(
                            format!(
                                "'split' expects a String or char separator, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Named {
                    name: "Vec".to_string(),
                    args: vec![Type::Str],
                }
            }
            "lines" => {
                // lines() -> Vec[String]. Splits the receiver into lines at
                // `\n`, stripping a trailing `\r` from each (so `\r\n` endings
                // are handled) and NOT emitting a final empty line for a
                // trailing newline — Rust `str::lines` semantics. No argument.
                self.expect_no_args("String.lines", args, span);
                Type::Named {
                    name: "Vec".to_string(),
                    args: vec![Type::Str],
                }
            }
            "split_whitespace" => {
                // split_whitespace() -> Vec[String]. Splits on runs of Unicode
                // whitespace with no leading/trailing/repeated empty pieces
                // (Rust `str::split_whitespace`). No argument.
                self.expect_no_args("String.split_whitespace", args, span);
                Type::Named {
                    name: "Vec".to_string(),
                    args: vec![Type::Str],
                }
            }
            "starts_with" | "ends_with" => {
                // starts_with(prefix: String) / ends_with(suffix: String) -> bool.
                // True iff the receiver's bytes begin / end with the argument's.
                if args.len() != 1 {
                    self.type_error(
                        format!("'{method}' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !is_str_like(&arg_ty) {
                        self.type_error(
                            format!(
                                "'{method}' expects a String argument, found '{}'",
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
                // substring(start: i64) -> String  — bytes from `start` to end.
                // substring(start: i64, end: i64) -> String — bytes in the
                // half-open byte range `[start, end)`. Out-of-range / negative
                // / inverted bounds saturate to an empty String. Both indices
                // are byte offsets (matching the `bytes()` view), so the
                // self-hosted lexer can extract `token_text` via
                // `source.substring(start, current)`.
                if args.len() != 1 && args.len() != 2 {
                    self.type_error(
                        format!("'substring' expects 1 or 2 arguments, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    for arg in args {
                        let arg_ty = self.infer_expr(&arg.value);
                        if !matches!(arg_ty, Type::Int(_) | Type::Error) {
                            self.type_error(
                                format!(
                                    "'substring' expects integer byte indices, found '{}'",
                                    type_display(&arg_ty)
                                ),
                                arg.value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                }
                Type::Str
            }
            "repeat" => {
                // repeat(n: i64) -> String — the receiver concatenated `n`
                // times (`"ab".repeat(3) == "ababab"`); `n <= 0` yields an
                // empty String. Analog of Rust's `str::repeat`. Surfaced by
                // kata-katas #394 decode-string, whose `k[encoded]` decode is a
                // repeat storm; the self-hosted lexer wants it for counted
                // constructs too. Allocating String→String, same arg shape as
                // `substring`'s indices.
                if args.len() != 1 {
                    self.type_error(
                        format!("'repeat' expects 1 argument, found {}", args.len()),
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
                                "'repeat' expects an integer count, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Str
            }
            "trim" | "trim_start" | "trim_end" | "to_lowercase" | "to_uppercase" => {
                // trim() -> String: a fresh owned copy with leading/trailing
                // Unicode whitespace removed (Rust `str::trim`, owned rather
                // than a borrowed view). trim_start()/trim_end() strip only the
                // leading / trailing whitespace. to_lowercase()/to_uppercase()
                // -> String: full Unicode case mapping (Rust
                // `str::to_{lower,upper}case`, which can change byte length —
                // `ß`→`SS`). All allocate a new String and match the
                // interpreter's Rust-stdlib semantics exactly (codegen routes
                // through `karac_string_{trim,trim_start,trim_end,to_lowercase,
                // to_uppercase}` so the two backends never diverge).
                if !args.is_empty() {
                    self.type_error(
                        format!("'{method}' takes no arguments, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                }
                Type::Str
            }
            "replace" => {
                // replace(from: String, to: String) -> String: every
                // non-overlapping occurrence of `from` replaced with `to`
                // (Rust `str::replace`). Allocating String→String. Both
                // arguments are String/str-like. (SIMD `Vector.replace` is a
                // distinct lane-replace handled on the vector receiver path.)
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "'replace' expects 2 arguments (from, to), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    for arg in args {
                        let arg_ty = self.infer_expr(&arg.value);
                        if !is_str_like(&arg_ty) && arg_ty != Type::Error {
                            self.type_error(
                                format!(
                                    "'replace' expects String arguments, found '{}'",
                                    type_display(&arg_ty)
                                ),
                                arg.value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
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
                    if !is_str_like(&arg_ty) {
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
            "find" => {
                // find(needle: String | char) -> Option[i64] — byte offset of
                // the first occurrence of `needle`, or `None`. The companion of
                // `slice` for the canonical `first_word` shape (design.md §
                // StringSlice). Same separator-arg shape as `split` (String or
                // one-char literal). Codegen + interp search the UTF-8 bytes.
                if args.len() != 1 {
                    self.type_error(
                        format!("'find' expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !is_str_like(&arg_ty) && !matches!(arg_ty, Type::Char | Type::Error) {
                        self.type_error(
                            format!(
                                "'find' expects a String or char needle, found '{}'",
                                type_display(&arg_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![Type::Int(IntSize::I64)],
                }
            }
            "slice" => {
                // slice(start: i64, end: i64) -> StringSlice — a zero-copy
                // borrowed view over the half-open byte range `[start, end)`
                // of the receiver, pointing into its buffer (no allocation).
                // The v1 `StringSlice` producer (design.md § StringSlice). The
                // returned view borrows from the receiver; source pinning
                // (ownership) ensures it can't outlive it. Distinct from
                // `substring`, which copies into an owned `String`.
                if args.len() != 2 {
                    self.type_error(
                        format!("'slice' expects 2 arguments, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    for arg in args {
                        let arg_ty = self.infer_expr(&arg.value);
                        if !matches!(arg_ty, Type::Int(_) | Type::Error) {
                            self.type_error(
                                format!(
                                    "'slice' expects integer byte indices, found '{}'",
                                    type_display(&arg_ty)
                                ),
                                arg.value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                }
                Type::Named {
                    name: "StringSlice".to_string(),
                    args: Vec::new(),
                }
            }
            // Unknown string method — typo-suggestion diagnostic if close to
            // a known name. `len` / `is_empty` / `contains` joined the
            // enumerated list 2026-05-22; `push_str` joined 2026-05-23;
            // `push` joined 2026-05-25 (kata 71 follow-up); `find` / `slice`
            // joined 2026-06-14 (StringSlice v1).
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
                    "find",
                    "is_empty",
                    "len",
                    "push",
                    "push_str",
                    "slice",
                    "sorted",
                    "sorted_by",
                    "split",
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
            // `CStr.to_string() -> Result[String, Utf8Error]` — the outbound
            // half of the `char*` read path (FFI/host-fn boundary; the LLVM-C
            // self-hosting port reads `char*` error messages this way). Mirrors
            // `String.from_utf8`'s return shape (UTF-8-validating). Codegen +
            // the `karac_runtime_cstr_to_string` extern lower it for `karac
            // build`; the interpreter validates via `String.from_utf8` semantics.
            "to_string" => {
                require_no_args(self, "to_string");
                Type::Named {
                    name: "Result".to_string(),
                    args: vec![
                        Type::Str,
                        Type::Named {
                            name: "Utf8Error".to_string(),
                            args: vec![],
                        },
                    ],
                }
            }
            // `CStr.to_string_slice() -> Result[StringSlice, Utf8Error]` — the
            // zero-copy sibling of `to_string`. Validates UTF-8 but yields a
            // borrowed `StringSlice` view over the receiver's bytes (no owning
            // copy); the borrow is tied to the `ref self` receiver, so the
            // view (and its `c"..."`-literal-static or `from_ptr`-borrowed
            // source) must outlive it. Codegen builds the `{ptr,len,cap=0}`
            // view; the interpreter validates via `String.from_utf8` semantics.
            "to_string_slice" => {
                require_no_args(self, "to_string_slice");
                Type::Named {
                    name: "Result".to_string(),
                    args: vec![
                        Type::Named {
                            name: "StringSlice".to_string(),
                            args: vec![],
                        },
                        Type::Named {
                            name: "Utf8Error".to_string(),
                            args: vec![],
                        },
                    ],
                }
            }
            _ => self.require_known_method(
                "CStr",
                method,
                &[
                    "as_bytes",
                    "as_ptr",
                    "is_empty",
                    "len",
                    "to_string",
                    "to_string_slice",
                ],
                args,
                span,
            ),
        }
    }

    /// Infer the return type of a method call on the owning `CString`
    /// (design.md § C-String Literals, "Owning `CString`"). Same introspection
    /// surface as the borrowed `CStr` — `as_ptr` (`*const u8` at the buffer's
    /// NUL-terminated bytes), `len` / `is_empty` / `as_bytes` reporting the
    /// source bytes excluding the trailing NUL — but no `to_string` /
    /// `to_string_slice` (those convert C bytes back to UTF-8; a `CString` is
    /// already an owned buffer the programmer built from a `String`, so the
    /// round-trip has no use here). The distinction from `CStr` is ownership,
    /// not surface: a `CString` owns its heap buffer and drops it, whereas a
    /// `CStr` borrows.
    pub(super) fn infer_cstring_method(
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
                "CString",
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
        // get/first/last return a BORROW of the element — `Option[ref T]`
        // (design.md § Feature 4 Part 3 — Option[ref T] accessors). At runtime
        // the borrow is a by-value alias of the element with codegen cleanup
        // suppressed (`scrutinee_is_borrow_call`); the `ref` makes the no-move
        // contract honest at the type level, so the typechecker rejects moving
        // the bound payload into an owned position.
        let option_elem = Type::Named {
            name: "Option".to_string(),
            args: vec![Type::Ref(Box::new(elem.clone()))],
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
        // get/first/last return a BORROW of the element — `Option[ref T]`
        // (design.md § Feature 4 Part 3 — Option[ref T] accessors). At runtime
        // the borrow is a by-value alias of the element with codegen cleanup
        // suppressed (`scrutinee_is_borrow_call`); the `ref` makes the no-move
        // contract honest at the type level, so the typechecker rejects moving
        // the bound payload into an owned position.
        let option_elem = Type::Named {
            name: "Option".to_string(),
            args: vec![Type::Ref(Box::new(elem.clone()))],
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
            "clear" => {
                // Empty the Vec (drop every element, length → 0). No-arg
                // in-place mutator; codegen reuses the element-drop fn so the
                // heap-owning elements can't leak (`vec_method.rs`).
                self.expect_no_args("Vec.clear", args, span);
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
