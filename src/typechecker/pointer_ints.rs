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
            _ => self.note_pointer_int_mix(a, b),
        }
    }

    /// The fixes, one error each, when the program mixed the two.
    pub(super) fn emit_written_pointer_int_fixes(&mut self) {
        if !self.pointer_int_mixed {
            return;
        }
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
        let spans: Vec<Span> = self
            .written_pointer_ints
            .values()
            .copied()
            .filter(|s| !inside(s))
            .collect();
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
    }
}
