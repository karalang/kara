//! Type-expression parsing.
//!
//! Houses `parse_type` (the big TypeKind dispatch covering primitives,
//! generics, references, slices, options, results, function types
//! `Fn(args) -> ret with E`, tuple types, path types, etc.) and
//! `parse_path_type` (the `Foo.Bar[T1, T2]` PathExpr form used inside
//! type position).
//!
//! Lives in a sibling `impl super::Parser` block.

use crate::ast::*;
use crate::parser::ImplTraitBlockReason;
use crate::token::{Span, Token};

impl super::Parser {
    // ── Types ────────────────────────────────────────────────────

    /// Whether `tok` can START a type expression. Used only to disambiguate
    /// the contextual `frozen` mode from an ordinary identifier, so it lists
    /// the type-leading tokens rather than trying to be a full FIRST set:
    /// `frozen Node` is the mode, a lone `frozen` is a path.
    fn token_begins_type(tok: &Token) -> bool {
        matches!(
            tok,
            Token::Identifier { .. }
                | Token::Ref
                | Token::Mut
                | Token::LeftParen
                | Token::LeftBracket
                | Token::Star
        )
    }

    // Depth-guarded shell: type nesting (`Vec[Vec[…]]`, tuple types) shares
    // the recursion budget with expressions and patterns (B-2026-08-16-4).
    /// A function's return type: a trailing `with` after it is the
    /// function's clause, not the type's (see `Parser::return_tail`).
    pub(crate) fn parse_return_type(&mut self) -> Option<TypeExpr> {
        let paren = self.check(&Token::LeftParen);
        self.return_tail = true;
        let t = self.parse_type();
        self.return_tail = false;
        if let (true, Some(t)) = (paren, &t) {
            self.paren_return_types
                .insert(crate::resolver::SpanKey::from_span(&t.span));
        }
        t
    }

    /// A function type, `Fn(T) -> U with _`, and the once-callable variant
    /// `OnceFn(...)` (round 12.46, Step 4), the current token being `name`. Both share the same AST shape; the
    /// `is_once` flag distinguishes them so `lower_type_expr` can emit
    /// `Type::OnceFunction` for the OnceFn form.
    /// `MutFn(...)` (`docs/core-semantics.md` §9.6) parses as a
    /// non-once `FnType` and is recorded in `mut_fn_types`.
    fn parse_fn_type(&mut self, name: &str, start: Span, return_tail: bool) -> Option<TypeExpr> {
        let is_once = name == "OnceFn";
        let is_mut = name == "MutFn";
        self.advance();
        self.expect(&Token::LeftParen)?;
        let mut params = Vec::new();
        let mut own_params = Vec::new();
        while !self.check(&Token::RightParen) && !self.is_at_end() {
            // `Fn(own T)` (D5): an owned parameter of the function type.
            own_params.push(
                self.at_kw("own")
                    && !matches!(self.peek_token_ref_at(1), Token::Comma | Token::RightParen)
                    && self.eat_kw("own"),
            );
            params.push(self.parse_type()?);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RightParen)?;

        // A tail `Fn` passes the return position on to its own
        // return type: `-> Fn(i64) -> Fn(i64) with E` still ends in
        // the function's clause.
        let return_type = if self.eat(&Token::Arrow) {
            self.return_tail = return_tail;
            self.fn_return_at = Some(self.pos);
            Some(Box::new(self.parse_type()?))
        } else {
            None
        };

        // `Fn(A) -> R + CrossTask` (core semantics §6.3): the marker comes
        // after the return type and before `with`, as after a `dyn` trait,
        // and binds to the nearest function type.
        let last = self.tokens[self.pos.saturating_sub(1)].span;
        let marker_point = Span {
            line: last.line,
            column: last.column + last.length,
            offset: last.offset + last.length,
            length: 0,
        };
        let cross_task = self.peek_token_ref() == &Token::Plus && self.at_kw_at(1, "CrossTask");
        if cross_task {
            self.advance(); // `+`
            self.advance(); // `CrossTask`
        }

        let effect_spec = if self.at_empty_effect_clause() {
            // `Fn(A) -> R with ()`: the empty set, declared. Taken in a
            // return tail too: `with ()` is never a declaration's clause, so
            // `fn make() -> Fn(i64) -> i64 with () {` can only mean the type's.
            let at = self.current_span();
            self.advance(); // with
            self.advance(); // (
            self.advance(); // )
            Some(EffectSpec::Specific(EffectList {
                items: Vec::new(),
                span: self.span_from(&at),
            }))
        } else if !return_tail && self.at_kw("with") {
            // Peek-only: `parse_effect_list` consumes the `with`
            // keyword itself and then handles `_` / verbs / named
            // effect variables / group names uniformly. Pre-
            // consuming would force the call into the without-
            // `with`-prefix branch, which only handles bare verbs
            // and silently drops named variables (latent bug fixed
            // here as part of round 9).
            let saved = self.pos;
            if let Some(token) = self.tokens.get(self.pos + 1) {
                if matches!(token.token, Token::Underscore) {
                    self.advance(); // with
                    self.advance(); // _
                    Some(EffectSpec::Polymorphic)
                } else {
                    let effect_vars: Vec<String> = self.current_effect_vars().to_vec();
                    // Nested `Fn(...) with E` type: a comma after the
                    // clause separates fields or arguments, so it is
                    // taken as a stray separator only before another
                    // verb (`comma_continues_effect_clause`).
                    match self.parse_effect_list(&effect_vars, false) {
                        Some(effects) => Some(EffectSpec::Specific(effects)),
                        None => {
                            self.pos = saved;
                            return None;
                        }
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let span = self.span_from(&start);
        if is_mut {
            self.mut_fn_types
                .insert(crate::resolver::SpanKey::from_span(&span));
        }
        if cross_task {
            self.cross_task_fn_types
                .insert(crate::resolver::SpanKey::from_span(&span));
        }
        self.fn_type_marker_points
            .insert(crate::resolver::SpanKey::from_span(&span), marker_point);
        Some(TypeExpr {
            kind: TypeKind::FnType {
                params,
                return_type,
                effect_spec,
                is_once,
                own_params: if own_params.contains(&true) {
                    own_params
                } else {
                    Vec::new()
                },
            },
            span,
        })
    }

    /// The name of the function type the current token begins (`Fn`,
    /// `MutFn` or `OnceFn`), if it begins one. Rust's spellings, `fn(..)`
    /// (a function pointer), `FnMut(..)`, `FnOnce(..)` and any of them
    /// after `impl`, are each reported with the edit that writes Kāra's,
    /// and the type is then parsed as Kāra's, so nothing cascades
    /// (B-2026-10-29-1). On return the current token is the name.
    fn fn_type_head(&mut self) -> Option<&'static str> {
        fn kara_name(t: &Token) -> Option<(&'static str, bool)> {
            match t {
                Token::Identifier { name, raw: false } => match name.as_str() {
                    "Fn" => Some(("Fn", false)),
                    "MutFn" => Some(("MutFn", false)),
                    "OnceFn" => Some(("OnceFn", false)),
                    "FnMut" => Some(("MutFn", true)),
                    "FnOnce" => Some(("OnceFn", true)),
                    _ => None,
                },
                Token::Fn => Some(("Fn", true)),
                _ => None,
            }
        }
        let after_impl = matches!(self.peek_token_ref(), Token::Impl);
        let at = usize::from(after_impl);
        let (name, rust) = kara_name(self.peek_token_ref_at(at))?;
        // A Rust spelling, or anything after `impl`, is only taken for a
        // function type when its `(` follows.
        if (rust || after_impl) && !matches!(self.peek_token_ref_at(at + 1), Token::LeftParen) {
            return None;
        }
        if after_impl {
            let kw = self.current_span();
            let name_at = self.tokens[self.pos + 1].span.offset;
            let written = match self.peek_token_ref_at(1) {
                Token::Identifier { name, .. } => name.as_str(),
                _ => "fn",
            };
            self.error_at(
                &format!(
                    "`impl {written}(..)` is Rust syntax: in Kāra a function type \
                     is written `{name}(..) -> T`, with no `impl`"
                ),
                kw,
            );
            self.fix_edits.insert(
                crate::resolver::SpanKey::from_span(&kw),
                crate::resolver::TextEdit {
                    offset: kw.offset,
                    length: name_at.saturating_sub(kw.offset),
                    replacement: String::new(),
                },
            );
            self.advance();
        }
        if rust {
            let span = self.current_span();
            let message = match self.peek_token_ref() {
                Token::Fn => "`fn(..) -> T` is Rust's function-pointer type: Kāra writes \
                              a function type `Fn(..) -> T`"
                    .to_string(),
                _ => {
                    let written = if name == "MutFn" { "FnMut" } else { "FnOnce" };
                    format!("`{written}` is Rust's name: Kāra writes `{name}(..) -> T`")
                }
            };
            self.error_at(&message, span);
            self.fix_edits.insert(
                crate::resolver::SpanKey::from_span(&span),
                crate::resolver::TextEdit {
                    offset: span.offset,
                    length: span.length,
                    replacement: name.to_string(),
                },
            );
        }
        Some(name)
    }

    pub(crate) fn parse_type(&mut self) -> Option<TypeExpr> {
        if !self.enter_recursion() {
            return None;
        }
        // Red-zone stack growth — see parse_expr_bp_with_ctx for sizing.
        let result = stacker::maybe_grow(512 * 1024, 16 * 1024 * 1024, || self.parse_type_inner());
        self.exit_recursion();
        result
    }

    /// `Box<dyn Fn(A) -> R>`, Rust's boxed closure, at the current token. A
    /// Kāra function value has its own type, so the edit deletes `Box<dyn `
    /// and the closing `>`, and the type parses as the function type. The
    /// generic `Box<..>` edit would write `Box[dyn Fn(..)]`, which does not
    /// parse (B-2026-11-04-7). `None` when this is not that shape.
    fn rust_boxed_closure(&mut self, start: Span) -> Option<Option<TypeExpr>> {
        let fn_head = |t: &Token| {
            matches!(t, Token::Identifier { name, raw: false }
                if matches!(name.as_str(), "Fn" | "MutFn" | "OnceFn" | "FnMut" | "FnOnce"))
        };
        if !matches!(self.peek_token_ref(), Token::Identifier { name, raw: false } if name == "Box")
            || !matches!(self.peek_token_ref_at(1), Token::LessThan)
            || !matches!(self.peek_token_ref_at(2), Token::Dyn)
            || !fn_head(self.peek_token_ref_at(3))
        {
            return None;
        }
        let fn_at = self.tokens[self.pos + 3].span.offset;
        let (saved, saved_errors) = (self.pos, self.errors.len());
        self.pos += 3;
        let inner = self.parse_type();
        if inner.is_none() || !self.check(&Token::GreaterThan) {
            self.pos = saved;
            self.errors.truncate(saved_errors);
            return None;
        }
        let close = self.current_span();
        self.advance();
        self.error_at(
            "`Box<dyn Fn(..)>` is Rust's boxed closure: in Kāra a function value has its \
             own type, `Fn(A) -> R`, and a closure is one; return or store the closure \
             itself",
            start,
        );
        self.fix_diffs.insert(
            crate::resolver::SpanKey::from_span(&start),
            vec![
                crate::resolver::TextEdit {
                    offset: start.offset,
                    length: fn_at - start.offset,
                    replacement: String::new(),
                },
                crate::resolver::TextEdit {
                    offset: close.offset,
                    length: close.length,
                    replacement: String::new(),
                },
            ],
        );
        Some(inner)
    }

    fn parse_type_inner(&mut self) -> Option<TypeExpr> {
        let start = self.current_span();

        // One-shot: take the permission and clear it, so ONLY the outermost
        // `parse_type` of a parameter can accept `frozen`. Clearing it inside
        // the `frozen` branch alone was not enough — generic arguments are
        // parsed by a different descent, so `Vec[frozen N]` kept the flag lit
        // and was wrongly accepted. Taking it here covers every nested position
        // by construction rather than by enumerating them.
        let frozen_ok = std::mem::replace(&mut self.frozen_ok, false);
        let escaping_ok = std::mem::replace(&mut self.escaping_ok, false);
        // Same one-shot shape: only the return type's own tail sees it.
        let return_tail = std::mem::replace(&mut self.return_tail, false);
        if let Some(boxed) = self.rust_boxed_closure(start) {
            return boxed;
        }

        // `escaping Fn(...)` (`docs/core-semantics.md` §9.3) and `escaping
        // dyn Trait` (design.md § `dyn Trait`): a contextual keyword,
        // recognised only before `Fn` / `MutFn` / `OnceFn` / `dyn`, and
        // accepted only as a parameter's outermost type. The mark is kept
        // beside the tree (`Program::escaping_fn_types`), so the type itself
        // is unchanged.
        if let Token::Identifier { name, .. } = self.peek_token_ref() {
            if name == "escaping"
                && (matches!(self.peek_token_ref_at(1), Token::Dyn)
                    || matches!(
                        self.peek_token_ref_at(1),
                        Token::Identifier { name, .. }
                            if name == "Fn" || name == "MutFn" || name == "OnceFn"
                    ))
            {
                let kw_span = self.current_span();
                self.advance();
                if !escaping_ok {
                    self.error_at(
                        "`escaping` marks a function-typed or `dyn` parameter that the \
                         function may store or return; it is only accepted on a parameter's \
                         type",
                        kw_span,
                    );
                    // A returned function value needs no mark: delete the
                    // word and the space after it (B-2026-11-14-8).
                    let next = self.current_span().offset;
                    if next > kw_span.offset {
                        self.fix_edits.insert(
                            crate::resolver::SpanKey::from_span(&kw_span),
                            crate::resolver::TextEdit {
                                offset: kw_span.offset,
                                length: next - kw_span.offset,
                                replacement: String::new(),
                            },
                        );
                    }
                }
                let inner = self.parse_type()?;
                if escaping_ok {
                    self.escaping_fn_types
                        .insert(crate::resolver::SpanKey::from_span(&inner.span));
                }
                return Some(inner);
            }
        }

        // `frozen Type` — B-2026-08-01-33 mechanism 3, stage 1.
        //
        // A CONTEXTUAL keyword, not a reserved word: `frozen` stays a legal
        // identifier everywhere else, so adding the mode cannot break an
        // existing program that uses the name. The guard is the same shape the
        // `mut Slice[T]` arm below already uses — recognise it only when the
        // NEXT token can begin a type, so a bare `frozen` used as a type name
        // still parses as the path it always did.
        // THE MODE LEAVES THE TYPE TREE HERE, and that is the point. It is not
        // discarded — `parse_param` picks it up off `frozen_consumed` and
        // records it on [`Param::is_frozen`], where every later phase can see
        // it. What it does NOT do is travel inside the `TypeExpr`.
        //
        // `TypeKind::Frozen` exists and every walk in the compiler handles it,
        // but nothing constructs it, because a mode inside the type tree means
        // every phase that unwraps `Ref | MutRef` must learn a third form.
        // Trying that first turned up four rounds of such sites in codegen
        // alone (`call_dispatch`, `functions`, `mono`, then the param
        // type-name registry) with no reason to believe the fourth was the
        // last, and each one is a place a later phase could disagree about what
        // `frozen` means. A bit on the parameter cannot disagree with itself,
        // and it keeps the mode away from codegen by construction — codegen
        // will learn which values are non-counting through the plain-data
        // `elidable_ref_params` hint channel, per the codegen-containment
        // invariant. The variant stays for stage 2, when widening `frozen`
        // past parameter position will genuinely need a type-level mode.
        if let Token::Identifier { name, .. } = self.peek_token_ref() {
            if name == "frozen" && Self::token_begins_type(self.peek_token_ref_at(1)) {
                let kw_span = self.current_span();
                self.advance();
                if frozen_ok {
                    self.frozen_consumed = true;
                } else {
                    self.error_at(
                        "`frozen` is only supported on a Kara function's parameter \
                         type (stage 1); it is not yet accepted on a `let` \
                         annotation, a struct field, a return type, inside a \
                         generic argument, or on a foreign-import (`extern`) \
                         parameter",
                        kw_span,
                    );
                }
                // Parsing continues through the inner type either way, so one
                // misplaced keyword does not cascade into unrelated errors.
                // The flag was already taken above, so a nested `frozen`
                // (`frozen frozen T`) is rejected on the inner occurrence.
                let inner = self.parse_type()?;
                if !frozen_ok {
                    // Already reported; hand back the bare type so the rest of
                    // the signature keeps its shape.
                    return Some(inner);
                }
                // `frozen T` LOWERS TO A BORROW — `ref T` — and this is the
                // whole of stage 1's RC suppression.
                //
                // The design calls a `frozen` value "non-owning, non-counting".
                // Non-owning is exactly what `ref` already means, and codegen
                // emits no retain/release for a borrow because the caller keeps
                // ownership. Measured: an owned `shared` pass-through emits
                // rc_inc=4/rc_dec=9, the same chain through `ref` emits 0/0 —
                // and it still emits 0/0 with `KARAC_RC_ELIDE_REF_PARAMS=0`, so
                // that zero is the BORROW CONVENTION, not the rc-elide pass.
                //
                // Modes are declared at the signature (CLAUDE.md: body-level
                // ownership analysis "is not a signature-derivation
                // mechanism"), so a mode that wants borrow semantics has to say
                // so in the declared type. Inferring `Ref` into `param_modes`
                // is not enough and was measured not to be: the mode was
                // already `Ref` there while codegen still emitted the owned
                // traffic.
                //
                // Note what does NOT happen here: `TypeKind::Frozen` is still
                // never constructed. Codegen sees `Ref`, a form it already
                // handles on a shipped, ASAN-verified path, so this adds no
                // unwrap site anywhere. The frozen-ness itself stays on
                // `Param::is_frozen`, which is what the escape checker, the
                // freeze-site check, and `par` admission read.
                return Some(TypeExpr {
                    kind: TypeKind::Ref(Box::new(inner)),
                    span: self.span_from(&start),
                });
            }
        }

        if let Some(name) = self.fn_type_head() {
            return self.parse_fn_type(name, start, return_tail);
        }
        match self.peek_token_ref() {
            // `&T` / `&mut T`: Rust's borrow types. The edit rewrites the
            // sigil to the Kāra word and the type parses as that borrow, so
            // one diagnostic stands for it (B-2026-11-01-5).
            Token::Amp => {
                let amp_at = self.pos;
                self.advance();
                // `&'static str`, `&'a mut T`: Kāra has no lifetimes, so the
                // edit below covers the lifetime with the sigil and writes
                // the same type as for `&str` / `&mut T` (B-2026-11-14-25).
                if matches!(self.peek_token_ref(), Token::Label(_)) {
                    self.advance();
                }
                let is_mut = self.eat(&Token::Mut);
                let inner_at = self.current_span().offset;
                let sigil = Span {
                    length: inner_at.saturating_sub(start.offset),
                    ..start
                };
                let (message, kara) = if is_mut {
                    (
                        "`&mut T` is Rust syntax -- a borrow that writes is `mut ref T`",
                        "mut ref ",
                    )
                } else if self.rust_rc_param_borrow(amp_at) {
                    // `&Rc<RefCell<T>>` / `&Option<Rc<RefCell<T>>>` on a
                    // parameter: the type becomes the shared struct `T`, a
                    // handle, and a bare parameter already borrows, so the
                    // `&` goes. `ref` made each node matched out of it a
                    // `ref T`, which `Some(n)` could not return as an
                    // `Option[T]`, with no fix (kata 1123).
                    (
                        "`&T` is Rust syntax -- a bare parameter already borrows, and \
                         `Rc<RefCell<T>>` is a shared struct's handle in Kāra: drop the `&`",
                        "",
                    )
                } else {
                    (
                        "`&T` is Rust syntax -- a borrow is `ref T` (a bare parameter \
                         already borrows)",
                        "ref ",
                    )
                };
                // `&str`: Rust's borrowed text, which is exactly `Str` (a
                // `Str` is already a view). The `&` edit alone made it
                // `ref Str`, which a `String` argument does not pass as, so
                // every call site failed with no fix (kata 2002). One
                // diagnostic, one edit over `&str`.
                if !is_mut
                    && matches!(self.peek_token_ref(), Token::Identifier { name, .. } if name == "str")
                    && !matches!(self.peek_token_ref_at(1), Token::LeftBracket | Token::Dot)
                {
                    let word = self.current_span();
                    self.advance();
                    let whole = Span {
                        length: word.offset + word.length - start.offset,
                        ..start
                    };
                    self.error_at(
                        "`&str` is Rust's borrowed string -- in Kāra a view of text is `Str` \
                         (a `String` argument passes as `Str`)",
                        whole,
                    );
                    self.fix_edits.insert(
                        crate::resolver::SpanKey::from_span(&whole),
                        crate::resolver::TextEdit {
                            offset: whole.offset,
                            length: whole.length,
                            replacement: "Str".to_string(),
                        },
                    );
                    let span = self.span_from(&start);
                    return Some(TypeExpr {
                        kind: TypeKind::Path(PathExpr {
                            segments: vec!["Str".to_string()],
                            generic_args: None,
                            effect_args: None,
                            span,
                        }),
                        span,
                    });
                }
                // `&[T]` / `&mut [T]`: a borrowed run of elements, which is
                // exactly `Slice[T]` / `mut Slice[T]` (a slice is already a
                // view). One diagnostic and one edit, in place of the `&`
                // edit followed by the `[T]` one, which has no edit because
                // a bare `[T]` could mean `Vec[T]` (B-2026-11-04-7).
                if self.check(&Token::LeftBracket) {
                    let open = self.current_span();
                    let before = self.errors.len();
                    let inner = self.parse_type()?;
                    let is_slice = matches!(&inner.kind, TypeKind::Path(p)
                        if p.segments.len() == 1 && p.segments[0] == "Slice");
                    let pos = self.errors[before..]
                        .iter()
                        .position(|e| e.span == open && e.message.starts_with("`[T]` is Rust's"));
                    if let (true, Some(at)) = (is_slice, pos) {
                        self.errors.remove(before + at);
                        let (message, word) = if is_mut {
                            ("`&mut [T]` is Rust's mutable slice -- in Kāra it is `mut Slice[T]`", "mut ")
                        } else {
                            (
                                "`&[T]` is Rust's slice -- in Kāra a borrowed view of a run of \
                                 elements is `Slice[T]`",
                                "",
                            )
                        };
                        self.error_at(message, sigil);
                        self.fix_diffs.insert(
                            crate::resolver::SpanKey::from_span(&sigil),
                            vec![
                                crate::resolver::TextEdit {
                                    offset: sigil.offset,
                                    length: sigil.length,
                                    replacement: word.to_string(),
                                },
                                crate::resolver::TextEdit {
                                    offset: open.offset,
                                    length: open.length,
                                    replacement: "Slice[".to_string(),
                                },
                            ],
                        );
                        let elem = match inner.kind {
                            TypeKind::Path(PathExpr {
                                generic_args: Some(mut args),
                                ..
                            }) if args.len() == 1 => match args.pop() {
                                Some(GenericArg::Type(t)) => t,
                                _ => return None,
                            },
                            _ => return None,
                        };
                        let span = self.span_from(&start);
                        return Some(if is_mut {
                            TypeExpr {
                                kind: TypeKind::MutSlice(Box::new(elem)),
                                span,
                            }
                        } else {
                            TypeExpr {
                                kind: TypeKind::Path(PathExpr {
                                    segments: vec!["Slice".to_string()],
                                    generic_args: Some(vec![GenericArg::Type(elem)]),
                                    effect_args: None,
                                    span,
                                }),
                                span,
                            }
                        });
                    }
                    self.error_at(message, sigil);
                    self.fix_edits.insert(
                        crate::resolver::SpanKey::from_span(&sigil),
                        crate::resolver::TextEdit {
                            offset: sigil.offset,
                            length: sigil.length,
                            replacement: kara.to_string(),
                        },
                    );
                    let inner = Box::new(inner);
                    return Some(TypeExpr {
                        kind: if is_mut {
                            TypeKind::MutRef(inner)
                        } else {
                            TypeKind::Ref(inner)
                        },
                        span: self.span_from(&start),
                    });
                }
                self.error_at(message, sigil);
                self.fix_edits.insert(
                    crate::resolver::SpanKey::from_span(&sigil),
                    crate::resolver::TextEdit {
                        offset: sigil.offset,
                        length: sigil.length,
                        replacement: kara.to_string(),
                    },
                );
                let inner = self.parse_type()?;
                if kara.is_empty() {
                    return Some(inner);
                }
                let inner = Box::new(inner);
                Some(TypeExpr {
                    kind: if is_mut {
                        TypeKind::MutRef(inner)
                    } else {
                        TypeKind::Ref(inner)
                    },
                    span: self.span_from(&start),
                })
            }
            // ref Type
            Token::Ref => {
                self.advance();
                let inner = self.parse_type()?;
                Some(TypeExpr {
                    kind: TypeKind::Ref(Box::new(inner)),
                    span: self.span_from(&start),
                })
            }
            // mut ref Type  |  mut Slice[T]
            Token::Mut => {
                self.advance();
                // `mut Slice[T]` — mutable slice view (no `ref` keyword).
                if let Token::Identifier { name, .. } = self.peek_token_ref() {
                    if name == "Slice" {
                        // Parse the Slice path as a normal type, then strip
                        // down to its element and re-wrap as MutSlice.
                        let slice_ty = self.parse_type()?;
                        let element = match slice_ty.kind {
                            TypeKind::Path(ref path)
                                if path.segments.len() == 1 && path.segments[0] == "Slice" =>
                            {
                                match &path.generic_args {
                                    Some(args) if args.len() == 1 => match &args[0] {
                                        crate::ast::GenericArg::Type(t) => t.clone(),
                                        _ => {
                                            self.error(
                                                "mut Slice[T] requires a type argument, found const",
                                            );
                                            return None;
                                        }
                                    },
                                    _ => {
                                        self.error(
                                            "mut Slice[T] requires exactly one type argument",
                                        );
                                        return None;
                                    }
                                }
                            }
                            _ => {
                                self.error("expected Slice[T] after `mut`");
                                return None;
                            }
                        };
                        return Some(TypeExpr {
                            kind: TypeKind::MutSlice(Box::new(element)),
                            span: self.span_from(&start),
                        });
                    }
                }
                // Otherwise `mut ref T`.
                self.expect(&Token::Ref)?;
                let inner = self.parse_type()?;
                Some(TypeExpr {
                    kind: TypeKind::MutRef(Box::new(inner)),
                    span: self.span_from(&start),
                })
            }
            // weak Type
            Token::Identifier {
                ref name,
                raw: false,
            } if name == "weak"
                && matches!(
                    self.peek_token_ref_at(1),
                    Token::Identifier { .. }
                        | Token::SelfType
                        | Token::LeftParen
                        | Token::Star
                        | Token::Ref
                        | Token::Mut
                ) =>
            {
                self.advance();
                let inner = self.parse_type()?;
                Some(TypeExpr {
                    kind: TypeKind::Weak(Box::new(inner)),
                    span: self.span_from(&start),
                })
            }
            // *const T or *mut T
            Token::Star => {
                self.advance();
                let is_mut = if self.eat(&Token::Mut) {
                    true
                } else {
                    // expect "const" as identifier
                    match self.peek_token_ref() {
                        Token::Const => {
                            self.advance();
                            false
                        }
                        _ => {
                            self.error("Expected 'const' or 'mut' after '*' in pointer type");
                            return None;
                        }
                    }
                };
                let inner = self.parse_type()?;
                Some(TypeExpr {
                    kind: TypeKind::Pointer {
                        is_mut,
                        inner: Box::new(inner),
                    },
                    span: self.span_from(&start),
                })
            }
            // () unit type or (A, B) tuple type
            Token::LeftParen => {
                // Read before the inner type, whose own function types
                // move it.
                let fn_return = self.fn_return_at == Some(self.pos);
                self.advance();
                if self.eat(&Token::RightParen) {
                    return Some(TypeExpr {
                        kind: TypeKind::Unit,
                        span: self.span_from(&start),
                    });
                }
                let first = self.parse_type()?;
                if self.eat(&Token::Comma) {
                    // Tuple type
                    let mut types = vec![first];
                    while !self.check(&Token::RightParen) && !self.is_at_end() {
                        types.push(self.parse_type()?);
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    self.expect(&Token::RightParen)?;
                    Some(TypeExpr {
                        kind: TypeKind::Tuple(types),
                        span: self.span_from(&start),
                    })
                } else {
                    // Parenthesized type
                    self.expect(&Token::RightParen)?;
                    // `(Fn() -> Fn() -> i64) + CrossTask` (core semantics
                    // §6.3): the marker binds to the nearest function type,
                    // and parentheses make that the outer one
                    // (B-2026-11-02-20). A parenthesized return type of a
                    // function type, `Fn() -> (Fn() -> i64) + CrossTask`,
                    // leaves the marker to that outer function type.
                    if !fn_return
                        && matches!(first.kind, TypeKind::FnType { .. })
                        && self.peek_token_ref() == &Token::Plus
                        && self.at_kw_at(1, "CrossTask")
                    {
                        self.advance(); // `+`
                        self.advance(); // `CrossTask`
                        self.cross_task_fn_types
                            .insert(crate::resolver::SpanKey::from_span(&first.span));
                    }
                    Some(first)
                }
            }
            // `impl Trait[GenericArgs] [with EFFECT_LIST]` — existential
            // / argument-sugar type marker (see design.md § `impl Trait`
            // (Existential Types) and phase-5-diagnostics.md line 391).
            //
            // Slice 1: parser surface + AST node only. The four legal
            // positions (argument-type, return-type, trait-method-
            // return, RHS of `type` aliases) all reach this arm through
            // their normal `parse_type` path; the two illegal positions
            // (`Vec[impl T]`-style nested generic-args, and trait-method
            // argument-type position) are guarded by the
            // `impl_trait_block_stack` — see `parse_generic_type_args`
            // and `parse_param` for the matching push sites.
            //
            // Surface form is `impl <TraitPath>[<args>] [with E]`. The
            // trait path is parsed via `parse_path_type` so multi-
            // segment paths (`std.iter.Iterator`) and generic args
            // (`Iterator[Item = i64]`) are handled uniformly with
            // regular path types.
            // `dyn TraitPath[GENERIC_ARGS] [+ CrossTask] [with E]`
            // (design.md § `dyn Trait`). A `dyn` type names one trait; the
            // one trait allowed after `+` is `CrossTask`. In a return type
            // a `with` is the function's clause, as after a function type.
            Token::Dyn => {
                let dyn_kw_span = self.current_span();
                self.advance(); // consume `dyn`
                let (trait_path, assoc_bindings) = self.parse_trait_path_with_bindings()?;
                let (segments, args_opt) = (trait_path.segments, trait_path.generic_args);
                let args: Vec<GenericArg> = args_opt.unwrap_or_default();
                let trait_path_clean = PathExpr {
                    segments,
                    generic_args: None,
                    effect_args: None,
                    span: self.span_from(&dyn_kw_span),
                };
                // Where a fix-it writes `+ CrossTask`: after the trait,
                // before any bound or `with` (B-2026-11-02-12).
                let last = self.tokens[self.pos.saturating_sub(1)].span;
                let marker_point = Span {
                    line: last.line,
                    column: last.column + last.length,
                    offset: last.offset + last.length,
                    length: 0,
                };
                let mut cross_task = false;
                while self.peek_token_ref() == &Token::Plus {
                    self.advance(); // `+`
                    let bound_start = self.current_span();
                    let bound = self.parse_path_type()?;
                    if bound.segments.last().map(String::as_str) == Some("CrossTask")
                        && bound.generic_args.is_none()
                    {
                        cross_task = true;
                    } else {
                        self.error_at(
                            &format!(
                                "error[E_DYN_SECOND_TRAIT]: a `dyn` type names one trait, and \
                                 the only trait allowed after `+` is `CrossTask`; `{}` cannot \
                                 be added. Declare a trait with both as supertraits and use \
                                 `dyn` of that (design.md § `dyn Trait`)",
                                bound.segments.join(".")
                            ),
                            self.span_from(&bound_start),
                        );
                    }
                }
                let effects = self.parse_type_effect_clause(&dyn_kw_span, return_tail)?;
                let span = self.span_from(&start);
                self.fn_type_marker_points
                    .insert(crate::resolver::SpanKey::from_span(&span), marker_point);
                Some(TypeExpr {
                    kind: TypeKind::Dyn {
                        trait_path: trait_path_clean,
                        args,
                        assoc_bindings,
                        effects,
                        cross_task,
                        span,
                    },
                    span,
                })
            }
            Token::Impl => {
                let impl_kw_span = self.current_span();
                self.advance(); // consume `impl`

                // Position rejection. We still parse the body of the
                // `impl Trait` expression after emitting the
                // diagnostic — error recovery: producing a
                // `TypeKind::Error` here would cascade into a noisy
                // "expected type" downstream, while producing a real
                // `ImplTrait` lets the rest of the signature parse
                // cleanly and gives the user one focused diagnostic
                // rather than a cluster. The diagnostic is anchored
                // at the `impl` keyword's span.
                if let Some(reason) = self.current_impl_trait_block() {
                    let msg = match reason {
                        ImplTraitBlockReason::NestedGenericArg => {
                            "error[E_IMPL_TRAIT_IN_NESTED_POSITION]: `impl Trait` is not \
                             permitted inside a nested generic-argument position in 0.8; \
                             introduce an explicit generic parameter on the enclosing \
                             function (e.g. `fn f[T: Trait](x: Vec[T])`) instead. \
                             Deep-position `impl Trait` is not in 0.8 — see design.md \
                             § `impl Trait` (Existential Types)."
                                .to_string()
                        }
                        ImplTraitBlockReason::TraitMethodArg => {
                            "error[E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG]: `impl Trait` is not \
                             permitted in trait-method argument position; use the explicit \
                             generic form `fn method[T: Trait](x: T)` on the trait method \
                             declaration instead. The compiler restricts argument-position \
                             `impl Trait` to free functions and impl-block methods — see \
                             design.md § `impl Trait` (Existential Types)."
                                .to_string()
                        }
                    };
                    self.errors.push(crate::parser::ParseError {
                        kind: crate::parser::ParseErrorKind::Syntax,
                        message: msg,
                        span: impl_kw_span,
                    });
                }

                // Trait path + generic args (parsed uniformly with
                // regular path types), plus any inline associated-type
                // bindings — `impl Iterator[Item = i64]` (B-2026-08-22-4).
                let (trait_path, assoc_bindings) = self.parse_trait_path_with_bindings()?;
                let (segments, args_opt) = (trait_path.segments, trait_path.generic_args);
                let args: Vec<GenericArg> = args_opt.unwrap_or_default();

                // Rebuild a clean PathExpr (without the generic_args
                // — those live in the ImplTrait variant's `args`
                // field, not nested under the path).
                let trait_path_clean = PathExpr {
                    segments,
                    generic_args: None,
                    effect_args: None,
                    span: self.span_from(&impl_kw_span),
                };

                // Optional `with EFFECT_LIST` clause, as on a function type.
                let use_effects = self.parse_type_effect_clause(&impl_kw_span, return_tail)?;

                let span = self.span_from(&start);
                Some(TypeExpr {
                    kind: TypeKind::ImplTrait {
                        trait_path: trait_path_clean,
                        args,
                        assoc_bindings,
                        use_effects,
                        span,
                    },
                    span,
                })
            }
            // Path type: ident[::ident]*[<T, U>]
            Token::Identifier { .. } | Token::SelfType => {
                let path = self.parse_path_type()?;
                Some(TypeExpr {
                    kind: TypeKind::Path(path),
                    span: self.span_from(&start),
                })
            }
            // Rust's `[T; N]` and `[T]` (B-2026-10-20-27). Kāra spells them
            // `Array[T, N]` and `Slice[T]`; the array form carries a
            // machine-applicable fix, and either way the type parses as
            // the Kāra one so nothing cascades.
            Token::LeftBracket => {
                let open = self.current_span();
                self.advance();
                let elem = self.parse_type()?;
                if self.check(&Token::Semicolon) {
                    let semi = self.current_span();
                    self.advance();
                    let len = if matches!(self.peek_token_ref(), Token::Identifier { .. })
                        && matches!(self.peek_token_ref_at(1), Token::RightBracket)
                    {
                        GenericArg::Type(self.parse_type()?)
                    } else {
                        GenericArg::Const(self.parse_expression()?)
                    };
                    self.expect(&Token::RightBracket)?;
                    self.errors.push(super::ParseError {
                        kind: crate::parser::ParseErrorKind::Syntax,
                        message: "`[T; N]` is Rust's array type -- in Kāra a fixed-size \
                                  array is `Array[T, N]`"
                            .to_string(),
                        span: open,
                    });
                    self.fix_diffs.insert(
                        crate::resolver::SpanKey::from_span(&open),
                        vec![
                            crate::resolver::TextEdit {
                                offset: open.offset,
                                length: open.length,
                                replacement: "Array[".to_string(),
                            },
                            crate::resolver::TextEdit {
                                offset: semi.offset,
                                length: semi.length,
                                replacement: ",".to_string(),
                            },
                        ],
                    );
                    let span = self.span_from(&start);
                    return Some(TypeExpr {
                        kind: TypeKind::Path(PathExpr {
                            segments: vec!["Array".to_string()],
                            generic_args: Some(vec![GenericArg::Type(elem), len]),
                            effect_args: None,
                            span,
                        }),
                        span,
                    });
                }
                self.expect(&Token::RightBracket)?;
                self.errors.push(super::ParseError {
                    kind: crate::parser::ParseErrorKind::Syntax,
                    message: "`[T]` is Rust's slice type -- in Kāra a view of a run of \
                              elements is `Slice[T]`, and an owned, growable one is `Vec[T]`"
                        .to_string(),
                    span: open,
                });
                let span = self.span_from(&start);
                Some(TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec!["Slice".to_string()],
                        generic_args: Some(vec![GenericArg::Type(elem)]),
                        effect_args: None,
                        span,
                    }),
                    span,
                })
            }
            _ => {
                self.error(&format!(
                    "Expected type, found {}",
                    self.peek_token_ref().describe()
                ));
                None
            }
        }
    }

    /// `parse_path_type` for a TRAIT path — the `Src[Item = i64]` after an
    /// `impl` / `dyn` keyword — returning the positional args on the path and
    /// the inline associated-type bindings separately.
    ///
    /// `impl Trait` and `dyn Trait` name a TRAIT, not a type, so their bracket
    /// list is the same grammar a trait bound's is: `IDENT = TYPE` inside it
    /// binds an associated type rather than passing a positional argument
    /// (syntax.md § BOUND_ARG). Ordinary path types keep the flag off, so
    /// `Vec[Item = i64]` stays the parse error it should be.
    ///
    /// Mirrors `parse_trait_bound`'s save/enable/drain dance exactly, and for
    /// the same reason: the shared arg parser collects bindings into
    /// `pending_assoc_bindings`, and the saved/restored pair keeps a nested
    /// occurrence from inheriting or clobbering an outer one's.
    /// The optional `with EFFECT_LIST` after `impl Trait` or `dyn Trait`,
    /// mirroring the function type's clause: `with _` is one
    /// `EffectItem::Polymorphic`, and in a return type's tail the `with`
    /// is the function's, so nothing is taken. `parse_effect_list`
    /// consumes the `with` itself, so this peeks only. `None` is a parse
    /// failure; `Some(None)` is no clause.
    fn parse_type_effect_clause(
        &mut self,
        kw_span: &Span,
        return_tail: bool,
    ) -> Option<Option<EffectList>> {
        if return_tail || !self.at_kw("with") {
            return Some(None);
        }
        let saved = self.pos;
        let Some(token) = self.tokens.get(self.pos + 1) else {
            return Some(None);
        };
        if matches!(token.token, Token::Underscore) {
            self.advance(); // with
            self.advance(); // _
            return Some(Some(EffectList {
                items: vec![EffectItem::Polymorphic],
                span: self.span_from(kw_span),
            }));
        }
        let effect_vars: Vec<String> = self.current_effect_vars().to_vec();
        // Nested `Fn(...) with E` type: a trailing comma is a legitimate
        // param/list separator, not a stray effect-clause comma — recovery
        // off.
        match self.parse_effect_list(&effect_vars, false) {
            Some(effects) => Some(Some(effects)),
            None => {
                self.pos = saved;
                None
            }
        }
    }

    fn parse_trait_path_with_bindings(&mut self) -> Option<(PathExpr, Vec<AssocBinding>)> {
        let saved_allowed = self.assoc_bindings_allowed;
        let saved_pending = std::mem::take(&mut self.pending_assoc_bindings);
        self.assoc_bindings_allowed = true;
        let path = self.parse_path_type();
        self.assoc_bindings_allowed = saved_allowed;
        let bindings = std::mem::replace(&mut self.pending_assoc_bindings, saved_pending);
        Some((path?, bindings))
    }

    /// `Name<T, U>`, Rust's generic arguments, in a type (the `<` being the
    /// current token): reported at the `<`, with the edit that writes each
    /// of the list's angle brackets as Kāra's square ones when the list is
    /// balanced, and the tokens rewritten the same way so the list parses as
    /// `Name[T, U]` and nothing cascades (B-2026-10-29-23). An unbalanced
    /// list is reported with no fix and left as it was.
    pub(crate) fn angle_generic_args(&mut self, name: &str) {
        self.angle_generic_list(name, "arguments");
    }

    /// The same, for any bracketed list: `what` names it in the message
    /// ("arguments" in a type, "parameters" on a declaration).
    pub(crate) fn angle_generic_list(&mut self, name: &str, what: &str) {
        let open = self.current_span();
        // The open brackets, innermost last; each closer must match its own.
        let mut open_brackets = Vec::new();
        let mut angles = Vec::new();
        let mut end = None;
        for (i, t) in self.tokens.iter().enumerate().skip(self.pos) {
            let closes: &[Token] = match t.token {
                Token::LessThan | Token::LeftBracket | Token::LeftParen => {
                    if t.token == Token::LessThan {
                        angles.push(i);
                    }
                    open_brackets.push(t.token.clone());
                    &[]
                }
                Token::GreaterThan => &[Token::LessThan],
                Token::GreaterGreater => &[Token::LessThan, Token::LessThan],
                Token::RightBracket => &[Token::LeftBracket],
                Token::RightParen => &[Token::LeftParen],
                Token::EOF | Token::Semicolon | Token::LeftBrace | Token::RightBrace => break,
                // An associated-type binding inside a nested list
                // (`impl<T: Add<Output = T>>`) is part of it; at the list's
                // own level an `=` ends the type (`Vec<i64 = ..`).
                Token::Equal if open_brackets.len() <= 1 => break,
                _ => &[],
            };
            if matches!(t.token, Token::GreaterThan | Token::GreaterGreater) {
                angles.push(i);
            }
            if closes
                .iter()
                .any(|c| open_brackets.pop().as_ref() != Some(c))
            {
                break;
            }
            if open_brackets.is_empty() {
                end = Some(i);
                break;
            }
        }
        self.error_at(
            &format!(
                "`{name}<..>` is Rust's spelling: Kāra writes generic {what} in \
                 brackets, `{name}[..]`"
            ),
            open,
        );
        if end.is_none() {
            return;
        }
        let mut edits = Vec::new();
        for &i in angles.iter().rev() {
            let span = self.tokens[i].span;
            let (bracket, text) = match self.tokens[i].token {
                Token::LessThan => (Token::LeftBracket, "["),
                Token::GreaterThan => (Token::RightBracket, "]"),
                _ => (Token::RightBracket, "]]"),
            };
            edits.push(crate::resolver::TextEdit {
                offset: span.offset,
                length: span.length,
                replacement: text.to_string(),
            });
            self.tokens[i].token = bracket;
            if text == "]]" {
                // `>>` closes two lists: split it into two `]`.
                self.tokens[i].span.length = 1;
                let second = Span {
                    column: span.column + 1,
                    offset: span.offset + 1,
                    length: 1,
                    ..span
                };
                self.tokens.insert(
                    i + 1,
                    crate::token::SpannedToken {
                        token: Token::RightBracket,
                        span: second,
                    },
                );
            }
        }
        edits.reverse();
        self.fix_diffs
            .insert(crate::resolver::SpanKey::from_span(&open), edits);
    }

    pub(crate) fn parse_path_type(&mut self) -> Option<PathExpr> {
        let start = self.current_span();
        let segments = self.parse_path_segments()?;
        if self.check(&Token::LessThan) && !self.cast_type {
            let name = segments.last().map_or("", |s| s.as_str()).to_string();
            self.angle_generic_args(&name);
        }

        // Check for generic args [T, U] — unambiguous in type position. The
        // flag lets this list, and no other, end in an effect argument.
        let saved_allowed = std::mem::replace(&mut self.effect_args_allowed, true);
        let saved_pending = self.pending_effect_args.take();
        let generic_args = if self.check(&Token::LeftBracket) {
            Some(self.parse_generic_type_args())
        } else {
            None
        };
        self.effect_args_allowed = saved_allowed;
        let effect_args = std::mem::replace(&mut self.pending_effect_args, saved_pending);
        // `EventBus[with E]` writes no type argument: the list is absent, as
        // for a bare `EventBus`.
        let mut generic_args = match generic_args {
            Some(args) => Some(args?).filter(|a| !a.is_empty() || effect_args.is_none()),
            None => None,
        };

        let span = self.span_from(&start);
        self.take_container_hasher_arg(&segments, &mut generic_args, span);

        Some(PathExpr {
            segments,
            generic_args,
            effect_args,
            span,
        })
    }

    /// Remove a `Map[K, V, H]` / `Set[T, H]` trailing hasher argument from the
    /// path's generic arguments and record it on the parser (B-2026-08-21-6,
    /// B-2026-08-22-6).
    ///
    /// See [`crate::ast::Program::container_hashers`] for why the argument is
    /// deleted here rather than left in the tree. In one sentence: every later
    /// phase recognizes a `Map` by "head name plus exactly two arguments", so
    /// an extra one stops the type from being a `Map` to any of them.
    ///
    /// WHY THIS TAKES AN UNRECOGNIZED NAME TOO. Before user hashers, only the
    /// two compiler-known selectors were removed and everything else fell
    /// through to `take_hasher_type_arg`, whose whitelist reported it. A user
    /// hasher is any type that `impl BuildHasher for`, and the parser has no
    /// impl table — it runs before resolution — so it cannot tell
    /// `MyBuildHasher` from `i64`. It therefore removes ANY single trailing
    /// path argument and records the name; validation moves one phase later to
    /// `TypeChecker::check_recorded_container_hasher`, which does have the
    /// impl table
    /// and reports `Map[K, V, i64]` there instead. The alternative — leaving
    /// unknown names in place — would make a legitimate `Map[K, V,
    /// MyBuildHasher]` stop being a `Map` to every later phase, which is the
    /// exact failure this deletion exists to avoid.
    ///
    /// Still removes ONLY a bare path in exactly the trailing position, so
    /// `Map[K, V, H, X]`, `Map[K, V, Vec[u8]]` and `SortedMap[K, V, H]` all keep
    /// their arguments and reach `take_hasher_type_arg`, which reports them.
    pub(super) fn take_container_hasher_arg(
        &mut self,
        segments: &[String],
        generic_args: &mut Option<Vec<GenericArg>>,
        span: crate::token::Span,
    ) {
        // Single-segment only: `Map` / `Set` are prelude types written bare, so
        // a qualified `some.module.Map` is somebody ELSE's type and its third
        // argument is not a hasher. Falling through leaves the argument in
        // place, which is the safe direction — the typechecker then reports it
        // rather than this quietly eating it.
        if segments.len() != 1 {
            return;
        }
        let base = match segments[0].as_str() {
            "Map" => 2,
            "Set" => 1,
            _ => return,
        };
        let Some(args) = generic_args.as_mut() else {
            return;
        };
        if args.len() != base + 1 {
            return;
        }
        let GenericArg::Type(te) = &args[base] else {
            return;
        };
        let TypeKind::Path(p) = &te.kind else {
            return;
        };
        if p.generic_args.is_some() {
            return;
        }
        let kind = match p.segments.last().map(String::as_str) {
            Some("FxBuildHasher") => crate::hasher_kind::HasherKind::Fx,
            Some("SipHash13BuildHasher") => crate::hasher_kind::HasherKind::SipHash13,
            // Anything else is a candidate user hasher. Keyed on the LAST
            // segment so a qualified `my.hashers.MyBuildHasher` records the
            // same name the impl table is keyed by.
            Some(name) => crate::hasher_kind::HasherKind::User(name.to_string()),
            None => return,
        };
        args.truncate(base);
        self.container_hashers
            .insert(crate::resolver::SpanKey::from_span(&span), kind);
    }
}
