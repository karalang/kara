//! `usize` and `isize` written outside FFI (B-2026-11-06-3).
//!
//! design.md §5: the pointer-sized integers are for FFI signatures, and Kāra
//! code uses `i64` for every size, length, count and index. Rust code writes
//! `Vec<usize>` for indices, which `karac fix` spells `Vec[usize]`; the
//! values then meet the `i64` the library's `len()`, `enumerate()` and
//! indexing give, and each meeting is a coercion or comparison error with no
//! fix. When a program has such an error, every `usize` or `isize` it writes
//! as a type outside an FFI item gets a fix to `i64`.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::{IntSize, Type, TypeErrorKind, UIntSize};

impl<'a> super::TypeChecker<'a> {
    /// Notes an `i64` meeting a `usize` or `isize`, either way round.
    pub(super) fn note_pointer_int_mix(&mut self, a: &Type, b: &Type) {
        let pointer =
            |t: &Type| matches!(t, Type::UInt(UIntSize::Usize) | Type::Int(IntSize::Isize));
        let i64 = |t: &Type| matches!(t, Type::Int(IntSize::I64));
        if (pointer(a) && i64(b)) || (i64(a) && pointer(b)) {
            self.pointer_int_mixed = true;
        }
    }

    /// [`Self::note_pointer_int_mix`] at each place two types of one shape
    /// differ: `Vec[(i64, usize)]` against the `(i64, i64)` items `collect`
    /// is given.
    pub(super) fn note_pointer_int_mix_within(&mut self, a: &Type, b: &Type) {
        match (a, b) {
            (Type::Tuple(xs), Type::Tuple(ys)) if xs.len() == ys.len() => {
                for (x, y) in xs.iter().zip(ys) {
                    self.note_pointer_int_mix_within(x, y);
                }
            }
            (Type::Named { name: n, args: xs }, Type::Named { name: m, args: ys })
                if n == m && xs.len() == ys.len() =>
            {
                for (x, y) in xs.iter().zip(ys) {
                    self.note_pointer_int_mix_within(x, y);
                }
            }
            (Type::Ref(x), Type::Ref(y)) | (Type::MutRef(x), Type::MutRef(y)) => {
                self.note_pointer_int_mix_within(x, y)
            }
            // `Vec[Array[Option[usize], 4]]` against the arrays of `i64`
            // indices a `scan` gives (kata 1182's Rust arm, B-2026-11-17-107).
            (Type::Array { element: x, .. }, Type::Array { element: y, .. })
            | (Type::Slice { element: x, .. }, Type::Slice { element: y, .. }) => {
                self.note_pointer_int_mix_within(x, y)
            }
            _ => self.note_pointer_int_mix(a, b),
        }
    }

    /// The fixes, one error each, when the program mixed the two.
    pub(super) fn emit_written_pointer_int_fixes(&mut self) {
        let mut floors = std::mem::take(&mut self.usize_floor_calls);
        let mut seen_floor = std::collections::HashSet::new();
        floors.retain(|f| seen_floor.insert(f.2.offset));
        if !self.pointer_int_mixed {
            self.unsigned_floor_errors(floors, false);
            return;
        }
        let errors_before = self.errors.len();
        // FFI items keep their pointer-sized integers: an `extern` function
        // or block, and a struct with a `#[repr(..)]` layout.
        let mut ffi: Vec<Span> = Vec::new();
        for item in &self.program.items {
            match item {
                Item::ExternFunction(e) => ffi.push(e.span),
                Item::ExternBlock(b) => ffi.push(b.span),
                Item::StructDef(s) if s.attributes.iter().any(|a| a.path == ["repr"]) => {
                    ffi.push(s.span)
                }
                _ => {}
            }
        }
        let inside = |s: &Span| {
            ffi.iter()
                .any(|f| s.offset >= f.offset && s.offset < f.offset + f.length)
        };
        // Not in a program with FFI items, where a `usize` may stay one.
        self.unsigned_floor_errors(floors, ffi.is_empty());
        let spans: Vec<Span> = self
            .written_pointer_ints
            .values()
            .copied()
            .filter(|s| !inside(s))
            .collect();
        // `let n = k as usize;` over an `i64` `k`: the cast goes, and `n`
        // is the `i64` everything else gives. Not in a program that declares
        // FFI items, where `n` may be what an `extern` function is given.
        //
        // Every other `as usize` goes the same way, so no cast is left to
        // feed a `usize` into a type this pass makes `i64` (B-2026-11-08-49).
        // A `let`'s initializer is collected twice; its fix is made once.
        let mut lets = std::mem::take(&mut self.usize_cast_lets);
        let mut seen = std::collections::HashSet::new();
        lets.retain(|e| seen.insert((e.span.offset, e.span.length)));
        for e in lets.into_iter().filter(|_| ffi.is_empty()) {
            // Over any other integer (a `u64`, or a `usize` this pass is
            // about to make `i64`), the cast's target becomes `i64`.
            let to_i64 = || match &e.kind {
                ExprKind::Cast { ty, .. }
                    if matches!(&ty.kind, TypeKind::Path(p)
                        if p.generic_args.is_none()
                            && matches!(p.segments.as_slice(), [n] if n == "usize" || n == "isize")) =>
                {
                    Some(super::FixIt {
                        span: ty.span,
                        replacement: "i64".to_string(),
                    })
                }
                _ => None,
            };
            // `pos[(b - b'a') as usize]` over a `u8`: the index takes the
            // `i64` the `u8` widens to by itself, so the whole cast goes,
            // with its parentheses, rather than becoming an `as i64` that a
            // second round drops (B-2026-11-15-50).
            let key = crate::resolver::SpanKey::from_span(&e.span);
            if self.program.index_casts.contains(&key) {
                if let Some(edits) = self.narrow_usize_cast_removal(&e) {
                    self.type_error(
                        "this `as usize` makes a `usize` that mixes with `i64`, and `usize` \
                         is for FFI signatures: Kāra writes sizes, lengths, counts and \
                         indices as `i64` (design.md §5), and the index takes the `i64` \
                         this value widens to by itself, so drop the `as usize`"
                            .to_string(),
                        e.span,
                        TypeErrorKind::TypeMismatch,
                    );
                    self.error_fix_diffs.insert(key, edits);
                    continue;
                }
            }
            if let Some(fix) = self.usize_cast_of_an_i64(&e).or_else(to_i64) {
                let remedy = super::expr_ops::usize_cast_remedy(&fix);
                self.type_error_with_fix_it(
                    format!(
                        "this `as usize` makes a `usize` that mixes with `i64`, and `usize` \
                         is for FFI signatures: Kāra writes sizes, lengths, counts and \
                         indices as `i64` (design.md §5), so {remedy}"
                    ),
                    e.span,
                    TypeErrorKind::TypeMismatch,
                    fix,
                );
            }
        }
        let suffixes: Vec<Span> = self
            .written_pointer_int_suffixes
            .values()
            .copied()
            .filter(|s| !inside(s))
            .collect();
        for span in suffixes {
            self.type_error_with_fix_it(
                "this `usize` or `isize` suffix makes a pointer-sized integer that mixes \
                 with `i64`, and those are for FFI signatures: Kāra writes sizes, lengths, \
                 counts and indices as `i64` (design.md §5), so drop the suffix"
                    .to_string(),
                span,
                TypeErrorKind::TypeMismatch,
                super::FixIt {
                    span,
                    replacement: String::new(),
                },
            );
        }
        for span in spans {
            self.type_error_with_fix_it(
                "this type mixes with `i64`, and `usize` and `isize` are for FFI \
                 signatures: Kāra writes sizes, lengths, counts and indices as `i64` \
                 (design.md §5), so write `i64` here"
                    .to_string(),
                span,
                TypeErrorKind::TypeMismatch,
                super::FixIt {
                    span,
                    replacement: "i64".to_string(),
                },
            );
        }
        // A root fixed here makes each use's `as i64` redundant: the use
        // keeps its error and its edit waits for a pass where nothing it
        // reads from is being fixed (B-2026-11-08-16).
        if self.errors.len() > errors_before {
            for key in std::mem::take(&mut self.usize_use_site_fixes) {
                self.error_fix_diffs.remove(&key);
            }
            // The same for a literal binding declared a `usize` or `isize`
            // where it meets one (`let mut mouse = 0;` then `mouse = i`
            // with `i: usize`): this pass makes that `usize` an `i64`, the
            // literal's own default, so the declaration would only be
            // turned to a redundant `: i64` next pass (kata 1728's Rust
            // arm, B-2026-11-20-49). Its error stays, with no edit, and is gone
            // next pass.
            for i in 0..errors_before {
                let pointer_sized = self.errors[i].fix_it.as_ref().is_some_and(|f| {
                    matches!(
                        f.replacement.as_str(),
                        ": usize" | ": isize" | "usize" | "isize"
                    )
                });
                if pointer_sized {
                    self.errors[i].fix_it = None;
                    let key = crate::resolver::SpanKey::from_span(&self.errors[i].span);
                    self.error_fix_diffs.remove(&key);
                }
            }
        }
    }

    /// The `usize` floors the retype to `i64` would lose (kata 1219's Rust
    /// arm): `i.checked_sub(1)` is `None` at 0 on a `usize` and `Some(-1)`
    /// on an `i64`, and `i.saturating_sub(1)` stops at 0 on one and not on
    /// the other. With `retyped`, each gets the edit that keeps the floor,
    /// in the pass that makes the `usize` an `i64`; and `checked_add_signed`
    /// / `saturating_add_signed`, which `i64` lacks, become its `checked_add`
    /// / `saturating_add` with the floor. Without it, those two are an error
    /// with no edit, as any method the type does not have.
    fn unsigned_floor_errors(&mut self, floors: Vec<(String, Span, Span, Span)>, retyped: bool) {
        for (method, name, close, call) in floors {
            let signed = method.ends_with("_signed");
            if !retyped {
                if signed {
                    self.type_error(
                        format!("no method '{method}' on type 'usize'"),
                        call,
                        TypeErrorKind::NoMethodFound,
                    );
                }
                continue;
            }
            let checked = method.starts_with("checked_");
            let wrapping = method == "wrapping_sub";
            let floor = if checked {
                ".filter(|v| v >= 0)"
            } else if wrapping {
                ".rem_euclid(i64.MAX)"
            } else {
                ".max(0)"
            };
            let mut edits = Vec::new();
            let message = if signed {
                let to = if checked {
                    "checked_add"
                } else {
                    "saturating_add"
                };
                edits.push(crate::resolver::TextEdit {
                    offset: name.offset,
                    length: name.length,
                    replacement: to.to_string(),
                });
                format!(
                    "this `usize` becomes `i64` (design.md §5), which has no `{method}`: \
                     its `{to}` takes the `i64` the `isize` becomes, and `{floor}` keeps \
                     the floor at 0 the `usize` had"
                )
            } else if wrapping {
                format!(
                    "this `usize` becomes `i64` (design.md §5), and an `i64`'s `{method}` \
                     goes below 0 where the `usize` one wrapped past every length, so \
                     `{floor}` keeps it past every length (a value of 0 or more is unchanged)"
                )
            } else if checked {
                format!(
                    "this `usize` becomes `i64` (design.md §5), and an `i64`'s `{method}` \
                     gives `Some` of a number below 0 where the `usize` one gave `None`, \
                     so `{floor}` keeps the floor"
                )
            } else {
                format!(
                    "this `usize` becomes `i64` (design.md §5), and an `i64`'s `{method}` \
                     goes below 0 where the `usize` one stopped at 0, so `{floor}` keeps \
                     the floor"
                )
            };
            edits.push(crate::resolver::TextEdit {
                offset: close.offset + close.length,
                length: 0,
                replacement: floor.to_string(),
            });
            self.type_error(message, call, TypeErrorKind::TypeMismatch);
            self.error_fix_diffs
                .insert(crate::resolver::SpanKey::from_span(&call), edits);
        }
    }

    /// A `s.parse()` a closure's tail gives, checked against a return type
    /// the callee leaves open (`map`'s `OutV`), took that parameter as its
    /// target; the tail is then opened to a variable for a later use to fix
    /// (`collect()` into a `Vec[i32]`), and the parse follows it, so its
    /// error can name the type (`i32.parse(s)`) as it does after a `let`
    /// (kata 1154's Rust arm).
    pub(super) fn reopen_closure_parses(&mut self, from: usize, closed: &Type, opened: &Type) {
        let Type::TypeParam(param) = self.resolved_now(closed) else {
            return;
        };
        let Type::TypeVar(open) = self.resolved_now(opened) else {
            return;
        };
        for (_, target) in self.untargeted_parses.iter().skip(from) {
            let mut last = None;
            let mut cur = target.clone();
            while let Type::TypeVar(id) = cur {
                last = Some(id);
                match self.env.substitutions.get(&id) {
                    Some(next) => cur = next.clone(),
                    None => break,
                }
            }
            match (last, cur) {
                (Some(id), Type::TypeParam(p)) if p == param && id != open => {
                    self.env.substitutions.insert(id, Type::TypeVar(open));
                }
                _ => {}
            }
        }
    }

    /// The error for each `s.parse()` nothing named the target of where it
    /// was written: with the fix `T.parse(s)` when a later use fixed the
    /// target to a number type `T.parse` reads, and as the plain "name the
    /// type" error otherwise (B-2026-10-12-5, kata 1058).
    pub(super) fn report_untargeted_parses(&mut self) {
        let none: std::collections::HashMap<super::types::TypeVarId, String> = Default::default();
        let no_consts: std::collections::HashMap<super::types::ConstVarId, String> =
            Default::default();
        for (span, target) in std::mem::take(&mut self.untargeted_parses) {
            let target = super::inference::resolve_type_vars(
                &target,
                &self.env.substitutions,
                &none,
                &self.env.const_substitutions,
                &no_consts,
            );
            let name = match &target {
                Type::Int(_) | Type::UInt(_) | Type::Float(_) => super::type_display(&target),
                _ => String::new(),
            };
            let readable = matches!(
                name.as_str(),
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "f64"
            );
            let edit = self
                .program
                .parse_target_edits
                .get(&span.offset)
                .filter(|e| readable && e.length == span.length);
            match edit {
                Some(edit) => {
                    let call = format!("{name}.parse({})", edit.replacement);
                    self.type_error_with_fix_it(
                        format!(
                            "`.parse()` on a string takes its target from an expected \
                             `Option[T]`; here only a later use names it (`{name}`), so name \
                             the type: `{call}`"
                        ),
                        span,
                        TypeErrorKind::NoMethodFound,
                        super::FixIt {
                            span,
                            replacement: call,
                        },
                    );
                }
                None => self.type_error(
                    "`.parse()` on a string takes its target from an expected `Option[T]` \
                     (`let n: Option[i64] = s.parse();`); with none here, name the type: \
                     `i64.parse(s)`"
                        .to_string(),
                    span,
                    TypeErrorKind::NoMethodFound,
                ),
            }
        }
    }
}
