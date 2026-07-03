//! Layout-query intrinsics for the tree-walking interpreter
//! (`karac run`): `offset_of[T](field.path)`, `size_of[T]()`,
//! `align_of[T]()`.
//!
//! Codegen lowers `offset_of` to a `usize` constant by chaining
//! `TargetData::offset_of_element` over the LLVM struct types built by
//! `llvm_type_for_type_expr` (see `src/codegen/exprs.rs::compile_offset_of`),
//! and `size_of` / `align_of` to the type's alloc size / ABI alignment
//! (`compile_layout_query_intrinsic` in `src/codegen/call_dispatch.rs`).
//! The interpreter has no TargetData, so this module re-derives the
//! same natural-alignment layout from the AST: each field is placed at
//! the next multiple of its ABI alignment; aggregate size is padded to
//! a multiple of the largest field alignment. Per-field size/alignment
//! mirrors `llvm_type_for_type_expr` / `llvm_type_for_name` arm-for-arm
//! (String/Vec = `{ptr,i64,i64}` = 24 bytes, Slice = 16, opaque heap
//! handles = `ptr` = 8, enums = `{i64 tag, i64 × payload-words}` via
//! the `payload_word_count_for_type_expr` word rules, shared types =
//! `ptr`, unknown names = the `i64` fall-through) so `karac run` and
//! `karac build` report identical offsets. Both native targets Kāra
//! compiles for (x86-64, aarch64) agree on every mirrored rule — none
//! of these types has target-divergent size or alignment.

use crate::ast::*;
use crate::token::Span;

use super::value::Value;

/// Byte size + ABI alignment of one lowered field type.
#[derive(Clone, Copy)]
struct SizeAlign {
    size: u64,
    align: u64,
}

/// A single `i64`/pointer word — the layout of every 8-byte scalar and
/// of codegen's unknown-name `i64` fall-through.
const WORD: SizeAlign = SizeAlign { size: 8, align: 8 };

/// Recursion cap for nested aggregates. Mutually-recursive value
/// structs are rejected upstream (infinite size), but evaluation must
/// not stack-overflow if a broken program reaches this point.
const MAX_LAYOUT_DEPTH: u32 = 64;

fn align_up(v: u64, align: u64) -> u64 {
    if align <= 1 {
        v
    } else {
        v.div_ceil(align) * align
    }
}

fn first_type_arg(args: Option<&[GenericArg]>) -> Option<&TypeExpr> {
    match args?.first()? {
        GenericArg::Type(t) => Some(t),
        _ => None,
    }
}

/// Recover a `TypeExpr` from a type name in expression position (the
/// bracket operand of the `Call(Index(...))` layout-query shape).
/// Mirrors codegen's `expr_as_type_expr_codegen`.
fn expr_as_type_expr_interp(expr: &Expr) -> Option<TypeExpr> {
    match &expr.kind {
        ExprKind::Identifier(name) => Some(TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![name.clone()],
                generic_args: None,
                span: expr.span.clone(),
            }),
            span: expr.span.clone(),
        }),
        ExprKind::Path {
            segments,
            generic_args,
        } => Some(TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: segments.clone(),
                generic_args: generic_args.clone(),
                span: expr.span.clone(),
            }),
            span: expr.span.clone(),
        }),
        _ => None,
    }
}

fn type_expr_is_bool(ty: &TypeExpr) -> bool {
    matches!(&ty.kind, TypeKind::Path(p)
        if p.segments.len() == 1 && p.segments[0] == "bool" && p.generic_args.is_none())
}

/// Second generic arg of `Array[T, N]` / `Vector[T, N]` as a length.
/// Integer literals only — a const-generic param reference is not
/// resolvable here, matching codegen's `llvm_array_type` returning
/// `None` (which falls through to the `i64` default type).
fn const_arg_len(args: &[GenericArg]) -> Option<u64> {
    match args.get(1)? {
        GenericArg::Const(expr) => match &expr.kind {
            ExprKind::Integer(n, _) if *n >= 0 => Some(*n as u64),
            _ => None,
        },
        _ => None,
    }
}

impl<'a> super::Interpreter<'a> {
    /// Evaluate `offset_of[T](field.path)` — the interpreter twin of
    /// codegen's `compile_offset_of`. The typechecker has already
    /// validated the target and every path segment; failures here are
    /// layout-model gaps surfaced as runtime errors, not panics.
    pub(crate) fn eval_offset_of(
        &mut self,
        ty: &TypeExpr,
        field_path: &[String],
        span: &Span,
    ) -> Value {
        match self.offset_of_value(ty, field_path) {
            Ok(offset) => Value::Int(offset as i64),
            Err(msg) => self.record_runtime_error(msg, span),
        }
    }

    /// Evaluate `size_of[T]()` / `align_of[T]()` — the interpreter twin
    /// of codegen's `compile_layout_query_intrinsic` (alloc size / ABI
    /// alignment of the lowered type).
    pub(crate) fn eval_layout_query(&mut self, name: &str, ty: &TypeExpr, span: &Span) -> Value {
        match self.field_layout(ty, 0) {
            Ok(sa) => Value::Int(if name == "size_of" {
                sa.size as i64
            } else {
                sa.align as i64
            }),
            Err(msg) => self.record_runtime_error(msg, span),
        }
    }

    /// Recognize the two parse shapes of a zero-arg layout query and
    /// extract the intrinsic name plus target type. `size_of[T]()`
    /// parses as `Call(Index(Ident, T-expr))` (the parser treats a
    /// single bracketed arg at expression position as indexing — same
    /// shape `with_provider` handles); explicit generic-args parses as
    /// `Call(Path { segments: [name], generic_args })`. Mirrors the
    /// twin intercepts in codegen's `compile_call` and the
    /// typechecker's `infer_call`.
    ///
    /// The type slot is `None` when the callee IS a layout query but
    /// its bracket operand isn't recoverable as a type name (e.g. the
    /// nested-generic `size_of[Vec[i64]]()`, which the typechecker
    /// rejects with `E_LAYOUT_QUERY_TYPE_ARG_REQUIRED`). `karac run`
    /// tolerates typecheck errors, so the intercept must still claim
    /// the call and degrade to a runtime error — falling through to
    /// normal dispatch would panic on the `size_of` variable lookup.
    pub(crate) fn match_layout_query(callee: &Expr) -> Option<(String, Option<TypeExpr>)> {
        match &callee.kind {
            ExprKind::Index { object, index } => {
                let ExprKind::Identifier(name) = &object.kind else {
                    return None;
                };
                if name != "size_of" && name != "align_of" {
                    return None;
                }
                Some((name.clone(), expr_as_type_expr_interp(index)))
            }
            ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } if segments.len() == 1
                && (segments[0] == "size_of" || segments[0] == "align_of") =>
            {
                match ga.as_slice() {
                    [GenericArg::Type(te)] => Some((segments[0].clone(), Some(te.clone()))),
                    _ => Some((segments[0].clone(), None)),
                }
            }
            _ => None,
        }
    }

    fn offset_of_value(&self, ty: &TypeExpr, field_path: &[String]) -> Result<u64, String> {
        let TypeKind::Path(path) = &ty.kind else {
            return Err("offset_of: target must be a path-named struct".to_string());
        };
        let mut current_struct_name = path
            .segments
            .last()
            .cloned()
            .ok_or_else(|| "offset_of: empty type path".to_string())?;
        let mut total_offset: u64 = 0;
        for segment_name in field_path {
            let def = self.find_struct_def(&current_struct_name).ok_or_else(|| {
                format!(
                    "offset_of: struct '{current_struct_name}' has no layout \
                     the interpreter can compute"
                )
            })?;
            if def.is_shared || def.is_par {
                // Codegen rejects these too (shared types live behind an
                // RC heap pointer, not in `struct_types`).
                return Err(format!(
                    "offset_of: '{current_struct_name}' is a shared type; \
                     offset_of applies to value-struct layouts"
                ));
            }
            let mut cursor: u64 = 0;
            let mut found: Option<(u64, &TypeExpr)> = None;
            for f in &def.fields {
                let fl = self.field_layout(&f.ty, 0)?;
                cursor = align_up(cursor, fl.align);
                if &f.name == segment_name {
                    found = Some((cursor, &f.ty));
                    break;
                }
                cursor += fl.size;
            }
            let Some((offset, field_ty)) = found else {
                return Err(format!(
                    "offset_of: field '{segment_name}' not found on struct \
                     '{current_struct_name}'"
                ));
            };
            total_offset += offset;
            // Chase the field's type for the next segment (codegen walks
            // its `struct_field_type_names` table the same way). The
            // typechecker guarantees non-final segments land on structs.
            if let TypeKind::Path(p) = &field_ty.kind {
                if let Some(next) = p.segments.last() {
                    current_struct_name = next.clone();
                }
            }
        }
        Ok(total_offset)
    }

    /// Size/alignment of one field type — the layout twin of
    /// `llvm_type_for_type_expr`.
    fn field_layout(&self, ty: &TypeExpr, depth: u32) -> Result<SizeAlign, String> {
        if depth > MAX_LAYOUT_DEPTH {
            return Err(
                "offset_of: type nesting too deep (recursive value struct?)".to_string(),
            );
        }
        Ok(match &ty.kind {
            TypeKind::Path(path) => {
                let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                return self.named_layout(name, path.generic_args.as_deref(), depth);
            }
            // Unit type — codegen gives it an i64 zero slot.
            TypeKind::Tuple(elems) if elems.is_empty() => WORD,
            TypeKind::Tuple(elems) => self.aggregate_layout(elems.iter(), depth)?,
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::Pointer { .. } => WORD,
            TypeKind::MutSlice(_) => SizeAlign { size: 16, align: 8 },
            // First-class fn value: `{fn_ptr, env_ptr}` fat pointer.
            TypeKind::FnType { .. } => SizeAlign { size: 16, align: 8 },
            // Everything else mirrors codegen's i64 fall-through.
            _ => WORD,
        })
    }

    /// Natural (non-packed) aggregate layout over an ordered field list.
    fn aggregate_layout<'t>(
        &self,
        field_tys: impl Iterator<Item = &'t TypeExpr>,
        depth: u32,
    ) -> Result<SizeAlign, String> {
        let mut size: u64 = 0;
        let mut align: u64 = 1;
        for fty in field_tys {
            let fl = self.field_layout(fty, depth + 1)?;
            size = align_up(size, fl.align) + fl.size;
            align = align.max(fl.align);
        }
        Ok(SizeAlign {
            size: align_up(size, align),
            align,
        })
    }

    /// The layout twin of `llvm_type_for_name` (plus the generic-arg
    /// shapes `llvm_type_for_type_expr` special-cases before reaching
    /// it: Array / Vector / Atomic / Mutex / Vec-family).
    fn named_layout(
        &self,
        name: &str,
        generic_args: Option<&[GenericArg]>,
        depth: u32,
    ) -> Result<SizeAlign, String> {
        // Refinement / distinct aliases are layout-identical to their
        // base (codegen's `refinement_bases` / `distinct_bases`).
        // Baked-stdlib distinct types (e.g. ExitCode) are not visible
        // here; they fall to the unknown-name WORD arm below, which
        // matches their i64 bases.
        for item in &self.program.items {
            match item {
                Item::TypeAlias(t) if t.name == name => {
                    return self.field_layout(&t.ty, depth + 1);
                }
                Item::DistinctType(d) if d.name == name => {
                    return self.field_layout(&d.base_type, depth + 1);
                }
                _ => {}
            }
        }
        Ok(match name {
            "i8" | "u8" | "bool" => SizeAlign { size: 1, align: 1 },
            "i16" | "u16" => SizeAlign { size: 2, align: 2 },
            "i32" | "u32" | "char" | "f32" => SizeAlign { size: 4, align: 4 },
            "i64" | "u64" | "isize" | "usize" | "f64" => WORD,
            // `{ptr, i64 len, i64 cap}` — the String/Vec ABI, shared by
            // StringSlice (borrowed view) and VecDeque.
            "String" | "str" | "StringSlice" | "Vec" | "VecDeque" => {
                SizeAlign { size: 24, align: 8 }
            }
            "Slice" => SizeAlign { size: 16, align: 8 },
            // Opaque heap-pointer handles.
            "Map" | "Set" | "SortedSet" | "SortedMap" | "Tensor" | "Column" | "DataFrame"
            | "Request" | "File" | "Sender" | "Receiver" | "Channel" => WORD,
            // Baked single-i64-field handle structs.
            "TcpListener" | "TcpStream" | "WebSocket" | "TaskGroup" | "TaskHandle"
            | "TlsStream" => WORD,
            // `{i64 fd, ptr config}`.
            "TlsListener" => SizeAlign { size: 16, align: 8 },
            "Array" => match (first_type_arg(generic_args), generic_args.and_then(const_arg_len))
            {
                (Some(elem), Some(n)) => {
                    let el = self.field_layout(elem, depth + 1)?;
                    // LLVM array stride is the element's alloc size,
                    // which aggregate_layout already align-pads.
                    SizeAlign {
                        size: el.size * n,
                        align: el.align,
                    }
                }
                // Unresolvable args → codegen's i64 default.
                _ => WORD,
            },
            "Vector" => match (first_type_arg(generic_args), generic_args.and_then(const_arg_len))
            {
                (Some(elem), Some(n)) => {
                    let el = self.field_layout(elem, depth + 1)?;
                    // LLVM's default datalayout aligns a SIMD vector to
                    // its pow2-rounded byte size; alloc size rounds up
                    // the same way (e.g. <3 x f32>: store 12, alloc 16).
                    let bytes = (el.size * n).max(1).next_power_of_two();
                    SizeAlign {
                        size: bytes,
                        align: bytes,
                    }
                }
                _ => WORD,
            },
            // `Atomic[T]` is transparent over T; `Atomic[bool]` widens
            // to i8 (LLVM rejects atomic i1 loads/stores).
            "Atomic" => match first_type_arg(generic_args) {
                Some(inner) if type_expr_is_bool(inner) => SizeAlign { size: 1, align: 1 },
                Some(inner) => self.field_layout(inner, depth + 1)?,
                None => WORD,
            },
            // `Mutex[T]` — `{i64 lockflag, T value}`.
            "Mutex" => match first_type_arg(generic_args) {
                Some(inner) => {
                    let il = self.field_layout(inner, depth + 1)?;
                    let align = il.align.max(8);
                    let value_off = align_up(8, il.align);
                    SizeAlign {
                        size: align_up(value_off + il.size, align),
                        align,
                    }
                }
                None => WORD,
            },
            other => {
                // User-declared types, in `llvm_type_for_name`'s order:
                // shared → RC heap ptr; struct → field aggregate; union
                // → max-size/max-align storage; enum → tagged i64-word
                // struct; then the seeded builtin enums; then the i64
                // fall-through for anything still unknown.
                if let Some(s) = self.find_struct_def(other) {
                    if s.is_shared || s.is_par {
                        WORD
                    } else {
                        self.aggregate_layout(s.fields.iter().map(|f| &f.ty), depth)?
                    }
                } else if let Some(u) = self.find_union_def_for_layout(other) {
                    let mut size: u64 = 0;
                    let mut align: u64 = 1;
                    for f in &u.fields {
                        let fl = self.field_layout(&f.ty, depth + 1)?;
                        size = size.max(fl.size);
                        align = align.max(fl.align);
                    }
                    SizeAlign {
                        size: align_up(size, align),
                        align,
                    }
                } else if let Some(e) = self.find_enum_def_for_layout(other) {
                    if e.is_shared || e.is_par {
                        WORD
                    } else {
                        let max_words = e
                            .variants
                            .iter()
                            .map(|v| self.variant_payload_words(v, depth))
                            .max()
                            .unwrap_or(0);
                        SizeAlign {
                            size: 8 * (1 + max_words),
                            align: 8,
                        }
                    }
                } else if let Some(sa) = seeded_builtin_enum_layout(other) {
                    sa
                } else {
                    WORD
                }
            }
        })
    }

    fn find_union_def_for_layout(&self, name: &str) -> Option<&UnionDef> {
        self.program.items.iter().find_map(|item| match item {
            Item::UnionDef(u) if u.name == name => Some(u),
            _ => None,
        })
    }

    fn find_enum_def_for_layout(&self, name: &str) -> Option<&EnumDef> {
        self.program.items.iter().find_map(|item| match item {
            Item::EnumDef(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    /// Total payload words of one variant — Σ over its fields of the
    /// word-count rules below.
    fn variant_payload_words(&self, v: &Variant, depth: u32) -> u64 {
        let field_tys: Vec<&TypeExpr> = match &v.kind {
            VariantKind::Unit => Vec::new(),
            VariantKind::Tuple(tys) => tys.iter().collect(),
            VariantKind::Struct(fields) => fields.iter().map(|f| &f.ty).collect(),
        };
        field_tys
            .iter()
            .map(|t| self.payload_word_count(t, depth + 1))
            .sum()
    }

    /// The word-count twin of codegen's
    /// `payload_word_count_for_type_expr`: primitives/pointers 1,
    /// String/Vec 3, Slice 2, tuples and user structs sum recursively,
    /// everything else (shared types, nested enums — rejected upstream —
    /// and unknown names) a conservative 1.
    fn payload_word_count(&self, ty: &TypeExpr, depth: u32) -> u64 {
        if depth > MAX_LAYOUT_DEPTH {
            return 1;
        }
        match &ty.kind {
            TypeKind::Path(path) => {
                let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                match name {
                    "String" | "Vec" => 3,
                    "Slice" => 2,
                    "Map" | "Set" | "SortedSet" | "SortedMap" => 1,
                    "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    | "isize" | "f32" | "f64" | "bool" | "char" | "Unit" => 1,
                    other => match self.find_struct_def(other) {
                        Some(s) if !s.is_shared && !s.is_par => s
                            .fields
                            .iter()
                            .map(|f| self.payload_word_count(&f.ty, depth + 1))
                            .sum(),
                        _ => 1,
                    },
                }
            }
            TypeKind::Tuple(elems) if elems.is_empty() => 1,
            TypeKind::Tuple(elems) => elems
                .iter()
                .map(|t| self.payload_word_count(t, depth + 1))
                .sum(),
            TypeKind::Ref(_) | TypeKind::MutRef(_) => 1,
            TypeKind::MutSlice(_) => 2,
            _ => 1,
        }
    }
}

/// Layouts of the enums `seed_builtin_enum_layouts` registers without
/// AST definitions: `{i64 tag, i64 × payload-words}` shapes copied
/// from the seed constructors in `src/codegen/declarations.rs`.
fn seeded_builtin_enum_layout(name: &str) -> Option<SizeAlign> {
    let total_words: u64 = match name {
        // tag + 3 payload words.
        "Option" | "Json" | "IoError" | "Utf8Error" => 4,
        // tag + 5 payload words.
        "Result" => 6,
        // tag + 1 payload word.
        "AllocError" | "TcpError" | "TlsError" => 2,
        // tag only.
        "Ordering" | "VarError" | "ChannelError" | "OnFull" => 1,
        _ => return None,
    };
    Some(SizeAlign {
        size: 8 * total_words,
        align: 8,
    })
}
