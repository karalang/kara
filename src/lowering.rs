//! AST lowering pass: rewrites operator expressions into trait-method calls.
//!
//! Runs between typecheck and effectcheck. Consumes the typechecker's
//! `expr_types` side-table to resolve operand types, then rewrites
//! `BinOp` / `UnaryOp` nodes into `Call(Path([target_type, method]), args)`
//! shape. Downstream phases (effect, ownership, interpreter, codegen) see
//! a uniform Call shape — no special-case handling for "is this a primitive
//! op" is needed at semantic-analysis layers.
//!
//! v2 scope: arithmetic (`+ - * / %`), unary `Neg`, equality (`== !=`),
//! comparison (`< <= > >=`), bitwise (`& | ^ << >> ~`), and unary `Not` are
//! lowered. Logical `&&` / `||` stay as `BinOp` — short-circuit semantics
//! can't be faithfully expressed as a trait-method call (evaluating both
//! arms eagerly would change program meaning). Range operators stay as
//! `BinOp` too (they construct Range types, not trait calls).

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::{
    ConstArg, DimArg, FloatSize, IntSize, Type, TypeCheckResult, TypeChecker, UIntSize,
};

/// The set of method names this pass desugars comparison operators into.
/// Each is a relational-trait method (`PartialEq::eq`/`ne`, `Ord`/`PartialOrd`
/// `lt`/`le`/`gt`/`ge`/`cmp`/`partial_cmp`) whose canonical signature borrows
/// both operands (`ref self, other: ref Self` — see `partial_eq.kara` and
/// design.md § comparisons borrow). Ownership/RC classification uses this to
/// treat the desugared `Type.eq(a, b)` call's args as reads, not consumes:
/// the lowered call is a free-function `Call(Path([Type, method]), [recv,
/// arg])` targeting an *instance* method, so it never picks up a param-mode
/// entry from `collect_callee_param_modes` (static-methods-only) and its args
/// otherwise fall to the consume-everything default (B-2026-07-02-23).
pub fn is_relational_operator_method(name: &str) -> bool {
    matches!(
        name,
        "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "cmp" | "partial_cmp"
    )
}

/// Whether `callee` is the `Path([Type, method])` produced by lowering a
/// comparison operator — i.e. a two-segment path whose final segment is a
/// relational-trait method (see [`is_relational_operator_method`]). The
/// desugared shape is always binary (`Type.eq(a, b)`), so both argument
/// positions are borrows. `false` for any other callee, including bare
/// identifiers and non-relational associated calls.
pub fn callee_is_relational_operator(callee: &Expr) -> bool {
    match &callee.kind {
        ExprKind::Path { segments, .. } => {
            segments.len() == 2 && is_relational_operator_method(&segments[1])
        }
        _ => false,
    }
}

/// Rewrite operator expressions across the entire program in place.
pub fn lower_program(program: &mut Program, tc: &TypeCheckResult) {
    let mut lowerer = Lowerer { tc };
    for item in &mut program.items {
        lowerer.lower_item(item);
    }
    // Forward the typechecker's `?` cross-error-type conversion table onto
    // the program so codegen can read it without taking a `TypeCheckResult`.
    // Keys are `(span.offset, span.length)`; values are the target type's
    // canonical name (e.g. `"AppError"`). Codegen emits `Target.from(e)`
    // before the propagation early-return when an entry exists.
    program.question_conversions = tc
        .question_conversions
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Forward the method-call → `Type.method` callee key table so codegen
    // can narrow the par-branch cancel check at instance method sites.
    // The typechecker populates this directly because the parser sets
    // `MethodCall.span == receiver.span`, so a generic post-hoc walk
    // against `expr_types` would race with the return-type insertion at
    // the same key. See typechecker `infer_method_call`.
    program.method_callee_types = tc
        .method_callee_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Forward the Option/Result unwrap-family inner-type table so codegen's
    // `compile_method_call` arm for `unwrap`/`expect`/`is_*` knows the
    // LLVM shape of the value to reconstitute from the Option/Result
    // payload words. Sibling to `method_callee_types`; same keying.
    program.method_unwrap_inner_types = tc
        .method_unwrap_inner_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Forward the slice-3b fresh-temp `Vec`/`VecDeque` receiver element-type
    // table so codegen can materialize a non-identifier collection receiver
    // and re-dispatch element-type-aware read methods through
    // `compile_vec_method`. Sibling to `method_unwrap_inner_types`; same
    // keying (MethodCall span).
    program.temp_recv_elem_types = tc
        .temp_recv_elem_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Sibling table for `Map`/`Set` fresh-temp receivers — codegen materializes
    // the handle, registers K/V (or elem) for the redispatch, and drop-tracks
    // the handle (`FreeMapHandle`). Same keying (MethodCall span).
    program.temp_recv_mapset_types = tc
        .temp_recv_mapset_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Forward the channel-op element-type table so codegen's
    // `karac_runtime_channel_*` lowering knows the LLVM shape of `T` to size
    // the type-erased transfer + shape the recv/try_recv out slot. Sibling
    // to `method_unwrap_inner_types`; same keying (MethodCall span).
    program.channel_elem_types = tc
        .channel_elem_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // `Stats.<fn>` call-span -> slice element TypeExpr (i64 | f64) so the
    // codegen reduction reads the buffer at the right element type (S5).
    program.stats_elem_types = tc
        .stats_elem_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // `gpu.dispatch` kernel-arg span -> generated WGSL shader text, so codegen
    // can bake the constant and call the runtime GPU dispatch symbol without
    // re-walking the AST (spike slice-0c).
    program.gpu_dispatch_wgsl = tc
        .gpu_dispatch_wgsl
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Forward the `TaskHandle[T].join()` result-type table so codegen sizes
    // the cross-task result transfer for a non-scalar `T` (a `Vec`/`String`/
    // struct spawn return). Sibling to `channel_elem_types`; same keying.
    program.task_join_return_types = tc
        .task_join_return_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Inner type of every borrow-typed (`ref T` / `mut ref T`) expression,
    // keyed by span. A method call shares its receiver's span and the
    // call's *result* type is the last write at that key, so a borrow-
    // returning `u.name()` lands here keyed by the receiver span — exactly
    // the key the codegen let-arm looks up to bind the result as a
    // ref-local (method-ref half of B-2026-06-07-5; free-fn calls key off
    // `fn_ref_return_inner` instead, which is name-addressable).
    program.ref_return_inner_types = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| match ty {
            Type::Ref(inner) | Type::MutRef(inner) => {
                Some(((k.0, k.1), TypeChecker::type_to_type_expr(inner)))
            }
            _ => None,
        })
        .collect();
    // Forward the pattern-binding type table so codegen can reconstitute
    // struct payloads (single-field error wrappers, etc.) from the i64
    // word at match-arm bind sites. Without this, `Err(e) => e.field`
    // can't dispatch through the struct shape because `e` was bound as
    // i64. See `bind_pattern_values` in src/codegen.rs.
    program.pattern_binding_types = tc
        .pattern_binding_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // PB sibling slice (2026-05-09): forward the inner element-type table
    // for `Vec[T]` / `Slice[T]` pattern bindings so codegen can populate
    // `vec_elem_types` / `slice_elem_types` keyed by the binding's variable
    // name and route direct method dispatch on the binding through the
    // right element-typed path. See `bind_pattern_values` in src/codegen.rs.
    program.pattern_binding_inner_types = tc
        .pattern_binding_inner_types
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Forward the spans of every `Type::Str`-typed expression so codegen
    // can distinguish String values from Vec[T] / other 3-word types that
    // share the LLVM `{ptr, i64, i64}` shape. First consumer:
    // `emit_sort_by_key_inline_thunk`'s String-key dispatch arm. The set
    // is the cheaper sibling of plumbing the full `expr_types` map onto
    // Program — codegen only needs the discriminator bit, not the full
    // type representation. Add other small focused side-tables here if
    // future codegen paths need to identify other shape-sharing types.
    program.string_typed_exprs = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| {
            if matches!(ty, Type::Str) {
                Some((k.0, k.1))
            } else {
                None
            }
        })
        .collect();
    // For every `Fn(..)` / `OnceFn(..)`-typed expression, record its `FnType`
    // TypeExpr so codegen can recover a first-class fn value's signature from
    // the expression alone — e.g. an un-annotated `let g = h.f;` reading a
    // `Fn(..)`-typed struct field — and register the binding for indirect calls
    // (B-2026-06-21-3). A borrow-wrapped fn value (`ref Fn(..)`) unwraps to the
    // same callable signature.
    program.fn_value_typed_exprs = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| {
            let core = match ty {
                Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                other => other,
            };
            match core {
                Type::Function { .. } | Type::OnceFunction { .. } => {
                    Some(((k.0, k.1), TypeChecker::type_to_type_expr(core)))
                }
                _ => None,
            }
        })
        .collect();
    // Fold in the closure-VALUE call-callee `Fn` types the typechecker stashed
    // in a dedicated map. For a `(h.f)(x)` / `v[i](x)` / `(t.0)(x)` callee the
    // parser-shared span means `expr_types` holds the call's *result* type at
    // the callee key (so the loop above misses it); these entries carry the
    // un-overwritten callee signature so codegen can lower the env-first
    // indirect call (B-2026-06-22-4).
    for (k, fn_ty_expr) in &tc.fn_value_callee_types {
        program
            .fn_value_typed_exprs
            .insert((k.0, k.1), fn_ty_expr.clone());
    }
    // Forward the per-call-site generic type-arg substitution so codegen's
    // `compile_generic_call` can bind type params the LLVM-type-based
    // inference can't recover — a container element type (`ref Vec[T]`)
    // whose `{ptr,len,cap}` shape is element-erased (B-2026-07-02-41). The
    // interpreter reads `tc.call_type_subs` directly; codegen takes only
    // `Program`, so mirror the map here.
    program.call_type_subs = tc
        .call_type_subs
        .iter()
        .map(|(k, v)| ((k.0, k.1), v.clone()))
        .collect();
    // Sibling to `string_typed_exprs`: for every `Tensor[T, Shape]`-typed
    // expression whose rank is statically known (concrete `Type::Shape`,
    // no `...` splice), record the element type (as a TypeExpr, lowered
    // by codegen via `llvm_type_for_type_expr`) and the per-dim static
    // values (`Some(n)` for concrete literals, `None` for `?` / dim
    // params / unresolved dim metavars — codegen reads those from the
    // tensor value's runtime header, which is always authoritative).
    // Splice-bearing / bare-param shapes are skipped: rank unknown, and
    // the only ops the typechecker admits on them read the header.
    program.tensor_typed_exprs = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| {
            // A borrow of a tensor (`ref Tensor` / `mut ref Tensor`) is the
            // same single heap pointer as the owned value, so a ref-tensor
            // expression is just as indexable / transformable — unwrap the
            // borrow so its span lands in the side-table too. The drop
            // decision (a borrow is never freed) is taken separately at the
            // binding site via `ref_return_inner_types`.
            let core = match ty {
                Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                other => other,
            };
            match core {
                Type::Named { name, args } if name == "Tensor" && args.len() == 2 => {
                    let Type::Shape(dim_args) = &args[1] else {
                        return None;
                    };
                    let mut dims = Vec::with_capacity(dim_args.len());
                    for d in dim_args {
                        match d {
                            DimArg::Splice(_) | DimArg::SpliceVar(_) => return None,
                            DimArg::Const(ConstArg::Literal(v)) => dims.push(Some(*v)),
                            _ => dims.push(None),
                        }
                    }
                    Some((
                        (k.0, k.1),
                        crate::ast::TensorTypeInfo {
                            elem: TypeChecker::type_to_type_expr(&args[0]),
                            dims,
                        },
                    ))
                }
                _ => None,
            }
        })
        .collect();
    // Column[T] (phase-11 data-science stdlib, Arrow commitment Q5):
    // map every `Column[T]`-typed expression's span to its element
    // `TypeExpr`. `Column` is always 1-D with a runtime length, so there
    // is no shape payload — unlike the tensor table above. A borrow of a
    // column (`ref Column` / `mut ref Column`) is the same single control
    // pointer as the owned value, so unwrap the borrow so its span lands
    // here too (the drop decision — a borrow is never freed — is taken
    // separately at the binding site).
    program.column_typed_exprs = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| {
            let core = match ty {
                Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                other => other,
            };
            match core {
                Type::Named { name, args } if name == "Column" && args.len() == 1 => Some((
                    (k.0, k.1),
                    crate::ast::ColumnTypeInfo {
                        elem: TypeChecker::type_to_type_expr(&args[0]),
                    },
                )),
                _ => None,
            }
        })
        .collect();
    // Sibling to `string_typed_exprs`: spans of every `Vector[T, N]`-typed
    // expression whose element is an unsigned integer. The LLVM `<N x iX>`
    // lane type is signless, so codegen consults this set to pick the
    // unsigned compare predicate (`ult`/`ugt`) for SIMD `reduce_min/max`
    // (keyed by the receiver-vector span) and the slice-3 mask comparisons.
    program.unsigned_vector_exprs = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| match ty {
            Type::Vector { element, .. } if matches!(**element, Type::UInt(_)) => Some((k.0, k.1)),
            _ => None,
        })
        .collect();
    // The `expr_types` sweep alone misses method receivers: a MethodCall
    // node shares its receiver's span, and the call's *result* type is
    // the last write at that key — so `v.reduce_min()` erases `v`'s
    // `Vector[u8, N]` entry and reduce_min/max compared signed. Masked
    // until 2026-06-07 by bare-literal lanes lowering at i64 width (wide
    // positive values compare the same under either predicate); the lane
    // boundary coercion exposed it. `vector_method_receivers` records the
    // receiver vector type keyed by the call span (the same collided
    // key), so folding its unsigned hits in restores this table's
    // documented meaning for receiver positions.
    program.unsigned_vector_exprs.extend(
        tc.vector_method_receivers
            .iter()
            .filter(|(_, (elem, _))| matches!(elem, Type::UInt(_)))
            .map(|(k, _)| (k.0, k.1)),
    );
    // Sibling to `string_typed_exprs`: for each expression whose Kāra
    // type is a `Named` struct, record the canonical struct name. Codegen
    // uses this in `emit_sort_by_key_inline_thunk` to dispatch struct-typed
    // keys to a field-aware lex cascade (the all-int-tuple cascade doesn't
    // cover mixed int + String fields, and the LLVM struct type alone
    // can't recover the source-level struct name to query its field types).
    program.expr_struct_type_names = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| match ty {
            Type::Named { name, .. } => Some(((k.0, k.1), name.clone())),
            _ => None,
        })
        .collect();
    // For every expression whose Kāra type is a struct (or shared
    // struct) that the user provided an `impl Ord for T` for, record
    // the canonical `"Type.cmp"` callee key. `emit_sort_by_key_inline_thunk`
    // consults this map before the field-by-field derive cascade and
    // dispatches via direct call to the user's compiled cmp — preserves
    // custom orderings (reverse, multi-key tiebreaks, partial-field)
    // the cascade can't reproduce. Gated by the typechecker change in
    // derives.rs that lets `impl Ord` count toward the Ord bound:
    // without it, no sort_by_key call would ever reach codegen with a
    // user-impl-Ord struct key.
    program.user_ord_typed_exprs = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| {
            let name = match ty {
                Type::Named { name, .. } => name,
                Type::Shared(name) => name,
                _ => return None,
            };
            if tc
                .trait_impls
                .contains(&("Ord".to_string(), name.to_string()))
            {
                Some(((k.0, k.1), format!("{}.cmp", name)))
            } else {
                None
            }
        })
        .collect();
    // Forward per-leaf-binding borrow modes so codegen's
    // `bind_pattern_values` can wrap each ref/mut-ref leaf in a ref-shim
    // alloca (alloca-of-pointer-to-value-alloca, registered in
    // `ref_params`). Without this plumb, well-typed `match val { Foo
    // { name } => use_str(name) }` under `val: ref Foo` compiled the
    // arm-bound `name` as a value but then passed it where a pointer
    // (`ref String`) was expected — ABI miscompile fixed by the shim.
    program.pattern_binding_borrow_modes = tc
        .pattern_binding_borrow_modes
        .iter()
        .map(|(k, v)| ((k.0, k.1), *v))
        .collect();
    // Forward the typechecker's `impl Drop` registry so codegen's
    // `emit_user_drop_wrappers` pass can synthesize `karac_drop_<Type>`
    // wrappers for each user type with a validated drop body. Prereq.2
    // of the user-`impl Drop` dispatch slice — see
    // `docs/implementation_checklist/phase-7-codegen.md`.
    program.drop_method_keys = tc.drop_method_keys.clone();
    // Surface `TypeExpr` of every expression that produces a heap-owning
    // *temporary*. Codegen's `materialize_owned_temp` keys this by span to
    // reconstruct the scope-exit cleanup an unnamed temp needs: the element
    // type that closes the `Vec` nested-heap leak, the `Map`/`Set` key/val
    // classification, or the shared-struct RC heap layout — none recoverable
    // from the LLVM value (a `Map` handle and an RC box are both plain
    // pointers). Restricted to the kinds `materialize_owned_temp` handles so
    // the table stays small; any other type is simply absent and the temp
    // path falls through to "no cleanup". Vec/String are still LLVM-type
    // detectable on their own (slice 1) — the entry only *adds* the element
    // type, so a missing entry degrades to slice-1 behavior, never a leak
    // regression. See `docs/spikes/general-owned-temp-tracking.md` (slice 2).
    program.owned_temp_drops = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| {
            let droppable = match ty {
                Type::Shared(_) => true,
                Type::Named { name, .. } => {
                    matches!(name.as_str(), "Vec" | "VecDeque" | "String" | "Map" | "Set")
                }
                _ => false,
            };
            droppable.then(|| ((k.0, k.1), TypeChecker::type_to_type_expr(ty)))
        })
        .collect();
    // Surface the pointee `TypeExpr` of every raw-pointer-typed expression
    // (`*const T` / `*mut T` → `Type::Pointer { inner, .. }`). Codegen's
    // unary-deref arm keys this by the *operand* span: a raw-pointer operand's
    // value IS the address, so `*p` must emit a real `load` of `inner`, whereas
    // a `ref T` / `mut ref T` operand (never `Type::Pointer`) is already the
    // inner value after `load_variable`'s two-step deref and needs no entry.
    // A missing entry degrades to the reference pass-through, which is correct
    // for references and only wrong for raw pointers — exactly what this fills.
    program.raw_pointer_pointee_types = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| match ty {
            Type::Pointer { inner, .. } => {
                Some(((k.0, k.1), TypeChecker::type_to_type_expr(inner)))
            }
            _ => None,
        })
        .collect();
    // Surface the fully-instantiated `TypeExpr` of every *generic* `Named`
    // instantiation expression (`Option[String]`, `Result[i64, AllocError]`,
    // generic user enums). Codegen's heap-payload enum `==`
    // (`compile_enum_eq`) keys this by operand span to recover the concrete
    // type argument a generic enum's variant payload was instantiated with —
    // `var_type_names` only keeps the bare name (`"Option"`), losing the
    // `[String]` that decides whether `Some`'s payload compares by content
    // (String/Vec) or by word (scalar). Restricted to non-empty `args` so the
    // table stays small (concrete enums like a user `Msg { Text(String) }`
    // already route via `enum_has_heap_payload` and need no entry); a missing
    // entry degrades to the word-wise path (sound for scalar/unit enums),
    // never a miscompile. See `EnumInstTypeExprsTable`.
    program.enum_inst_type_exprs = tc
        .expr_types
        .iter()
        .filter_map(|(k, ty)| match ty {
            Type::Named { args, .. } if !args.is_empty() => {
                Some(((k.0, k.1), TypeChecker::type_to_type_expr(ty)))
            }
            _ => None,
        })
        .collect();
}

struct Lowerer<'a> {
    tc: &'a TypeCheckResult,
}

impl<'a> Lowerer<'a> {
    fn lower_item(&mut self, item: &mut Item) {
        match item {
            Item::Function(f) => self.lower_function(f),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(m) = it {
                        self.lower_function(m);
                    }
                }
            }
            Item::ConstDecl(c) => self.lower_expr(&mut c.value),
            _ => {}
        }
    }

    fn lower_function(&mut self, f: &mut Function) {
        self.lower_block(&mut f.body);
    }

    fn lower_block(&mut self, block: &mut Block) {
        // Loop-bound collection pre-sizing (phase-7-codegen.md § 7.3 lever #1)
        // runs here, before this block's operators are lowered, so it sees raw
        // `Binary` loop conditions / increments. Operates only on this block's
        // statement sequence; nested blocks are pre-sized by their own
        // `lower_block` calls during the recursion below.
        crate::presize::presize_block(block);
        for stmt in &mut block.stmts {
            self.lower_stmt(stmt);
        }
        if let Some(ref mut e) = block.final_expr {
            self.lower_expr(e);
        }
    }

    fn lower_stmt(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                self.lower_expr(value);
                if let StmtKind::LetElse { else_block, .. } = &mut stmt.kind {
                    self.lower_block(else_block);
                }
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.lower_block(body);
            }
            StmtKind::Assign { target, value } => {
                self.lower_expr(target);
                self.lower_expr(value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.lower_expr(target);
                self.lower_expr(value);
            }
            StmtKind::Expr(e) => self.lower_expr(e),
        }
    }

    /// Lower an expression: recurse into children first (bottom-up), then
    /// rewrite the current node if it's a lowerable operator.
    fn lower_expr(&mut self, expr: &mut Expr) {
        // Recurse into sub-expressions first.
        match &mut expr.kind {
            ExprKind::Binary { left, right, .. } => {
                self.lower_expr(left);
                self.lower_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.lower_expr(operand),
            ExprKind::Question(inner) => self.lower_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.lower_expr(object);
                if let Some(args) = args {
                    for a in args {
                        self.lower_expr(&mut a.value);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.lower_expr(left);
                self.lower_expr(right);
            }
            ExprKind::Call { callee, args } => {
                self.lower_expr(callee);
                // `collect_all` auto-thunking (design.md § Concurrency
                // Semantics — "closure wrappers optional"): wrap each
                // bare-expression branch `e` as `|| e` so the
                // typechecker-validated branches reach codegen / the
                // interpreter uniformly as zero-arg closures. Explicit
                // `|| …` branches are already closures and are left
                // untouched. `infer_collect_all` (which runs *before*
                // lowering) accepts the bare form by using the expression's
                // own type, so the synthesized closure's return type matches.
                let is_collect_all =
                    matches!(&callee.kind, ExprKind::Identifier(n) if n == "collect_all");
                for a in args.iter_mut() {
                    self.lower_expr(&mut a.value);
                    if is_collect_all && !matches!(&a.value.kind, ExprKind::Closure { .. }) {
                        let span = a.value.span.clone();
                        let inner = std::mem::replace(
                            &mut a.value,
                            Expr {
                                kind: ExprKind::Tuple(Vec::new()),
                                span: span.clone(),
                            },
                        );
                        a.value = Expr {
                            kind: ExprKind::Closure {
                                params: Vec::new(),
                                capture_mode: None,
                                prefix_span: None,
                                body: Box::new(inner),
                            },
                            span,
                        };
                    }
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.lower_expr(object);
                for a in args {
                    self.lower_expr(&mut a.value);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.lower_expr(object);
            }
            ExprKind::Index { object, index } => {
                self.lower_expr(object);
                self.lower_expr(index);
            }
            ExprKind::Block(b) | ExprKind::Comptime(b) => self.lower_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.lower_expr(condition);
                self.lower_block(then_block);
                if let Some(eb) = else_branch {
                    self.lower_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.lower_expr(value);
                self.lower_block(then_block);
                if let Some(eb) = else_branch {
                    self.lower_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.lower_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &mut arm.guard {
                        self.lower_expr(g);
                    }
                    self.lower_expr(&mut arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.lower_expr(condition);
                self.lower_block(body);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.lower_expr(value);
                self.lower_block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                self.lower_expr(iterable);
                self.lower_block(body);
            }
            ExprKind::Loop { body, .. } => self.lower_block(body),
            ExprKind::LabeledBlock { body, .. } => self.lower_block(body),
            ExprKind::Closure { body, .. } => self.lower_expr(body),
            ExprKind::Return(opt) => {
                if let Some(e) = opt {
                    self.lower_expr(e);
                }
            }
            ExprKind::Break { value, .. } => {
                if let Some(e) = value {
                    self.lower_expr(e);
                }
            }
            ExprKind::Continue { .. } => {}
            ExprKind::Tuple(es) => {
                for e in es {
                    self.lower_expr(e);
                }
            }
            ExprKind::ArrayLiteral(es) => {
                for e in es {
                    self.lower_expr(e);
                }
                // A bare `[e1, e2, …]` literal defaults to `Vec[T]` (the
                // typechecker's synthesis-mode rule); only an `Array[T, N]`
                // annotation makes it a fixed array. Codegen's
                // `compile_array_literal` always emits a fixed `[N x T]`
                // aggregate — correct ONLY for the Array case. For the Vec
                // default that aggregate is the wrong shape (a `{ptr,len,cap}`
                // heap Vec is needed), so a `return`/call-arg/`.push` against
                // it mismatches (LLVM verification failure or a segfault on
                // the array bytes read as a Vec header). Canonicalize the Vec
                // case to the `Vec[…]` prefix form, which codegen already
                // materializes as a real heap-backed Vec via
                // `compile_vec_prefix_literal`. The fixed-array case (typed
                // `Array[T, N]`, recorded as `Type::Array`) is left untouched.
                // Mirror-image of the `Array[…]` → ArrayLiteral
                // canonicalization in the PrefixCollectionLiteral arm.
                let is_vec = matches!(
                    self.tc.expr_types.get(&SpanKey::from_span(&expr.span)),
                    Some(Type::Named { name, .. }) if name == "Vec"
                );
                if is_vec {
                    if let ExprKind::ArrayLiteral(items) = &mut expr.kind {
                        let items = std::mem::take(items);
                        expr.kind = ExprKind::PrefixCollectionLiteral {
                            type_name: "Vec".to_string(),
                            items,
                        };
                    }
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.lower_expr(value);
                self.lower_expr(count);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.lower_expr(e);
                }
                // `Array[e1, e2, …]` is just the prefix-named form of the
                // bare `[e1, e2, …]` array literal — canonicalize it here so
                // codegen has a single ArrayLiteral arm to handle. Vec / Set
                // / Map prefix forms stay as PrefixCollectionLiteral; their
                // codegen paths consume the type-name marker.
                if let ExprKind::PrefixCollectionLiteral { type_name, items } = &mut expr.kind {
                    if type_name == "Array" {
                        let lowered_items = std::mem::take(items);
                        expr.kind = ExprKind::ArrayLiteral(lowered_items);
                    }
                }
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.lower_expr(k);
                    self.lower_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.lower_expr(&mut f.value);
                }
                if let Some(s) = spread {
                    self.lower_expr(s);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.lower_expr(left);
                self.lower_expr(right);
            }
            ExprKind::Cast { expr: inner, .. } => self.lower_expr(inner),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.lower_expr(s);
                }
                if let Some(e) = end {
                    self.lower_expr(e);
                }
            }
            ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
                self.lower_block(b)
            }
            ExprKind::Lock { body, .. } => self.lower_block(body),
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.lower_expr(&mut b.value);
                }
                self.lower_block(body);
            }
            // Leaf nodes — nothing to recurse into.
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::InterpolatedStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }

        // Then rewrite this node if it's an operator we lower.
        if let Some(rewritten) = self.try_rewrite(expr) {
            expr.kind = rewritten;
        }
    }

    /// Inspect an expression and return a rewritten `ExprKind` if it's an
    /// operator that should be lowered to a trait-method call. v1: arithmetic
    /// binary ops + unary `Neg` only.
    fn try_rewrite(&self, expr: &Expr) -> Option<ExprKind> {
        match &expr.kind {
            ExprKind::Binary { op, left, right } => {
                self.rewrite_binary(op, left, right, &expr.span)
            }
            ExprKind::Unary { op, operand } => self.rewrite_unary(op, operand, &expr.span),
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => self
                .rewrite_into_call(object, method, args, &expr.span)
                .or_else(|| self.rewrite_try_into_call(object, method, args, &expr.span)),
            ExprKind::Call { callee, args } => self
                .rewrite_path_call_to_method_call(callee, args, &expr.span)
                .or_else(|| self.rewrite_bare_assoc_fn_call(callee, args, &expr.span)),
            _ => None,
        }
    }

    /// Rewrite `Call(Path([X, method]), args)` to
    /// `MethodCall { object: Identifier(X), method, args }` when the
    /// typechecker flagged the call span in `path_call_method_dispatch`.
    /// The parser produces the `Call(Path)` shape for any uppercase-leading
    /// `X.method(args)` (see `src/parser/exprs.rs` 1298–1326); the
    /// typechecker disambiguates against the env in `infer_call`'s
    /// dispatch and routes value-binding receivers through
    /// `infer_method_call`. This rewrite collapses the AST node to the
    /// MethodCall shape downstream phases already handle, so codegen's
    /// `compile_assoc_call` (which would otherwise try to dispatch
    /// `X.method` as a Type-associated call and fall back to `const 0`)
    /// never sees the value-binding case. Tried before
    /// `rewrite_bare_assoc_fn_call` so the two patterns don't conflict
    /// for shapes that the typechecker decided are method calls.
    fn rewrite_path_call_to_method_call(
        &self,
        callee: &Expr,
        args: &[CallArg],
        span: &Span,
    ) -> Option<ExprKind> {
        if !self
            .tc
            .path_call_method_dispatch
            .contains(&SpanKey::from_span(span))
        {
            return None;
        }
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.len() != 2 {
            return None;
        }
        let object = Box::new(Expr {
            span: callee.span.clone(),
            kind: ExprKind::Identifier(segments[0].clone()),
        });
        Some(ExprKind::MethodCall {
            object,
            method: segments[1].clone(),
            turbofish: None,
            args: args.to_vec(),
            // Synthetic lowering — no real `)` to point at. Placeholder
            // span; consumers that care about the call's source extent
            // (L205 lock-block edit emission) only fire on user-source
            // method-call receivers, never on synthetic lowerings.
            args_close_span: span.clone(),
        })
    }

    /// Rewrite a bare-identifier associated-function call (`name(args)`) to
    /// `Target.name(args)` when the typechecker resolved the target via
    /// expected-type inference. Backed by `bare_assoc_fn_targets` populated in
    /// `try_apply_expected_assoc_fn_inference`. The rewritten call dispatches
    /// through the existing impl table at the interpreter / codegen layer.
    fn rewrite_bare_assoc_fn_call(
        &self,
        callee: &Expr,
        args: &[CallArg],
        span: &Span,
    ) -> Option<ExprKind> {
        let name = match &callee.kind {
            ExprKind::Identifier(n) => n.clone(),
            _ => return None,
        };
        let target = self
            .tc
            .bare_assoc_fn_targets
            .get(&SpanKey::from_span(span))?
            .clone();
        let new_callee = Box::new(Expr {
            span: callee.span.clone(),
            kind: ExprKind::Path {
                segments: vec![target, name],
                generic_args: None,
            },
        });
        Some(ExprKind::Call {
            callee: new_callee,
            args: args.to_vec(),
        })
    }

    /// Rewrite `x.into()` to `Target.from(x)` when the typechecker recorded a
    /// target type in `into_conversions`. The `Into` blanket impl is purely a
    /// lowering rewrite — no impl entries are materialized at type-check time.
    fn rewrite_into_call(
        &self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<ExprKind> {
        if method != "into" || !args.is_empty() {
            return None;
        }
        let target = self.tc.into_conversions.get(&SpanKey::from_span(span))?;
        Some(call_path(
            vec![target.clone(), "from".to_string()],
            vec![object.clone()],
        ))
    }

    /// Rewrite `x.try_into()` to `Target.try_from(x)` when the typechecker
    /// recorded a target type in `try_into_conversions`. Same desugar
    /// architecture as `rewrite_into_call` — `TryInto` is not materialized
    /// in the impl table; user impls `TryFrom` and the desugar routes
    /// through it.
    fn rewrite_try_into_call(
        &self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<ExprKind> {
        if method != "try_into" || !args.is_empty() {
            return None;
        }
        let target = self
            .tc
            .try_into_conversions
            .get(&SpanKey::from_span(span))?;
        Some(call_path(
            vec![target.clone(), "try_from".to_string()],
            vec![object.clone()],
        ))
    }

    fn rewrite_binary(
        &self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
        _span: &Span,
    ) -> Option<ExprKind> {
        let method = match op {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::Div => "div",
            BinOp::Mod => "rem",
            BinOp::Eq => "eq",
            BinOp::NotEq => "ne",
            BinOp::Lt => "lt",
            BinOp::LtEq => "le",
            BinOp::Gt => "gt",
            BinOp::GtEq => "ge",
            BinOp::BitAnd => "bitand",
            BinOp::BitOr => "bitor",
            BinOp::BitXor => "bitxor",
            BinOp::Shl => "shl",
            BinOp::Shr => "shr",
            // Defer: logical (short-circuit), range (constructs Range type).
            BinOp::And | BinOp::Or | BinOp::Range | BinOp::RangeInclusive => return None,
        };

        let lhs_ty = self.type_at(&left.span)?;
        let rhs_ty = self.type_at(&right.span)?;
        // Homogeneity: only lower when both operands are the same type.
        // Mixed-numeric arithmetic (`i32 + i64`) currently fails typecheck
        // — once widening is wired, this constraint can relax.
        if lhs_ty != rhs_ty {
            return None;
        }
        let target = target_type_name(lhs_ty, op, self.tc)?;

        // User impls of `Eq` conventionally provide only `eq`; mirror the
        // Rust/std convention and desugar `!=` to `!eq(a, b)` on Named
        // operand types. Primitive types always have `ne` registered in the
        // stdlib impl table, so they dispatch directly.
        if matches!(op, BinOp::NotEq) && matches!(lhs_ty, Type::Named { .. }) {
            let eq_call = Expr {
                span: left.span.clone(),
                kind: call_path(
                    vec![target, "eq".to_string()],
                    vec![left.clone(), right.clone()],
                ),
            };
            return Some(ExprKind::Unary {
                op: UnaryOp::Not,
                operand: Box::new(eq_call),
            });
        }

        Some(call_path(
            vec![target, method.to_string()],
            vec![left.clone(), right.clone()],
        ))
    }

    fn rewrite_unary(&self, op: &UnaryOp, operand: &Expr, _span: &Span) -> Option<ExprKind> {
        // `*expr` is a built-in dereference — no trait-method lowering needed.
        if matches!(op, UnaryOp::Deref) {
            return None;
        }
        let method = match op {
            UnaryOp::Neg => "neg",
            // Both `!` (logical not) and `~` (bitwise not) dispatch through
            // the `Not` trait's `not` method. The target type disambiguates:
            // `!bool` → `bool.not`, `~i32` → `i32.not`.
            UnaryOp::Not | UnaryOp::BitNot => "not",
            UnaryOp::Deref => unreachable!(),
        };
        let ty = self.type_at(&operand.span)?;
        // Gate each op to its valid operand types (mirrors typechecker rules).
        let ok = match op {
            UnaryOp::Neg => matches!(ty, Type::Int(_) | Type::Float(_)),
            UnaryOp::Not => matches!(ty, Type::Bool),
            UnaryOp::BitNot => matches!(ty, Type::Int(_) | Type::UInt(_)),
            UnaryOp::Deref => unreachable!(),
        };
        if !ok {
            return None;
        }
        let target = primitive_type_name(ty)?;
        Some(call_path(
            vec![target, method.to_string()],
            vec![operand.clone()],
        ))
    }

    fn type_at(&self, span: &Span) -> Option<&Type> {
        self.tc.expr_types.get(&SpanKey::from_span(span))
    }
}

/// Build a `Call(Path(segments), [args])` ExprKind with no labels.
fn call_path(segments: Vec<String>, args: Vec<Expr>) -> ExprKind {
    let span = args
        .first()
        .map(|a| a.span.clone())
        .unwrap_or_else(|| Span {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        });
    let callee = Box::new(Expr {
        span: span.clone(),
        kind: ExprKind::Path {
            segments,
            generic_args: None,
        },
    });
    let call_args = args
        .into_iter()
        .map(|e| CallArg {
            label: None,
            mut_marker: false,
            span: e.span.clone(),
            value: e,
        })
        .collect();
    ExprKind::Call {
        callee,
        args: call_args,
    }
}

/// Resolve the impl-target type name for a binary operator on operand type `ty`.
/// For primitive types, falls back to the stdlib target (`i32`, `bool`, etc.).
/// For user-defined `Type::Named`, returns the name only when a matching user
/// impl is registered — the operator-lowering pass supports user-type dispatch
/// only for the relational traits (`Eq`, `Ord`) at present; arithmetic/bitwise
/// stay primitive-only until the heterogeneous `Rhs`/`Output` design lands.
fn target_type_name(ty: &Type, op: &BinOp, tc: &TypeCheckResult) -> Option<String> {
    if let Some(name) = primitive_type_name(ty) {
        return Some(name);
    }
    if let Type::Named { name, .. } = ty {
        let trait_name = match op {
            BinOp::Eq | BinOp::NotEq => "Eq",
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => "Ord",
            _ => return None,
        };
        if tc
            .trait_impls
            .contains(&(trait_name.to_string(), name.clone()))
        {
            return Some(name.clone());
        }
    }
    None
}

/// Returns the canonical name of a primitive type for impl-target lookup,
/// or None for non-primitive types we don't yet lower operators on.
fn primitive_type_name(ty: &Type) -> Option<String> {
    let name = match ty {
        Type::Int(IntSize::I8) => "i8",
        Type::Int(IntSize::I16) => "i16",
        Type::Int(IntSize::I32) => "i32",
        Type::Int(IntSize::I64) => "i64",
        Type::UInt(UIntSize::U8) => "u8",
        Type::UInt(UIntSize::U16) => "u16",
        Type::UInt(UIntSize::U32) => "u32",
        Type::UInt(UIntSize::U64) => "u64",
        Type::UInt(UIntSize::Usize) => "usize",
        Type::Float(FloatSize::F32) => "f32",
        Type::Float(FloatSize::F64) => "f64",
        Type::Bool => "bool",
        Type::Char => "char",
        Type::Str => "String",
        _ => return None,
    };
    Some(name.to_string())
}
