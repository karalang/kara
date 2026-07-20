//! For-loop codegen: every `for pattern in <iterable> { body }` shape
//! the compiler knows how to lower today.
//!
//! Houses `compile_for` (the entry dispatch) and the per-iterable-shape
//! specialisations: `compile_for_range`, `compile_for_range_with_step`,
//! `compile_for_slice_var`, `compile_for_vec_var`,
//! `compile_for_string_chars` / `compile_for_string_chars_inner`,
//! `compile_for_map_var`, `compile_for_set_var`, `compile_for_array_var`,
//! `compile_for_array_values`.
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use crate::ast::*;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::state::LoopFrame;

impl<'ctx> super::Codegen<'ctx> {
    // ── For loop ─────────────────────────────────────────────────

    /// `.enumerate()` support (B-2026-07-08-5): true when `object` (the receiver
    /// of `.enumerate()`, typically `xs.iter()`) peels to a named Vec / Slice /
    /// array variable — the receivers whose container loops carry a storage
    /// index we can bind as the enumerate index. Conservative: anything else
    /// (array literals, field/index receivers, map/set/string) returns false so
    /// the `.enumerate()` arm falls through to the existing dispatch unchanged.
    fn for_receiver_is_indexable(&self, object: &Expr) -> bool {
        let inner = match &object.kind {
            ExprKind::MethodCall {
                object: o,
                method,
                args,
                ..
            } if args.is_empty() && (method == "iter" || method == "into_iter") => o.as_ref(),
            _ => object,
        };
        if let ExprKind::Identifier(name) = &inner.kind {
            let n = name.as_str();
            if self.vec_elem_types.contains_key(n) || self.slice_elem_types.contains_key(n) {
                return true;
            }
            if let Some(slot) = self.variables.get(n) {
                if matches!(slot.ty, BasicTypeEnum::ArrayType(_)) {
                    return true;
                }
            }
            if matches!(self.ref_params.get(n), Some(BasicTypeEnum::ArrayType(_))) {
                return true;
            }
        }
        // `for (i, x) in obj.field.iter().enumerate()`: the `.iter()` peel routes
        // a `Vec`/`Slice` field through `try_compile_for_field_iter`, which mints
        // a synth identifier and recurses into `compile_for_{vec,slice}_var` (both
        // now bind the enumerate index). Accept optimistically — if the field
        // isn't an indexable container the field-iter path returns `None` and the
        // dispatch falls through to the prior skip, with the stashed index pattern
        // restored by the `.enumerate()` arm's save/restore (no leak, no regression).
        if matches!(inner.kind, ExprKind::FieldAccess { .. }) {
            return true;
        }
        false
    }

    /// Bind the pending `.enumerate()` index sub-pattern to the container loop's
    /// current storage index `cur`, if an enumerate is active. `take()`s the
    /// pattern so a nested loop inside the body doesn't re-bind it. Called by
    /// each indexable container loop right after it binds the element.
    fn bind_enumerate_index(&mut self, cur: IntValue<'ctx>) -> Result<(), String> {
        if let Some(idx_pat) = self.enumerate_index_pattern.take() {
            self.bind_pattern(&idx_pat, cur.into())?;
        }
        Ok(())
    }

    /// Compile `for pattern in iterable { body }`.
    /// Currently supports ranges (`start..end`, `start..=end`) and array literals.
    pub(super) fn compile_for(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        iterable: &Expr,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Materialized iterator binding (B-2026-07-11-19): `for x in it` where
        // `it` was bound by a recorded `let it = <v.iter()-chain>` — inline the
        // chain as the iterable (`for x in v.iter()...`) so the existing
        // `.iter()` / fused-adaptor for-loop paths handle it.
        if !self.iter_let_bindings.is_empty() {
            if let Some(sub) = self.substitute_iter_let_receiver(iterable) {
                return self.compile_for(label, pattern, &sub, body);
            }
        }

        // `for x in <iter-chain>.rev()` — reverse-iterate (B-2026-07-18-41): if
        // the chain is reverse-SAFE (order-independent steps over a bound-Vec
        // base), strip `.rev()`, set the one-shot reverse signal, and recurse on
        // the stripped iterable — the base `compile_for_vec_var` then iterates
        // `len-1-i`. Otherwise bail LOUD (never the silent `_ =>` fall-through,
        // which would drop the body → empty output vs the interpreter).
        if Self::chain_receiver_contains_rev(iterable) {
            if self.rev_chain_reverse_iterable(iterable) {
                let stripped = Self::strip_rev_node(iterable);
                let saved = self.pending_reverse_iter;
                self.pending_reverse_iter = true;
                let r = self.compile_for(label, pattern, &stripped, body);
                let consumed = !self.pending_reverse_iter;
                self.pending_reverse_iter = saved;
                if consumed {
                    return r;
                }
            }
            return Err(
                "`Iterator.rev()` is not yet supported under `karac build`/`karac run` \
                 (codegen) for this chain shape; it works under the tree-walk \
                 interpreter. Re-run with `--interp` (or `KARAC_RUN_JIT=0`)."
                    .to_string(),
            );
        }

        // `for x in <recv>.flatten()` — nested-loop desugar (B-2026-07-19-12
        // slice 2): outer loop binds each inner iterable, inner loop yields its
        // elements. Only when `flatten` is the OUTERMOST call on the iterable;
        // fails closed to the loud defer below for a receiver shape it can't
        // prove.
        if let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &iterable.kind
        {
            if method == "flatten" && args.is_empty() {
                if let Some(v) =
                    self.try_compile_for_flatten(label, pattern, object, body, &iterable.span)?
                {
                    return Ok(v);
                }
            }
        }
        // A flatten NOT outermost (`for x in xs.iter().flatten().map(g)`) flows
        // to the fused-adaptor dispatch below, which now peels flatten as a
        // structural fused base (`peel_base_is_structural_adaptor`, slice 3). An
        // outermost flatten that `try_compile_for_flatten` above DECLINED, and
        // any flatten chain the fused path can't lower, are caught LOUD by the
        // `UNLOWERED_FOR_ADAPTORS` backstop (which lists `flatten`) before the
        // silent `_ =>` fall-through — never a silently-skipped body.

        // `for x in coll.iter()` / `for x in coll.into_iter()` —
        // codegen iterates the underlying storage directly via the
        // existing `compile_for_*_var` paths (no `Value::Iterator`
        // wrapper at this layer), so peel off a transparent `.iter()`
        // / `.into_iter()` and recurse on the inner receiver. Without
        // this, the method-call iterable falls through to the silent
        // `_ =>` arm below — the body never executes and outer-scope
        // mutables look unchanged.
        if let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &iterable.kind
        {
            // `for line in stdin.lines()` — the stdin line iterator (phase-8
            // `Stdin.lines()` slice). Caught before the `.iter()`/`.map()`
            // peeling and the silent `_ =>` fall-through; routes to a dedicated
            // loop that pulls one line per iteration via
            // `karac_runtime_stdin_next_line`. The receiver is the ambient
            // `stdin` alias (or the capitalized `Stdin`), guarded against a
            // local shadow — the same rule the resource-method dispatch uses.
            if method == "lines" && args.is_empty() {
                if let ExprKind::Identifier(recv) = &object.kind {
                    let is_stdin = recv == "Stdin"
                        || (super::method_call::ambient_resource_for_alias(recv) == Some("Stdin")
                            && !self.variables.contains_key(recv));
                    if is_stdin {
                        return self.compile_for_stdin_lines(label, pattern, body);
                    }
                }
            }
            // Pure `skip`/`take` chains over a named scalar-element Vec use
            // the cheaper index-window lowering (no per-element counters, no
            // iteration over skipped prefixes) — try it BEFORE the fused
            // desugar below so it keeps priority for the shapes it covers
            // (B-2026-07-14-8, skip/take leg).
            if args.len() == 1 && (method == "skip" || method == "take") {
                if let Some(v) =
                    self.try_compile_for_skip_take_chain(label, pattern, iterable, body)?
                {
                    return Ok(v);
                }
            }
            // `for x in <src>.iter().{map|filter|take_while|skip_while|take|
            // skip|step_by|inspect}+ { … }` — a fused-adaptor iterable. Without
            // this it falls through to the silent `_ =>` arm below and the body
            // runs ZERO times (B-2026-07-11-18). Routed through the same fusion
            // as the `fold` terminal, with the user body as the sink; fails
            // closed (`None`) for any other chain (which then hits the loud
            // unlowered-adaptor bail below, B-2026-07-14-8). `step_by` directly
            // on a Range is EXCLUDED here: the dedicated strided-range arm
            // below lowers it without a per-element counter.
            let fusable_outer = args.len() == 1
                && match method.as_str() {
                    "map" | "filter" | "take_while" | "skip_while" | "inspect" | "take"
                    | "skip" => true,
                    "step_by" => !matches!(&object.kind, ExprKind::Range { .. }),
                    _ => false,
                };
            if fusable_outer {
                if let Some(v) = self.try_compile_for_iter_chain(label, pattern, iterable, body)? {
                    return Ok(v);
                }
            }
            // `for x in <recv>.flat_map(|p| <inner>) { … }` — nested-loop
            // desugar (B-2026-07-14-8, flat_map leg). Fails closed to the
            // loud `.flat_map()` adaptor bail below for shapes it can't
            // prove (user label, complex closure param, unproven inner
            // iterable).
            if args.len() == 1 && method == "flat_map" {
                if let Some(v) = self.try_compile_for_flat_map(
                    label,
                    pattern,
                    object,
                    &args[0].value,
                    body,
                    &iterable.span,
                )? {
                    return Ok(v);
                }
            }
            // `for x in <src>.cycle() { … }` — restart-loop desugar
            // (B-2026-07-14-8, cycle leg). Fails closed to the loud
            // `.cycle()` bail below for a labeled loop or a source the
            // fused peel rejects.
            if args.is_empty() && method == "cycle" {
                if let Some(v) =
                    self.try_compile_for_cycle(label, pattern, object, body, &iterable.span)?
                {
                    return Ok(v);
                }
            }
            // `for w in xs.iter().windows(k)` / `.chunks(k)` — per-group
            // materializing desugar over a named scalar-element Vec
            // (B-2026-07-14-8, windows/chunks legs). Fails closed to the
            // loud adaptor bail below for heap elements or non-identity
            // sources.
            if args.len() == 1 && (method == "windows" || method == "chunks") {
                if let Some(v) = self.try_compile_for_windows_chunks(
                    label,
                    pattern,
                    object,
                    &args[0].value,
                    method == "windows",
                    body,
                    &iterable.span,
                )? {
                    return Ok(v);
                }
            }
            // `for g in xs.iter().chunk_by(|x| key) { … }` — boundary-walk +
            // group-materializing desugar over a named scalar-element Vec
            // (B-2026-07-14-8, chunk_by leg). Fails closed to the loud
            // `.chunk_by()` bail below for heap elements / complex closure
            // params / non-identity sources.
            if args.len() == 1 && method == "chunk_by" {
                if let Some(v) = self.try_compile_for_chunk_by(
                    label,
                    pattern,
                    object,
                    &args[0].value,
                    body,
                    &iterable.span,
                )? {
                    return Ok(v);
                }
            }
            // `for x in <src>.scan(init, |acc, x| Some((new, out))) { … }` —
            // single accumulator-loop desugar (B-2026-07-14-8, scan leg).
            // Fails closed to the loud `.scan()` bail below for the
            // conditional-`None` early-stop body form or a peel-rejected
            // source.
            if args.len() == 2 && method == "scan" {
                if let Some(v) = self.try_compile_for_scan(
                    label,
                    pattern,
                    object,
                    &args[0].value,
                    &args[1].value,
                    body,
                    &iterable.span,
                )? {
                    return Ok(v);
                }
            }
            // `for x in <src>.peekable()` — a bare `Peekable` wrapper is a
            // pure identity in for-loop position (`.peek()` only exists on a
            // materialized iterator binding, which a for-iterable never is) —
            // peel it off and recurse (B-2026-07-14-8, peekable leg). The
            // fused peel does the same for mid-chain `peekable()`.
            if args.is_empty() && method == "peekable" {
                return self.compile_for(label, pattern, object, body);
            }
            if args.is_empty() && (method == "iter" || method == "into_iter") {
                // Indexed receiver (`coll[i].iter()`): synthesize a
                // temp identifier pointing into `coll`'s storage and
                // recurse, mirroring `compile_nested_index_read`.
                // Without this, the recursed `compile_for` sees an
                // Index expression and falls through the dispatch
                // match's `_ =>` arm — the body never executes.
                if let ExprKind::Index {
                    object: outer,
                    index: idx,
                } = &object.kind
                {
                    return self.compile_for_indexed_iter(label, pattern, outer, idx, body);
                }
                // Field receiver (`obj.field.iter()`) where `obj` is a
                // known struct (shared or plain) and `field` is a
                // `Vec[T]` / `Slice[T]`: synthesize a temp identifier
                // pointing at the field's embedded `{ptr,len,cap}`
                // struct and recurse. Without this, the recursed
                // `compile_for` sees a FieldAccess expression and falls
                // through to the `_ =>` arm — the body never executes
                // and outer-scope mutables look unchanged (the
                // clone-graph kata's `for nb in curr.neighbors.iter()`
                // surface, 2026-05-16).
                if let ExprKind::FieldAccess {
                    object: outer,
                    field,
                } = &object.kind
                {
                    if let Some(result) =
                        self.try_compile_for_field_iter(label, pattern, outer, field, body)?
                    {
                        return Ok(result);
                    }
                }
                return self.compile_for(label, pattern, object, body);
            }
            // `for c in <receiver>.chars()` — codegen iterators are
            // dispatch points, not runtime values (design.md § Iterator
            // Adaptors v1 surface), so peel `.chars()` off and drive the
            // per-Unicode-scalar-value loop on the receiver's String
            // value. By the time we get here the typechecker has proven
            // the receiver is a String, so we don't need to enumerate
            // receiver shapes — the bare-String dispatch handles both
            // the var-alloca path (Identifier) and the value path
            // (everything else: Index, MethodCall, Call, FieldAccess,
            // StringLit, …) uniformly.
            //
            // Pre-2026-05-29: this arm recursed via `compile_for(…,
            // object, body)`, which only matched the Identifier /
            // StringLit / FieldAccess arms in the dispatcher below.
            // Any other receiver — `groups[idx].chars()` from a
            // `Vec[String]`, `get_str().chars()` from a fn-return,
            // `s.clone().chars()` from a method — fell through the
            // dispatcher's silent `_ =>` arm and the body never ran.
            // kata-17 (Letter Combinations of a Phone Number) surfaced
            // the indexed-Vec[String] case: `for letter in
            // groups[idx].chars()` produced 0 combinations instead of
            // 3 or 4 per digit, with no error.
            if args.is_empty() && method == "chars" {
                // Variable receiver: preserve the alloca-based dispatch
                // (extracts ptr/len from the var's struct slot, lets
                // any per-var tracking state stay in scope).
                if let ExprKind::Identifier(name) = &object.kind {
                    if self.string_vars.contains(name.as_str()) {
                        return self.compile_for_string_chars(label, pattern, name, body);
                    }
                }
                // Value receiver: compile the expression to a
                // `{ptr, len, cap}` String struct, extract data + len,
                // and drive the per-char loop — same shape as the
                // StringLit arm in the dispatcher below.
                let val = self.compile_expr(object)?;
                let sv = val.into_struct_value();
                let data = self
                    .builder
                    .build_extract_value(sv, 0, "for.s.recv.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(sv, 1, "for.s.recv.len")
                    .unwrap()
                    .into_int_value();
                return self.compile_for_string_chars_inner(label, pattern, data, len, body);
            }
            // `for b in <receiver>.bytes()` — the byte-wise sibling of
            // `.chars()`. Same peel-and-drive shape, but each iteration
            // binds the raw `u8` byte (no UTF-8 decode). Without this arm
            // the `.bytes()` MethodCall iterable falls through to the
            // dispatcher's silent `_ =>` arm and the body never runs — a
            // silent miscompile (kata-71's byte-scan probe surfaced it:
            // `for b in s.bytes()` iterated zero times in compiled mode
            // while the interpreter iterated correctly).
            if args.is_empty() && method == "bytes" {
                let val = self.compile_expr(object)?;
                let sv = val.into_struct_value();
                let data = self
                    .builder
                    .build_extract_value(sv, 0, "for.b.recv.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(sv, 1, "for.b.recv.len")
                    .unwrap()
                    .into_int_value();
                return self.compile_for_string_bytes_inner(label, pattern, data, len, body);
            }
            // `for j in (start..end).step_by(n)` — the only chained
            // iterator-adaptor codegen surface supported in v1.
            // Lowers to a Range loop with a custom step (default 1).
            // The step expression `n` is evaluated once at loop entry
            // and captured for the increment block. Chained beyond
            // step_by (e.g. `.step_by(n).map(f)`) falls through to
            // the silent `_ =>` arm — the broader iterator-adaptor
            // codegen surface is a separate slice.
            if args.len() == 1 && method == "step_by" {
                if let ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } = &object.kind
                {
                    let step_expr = &args[0].value;
                    return self.compile_for_range_with_step(
                        label,
                        pattern,
                        start,
                        end,
                        *inclusive,
                        Some(step_expr),
                        body,
                    );
                }
            }
            // `for (i, v) in xs.iter().enumerate()` (B-2026-07-08-5). The
            // underlying `compile_for_{vec,slice,array}_var` loop already carries
            // the storage index as its induction variable, which is exactly the
            // enumerate index — so peel `.enumerate()`, stash the index
            // sub-pattern in `self.enumerate_index_pattern`, and recurse on the
            // inner receiver (`xs.iter()` → `xs`) with the ELEMENT sub-pattern.
            // The container loop binds the element as usual and additionally
            // binds the stashed index to its `cur`. Only 2-tuple patterns over an
            // indexable receiver are handled here; anything else falls through to
            // the dispatch below unchanged (no regression vs the prior skip). The
            // save/restore protects the fall-through case; the container loop's
            // `take()` protects nested loops in the body.
            if args.is_empty() && method == "enumerate" {
                if let PatternKind::Tuple(subs) = &pattern.kind {
                    if subs.len() == 2 && self.for_receiver_is_indexable(object) {
                        let idx_pat = subs[0].clone();
                        let elem_pat = &subs[1];
                        let saved = self.enumerate_index_pattern.take();
                        self.enumerate_index_pattern = Some(idx_pat);
                        let r = self.compile_for(label, elem_pat, object, body);
                        self.enumerate_index_pattern = saved;
                        return r;
                    }
                }
                // SINGLE-VAR binding (`for p in xs.iter().enumerate()`) over a
                // named SCALAR-element Vec (B-2026-07-14-8): materialize the
                // `(index, element)` pair into a `{i64, T}` tuple struct bound
                // to `p`, so `p.0` / `p.1` extract via the normal TupleIndex
                // path. Scalar elements only — a heap element stored into the
                // tuple would make the container loop AND the tuple both own it
                // (the double-drop the ledger flags); heap shapes keep the loud
                // adaptor bail below.
                if matches!(&pattern.kind, PatternKind::Binding(_)) {
                    if let Some(v) = self.peel_iter_to_scalar_vec_ident(object) {
                        return self.compile_for_enumerate_single_var(label, pattern, &v, body);
                    }
                }
            }
            // `for (a, b) in xs.iter().zip(ys.iter())` (B-2026-07-14-8, zip
            // leg): a lockstep two-source index loop over `0..min(lenA, lenB)`
            // binding `A[i]` / `B[i]` to the tuple's sub-patterns. Supported
            // shape: a 2-tuple destructure pattern and BOTH sources peeling
            // (through `.iter()`/`.into_iter()`) to named Vec bindings — ANY
            // element type: the compiler registers each sub-binding via
            // `register_for_loop_bindings`, whose borrow-marking makes heap
            // elements safe exactly like the single-source Vec loop. Anything
            // else falls through to the loud adaptor bail below.
            if args.len() == 1 && method == "zip" {
                if let PatternKind::Tuple(subs) = &pattern.kind {
                    if subs.len() == 2 {
                        let lhs = self.peel_iter_to_vec_ident(object);
                        let rhs = self.peel_iter_to_vec_ident(&args[0].value);
                        if let (Some(va), Some(vb)) = (lhs, rhs) {
                            let pat_a = subs[0].clone();
                            let pat_b = subs[1].clone();
                            return self
                                .compile_for_zip_vec_vars(label, &pat_a, &pat_b, &va, &vb, body);
                        }
                    }
                }
                // Single-binding zip (`for pair in xs.iter().zip(ys.iter()) {
                // pair.0 … }`, B-2026-07-15-10): bind the whole `(EA, EB)` tuple
                // to one variable and let the body read `.0`/`.1`, the enumerate
                // single-var precedent above. Scalar sources only (both peel to
                // trivially-copyable Vecs) — a heap-element tuple would copy the
                // element headers into the tuple with no borrow-marking, so those
                // stay on the two-sub-pattern destructure path (which
                // borrow-marks each side); a heap single-binding still bails loud.
                if matches!(&pattern.kind, PatternKind::Binding(_)) {
                    let lhs = self.peel_iter_to_scalar_vec_ident(object);
                    let rhs = self.peel_iter_to_scalar_vec_ident(&args[0].value);
                    if let (Some(va), Some(vb)) = (lhs, rhs) {
                        return self.compile_for_zip_single_var(label, pattern, &va, &vb, body);
                    }
                }
            }
            // (The `skip`/`take` index-window dispatch moved ABOVE the fused
            // desugar gate — see the top of this MethodCall block; chains it
            // rejects flow through the fused counter lowering instead.)
            // `for x in xs.iter().chain(ys.iter())` (B-2026-07-14-8, chain
            // leg): two sequential by-value index loops over the two sources,
            // sharing ONE exit block so `break` leaves BOTH (a break in the
            // first source's body must not fall into the second). Named Vec
            // sources, ANY element type (per-leg `register_for_loop_bindings`
            // borrow-marks heap elements); anything else bails loud below.
            if args.len() == 1 && method == "chain" {
                let lhs = self.peel_iter_to_vec_ident(object);
                let rhs = self.peel_iter_to_vec_ident(&args[0].value);
                if let (Some(va), Some(vb)) = (lhs, rhs) {
                    return self.compile_for_chain_vec_vars(label, pattern, &va, &vb, body);
                }
            }
        }
        match &iterable.kind {
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => self.compile_for_range(label, pattern, start, end, *inclusive, body),
            ExprKind::ArrayLiteral(elems) => {
                // Compile each element eagerly and iterate by index
                let elems: Vec<BasicValueEnum<'ctx>> = elems
                    .iter()
                    .map(|e| self.compile_expr(e))
                    .collect::<Result<_, _>>()?;
                self.compile_for_array_values(pattern, &elems, body)
            }
            ExprKind::StringLit(_) | ExprKind::InterpolatedStringLit(_) => {
                // Bare string literal or f-string as the iterable —
                // `for c in "abc"` / `for c in "abc".chars()` (after the
                // peel-off above). Compile the literal to a {ptr, len, cap}
                // String struct, extract data + len, drive the per-char
                // loop. No alloca needed: the struct is value-form and the
                // backing buffer is the program's read-only string pool
                // (cap=0 indicates static, no scope-exit free).
                let val = self.compile_expr(iterable)?;
                let sv = val.into_struct_value();
                let data = self
                    .builder
                    .build_extract_value(sv, 0, "for.s.lit.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(sv, 1, "for.s.lit.len")
                    .unwrap()
                    .into_int_value();
                self.compile_for_string_chars_inner(label, pattern, data, len, body)
            }
            ExprKind::Identifier(name) => {
                if let Some(slot) = self.variables.get(name.as_str()).copied() {
                    // Owned array
                    if let BasicTypeEnum::ArrayType(at) = slot.ty {
                        return self.compile_for_array_var(label, pattern, slot.ptr, at, body);
                    }
                    // Ref array
                    if let Some(&BasicTypeEnum::ArrayType(at)) = self.ref_params.get(name.as_str())
                    {
                        let arr_ptr = self.get_data_ptr(name).unwrap();
                        return self.compile_for_array_var(label, pattern, arr_ptr, at, body);
                    }
                    // String iteration — per Unicode scalar value. Must
                    // come before the `vec_elem_types` arm: String vars
                    // are *also* registered in `vec_elem_types` (with i8
                    // element type, matching the `{ptr, i64, i64}` byte
                    // buffer), but `for c in s` iterates chars (i32), not
                    // bytes (i8). `string_vars` is the disambiguator.
                    // Design pin: design.md § Character type (line 2299).
                    if self.string_vars.contains(name.as_str()) {
                        return self.compile_for_string_chars(label, pattern, name, body);
                    }
                    // Vec iteration (owned or ref)
                    if self.vec_elem_types.contains_key(name.as_str()) {
                        return self.compile_for_vec_var(label, pattern, name, body);
                    }
                    // Slice iteration: `{ptr, len}` struct alloca.
                    if self.slice_elem_types.contains_key(name.as_str()) {
                        return self.compile_for_slice_var(label, pattern, name, body);
                    }
                    // Map iteration: for (k, v) in map { }
                    if self.map_key_types.contains_key(name.as_str()) {
                        return self.compile_for_map_var(label, pattern, name, body);
                    }
                    // Set iteration: for x in set { }
                    if self.set_elem_types.contains_key(name.as_str()) {
                        return self.compile_for_set_var(label, pattern, name, body);
                    }
                }
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // `for x in self` inside a user `impl Trait for Vec[i64]` body.
            // `self` is a container receiver registered under the name "self"
            // (String/Vec/Slice/Map/Set tables), so route it to the SAME
            // borrow-path iterators as a named Identifier — NOT the `_ =>`
            // value path below, which materializes-iterates-DROPS the source.
            // For a `ref self` receiver (the borrow the caller still owns)
            // that drop is a double-free (free() double-free / SIGTRAP under
            // two-method dispatch); for an owned `self` the param-drop
            // machinery frees it at function exit, so the loop must not.
            // S6c blanket-Vec.
            ExprKind::SelfValue => {
                if self.string_vars.contains("self") {
                    return self.compile_for_string_chars(label, pattern, "self", body);
                }
                if self.vec_elem_types.contains_key("self") {
                    return self.compile_for_vec_var(label, pattern, "self", body);
                }
                if self.slice_elem_types.contains_key("self") {
                    return self.compile_for_slice_var(label, pattern, "self", body);
                }
                if self.map_key_types.contains_key("self") {
                    return self.compile_for_map_var(label, pattern, "self", body);
                }
                if self.set_elem_types.contains_key("self") {
                    return self.compile_for_set_var(label, pattern, "self", body);
                }
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // Bare field receiver: `for x in obj.field { }` (no
            // `.iter()` peel-off). Same synth-identifier pattern as the
            // `.iter()` arm above — recover the field pointer, mint a
            // tracked alias, and recurse with the alias as a regular
            // named-variable iterable.
            ExprKind::FieldAccess {
                object: outer,
                field,
            } => {
                if let Some(result) =
                    self.try_compile_for_field_iter(label, pattern, outer, field, body)?
                {
                    return Ok(result);
                }
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            _ => {
                // Value-producing iterable whose type is a Vec — e.g.
                // `for sub in t.iter_axis(0)` (a `Vec[Tensor]` temporary).
                // Materialize it into a synth local and iterate. Returns
                // None when the iterable isn't a recognised Vec-typed
                // value, in which case the body is skipped (the prior
                // behaviour for unknown iterables).
                if let Some(result) =
                    self.try_compile_for_vec_value(label, pattern, iterable, body)?
                {
                    return Ok(result);
                }
                // Fresh-temp `Map[K,V]` / `Set[T]` iterable (`for (k, v) in
                // make_map()`, `for x in make_set().iter()`) — same
                // materialize-iterate-drop shape, driving the map/set iterator.
                if let Some(result) =
                    self.try_compile_for_mapset_value(label, pattern, iterable, body)?
                {
                    return Ok(result);
                }
                // A for-loop over an iterator ADAPTOR that reached this
                // fall-through was NOT handled by any peel/fusion path above,
                // so the body would be silently skipped (loop runs zero times)
                // — a silent wrong answer, the worst failure mode
                // (B-2026-07-14-7). Adaptors that ARE lowered (`map`/`filter`
                // via the fusion path; `enumerate` with a 2-tuple pattern via
                // the peel) return BEFORE reaching here, so any lazy-adaptor
                // method call that lands in this catch-all is genuinely
                // unhandled. Confirmed silently-skipping shapes: `enumerate`
                // (single-var binding), `zip` (any pattern), `skip`/`take`
                // chains, `chain`, and the other lazy adaptors below. Bail
                // LOUD instead — matching the codebase's existing policy for
                // unsupported zip chains (`zip().map()` bails; see
                // tests/codegen.rs `e2e_iter_adaptor_zip_identity_collect`).
                // The set mirrors the typechecker's `Iterator` method list
                // (typechecker/stdlib_iter.rs), minus the eager TERMINALS
                // (`collect`/`sum`/`fold`/`count`/`reduce`/`all`/`any`/
                // `for_each`/`next`/`peek`) which never type as a for-loop
                // iterable. The interpreter handles all of these, so the
                // message points there. Full lowering is tracked as
                // B-2026-07-14-8.
                // `for x in xs.iter_mut()` — mutable iteration (B-2026-07-14-10).
                // The SUPPORTED shape lowers below: a named Vec receiver, a
                // simple binding pattern, and a trivially-copyable (scalar)
                // element — the loop binds `x` as a mut-ref slot pointer into
                // the Vec's storage (`data + i*stride`), routed through the
                // `entry_slot_ref_vars` deref machinery so `*x = …` / `*x += 1`
                // write back in place. Anything else (heap element, destructure
                // pattern, non-identifier receiver) bails LOUD — never the old
                // silent zero-iteration skip (B-2026-07-14-9) — pointing at
                // `--interp` (which handles all shapes) and the index-loop form.
                if let ExprKind::MethodCall {
                    method,
                    object: im_recv,
                    args: im_args,
                    ..
                } = &iterable.kind
                {
                    if method == "iter_mut" && im_args.is_empty() {
                        if let ExprKind::Identifier(recv_name) = &im_recv.kind {
                            let recv_name = recv_name.clone();
                            if self.vec_elem_types.contains_key(recv_name.as_str())
                                && matches!(&pattern.kind, PatternKind::Binding(_))
                                && self
                                    .var_elem_type_exprs
                                    .get(recv_name.as_str())
                                    .is_some_and(super::vec_method::is_trivially_copyable_te)
                            {
                                return self.compile_for_vec_var_iter_mut(
                                    label, pattern, &recv_name, body,
                                );
                            }
                        }
                        return Err("codegen: this `for x in …iter_mut()` shape is not yet \
                             supported under `karac build`/JIT (supported: a named \
                             `Vec` binding with a scalar element and a simple loop \
                             variable) — the loop body would otherwise be silently \
                             skipped. The interpreter implements every shape, so \
                             re-run with `--interp` (or `KARAC_RUN_JIT=0`); for a \
                             codegen build, use an index loop: \
                             `for i in 0..xs.len() { xs[i] = … }` (B-2026-07-14-10)."
                            .to_string());
                    }
                    // `map` and `filter` ARE lowered (the fused desugar above)
                    // — but the peel fails closed for shapes it can't prove
                    // (destructuring closure param, 2-param closure, non-source
                    // base), and before B-2026-07-14-21 those REJECTED chains
                    // fell through to the silent `_ =>` unit below and the body
                    // ran ZERO times (interp 10 / JIT 0 on
                    // `for x in ps.iter().map(|(a, b)| a + b)`). They belong in
                    // this loud backstop like every other adaptor.
                    const UNLOWERED_FOR_ADAPTORS: &[&str] = &[
                        "map",
                        "filter",
                        "enumerate",
                        "zip",
                        "skip",
                        "skip_while",
                        "take",
                        "take_while",
                        "chain",
                        "step_by",
                        "flat_map",
                        "flatten",
                        "chunks",
                        "chunk_by",
                        "windows",
                        "cycle",
                        "scan",
                        "peekable",
                        "inspect",
                    ];
                    if UNLOWERED_FOR_ADAPTORS.contains(&method.as_str()) {
                        let hint = if method == "enumerate" {
                            " (`enumerate` IS supported with a 2-tuple pattern — \
                             write `for (i, x) in …` instead of `for p in …`)"
                        } else if method == "zip" {
                            " (`zip` IS supported for `for (a, b) in \
                             xs.iter().zip(ys.iter())` over two named Vecs with \
                             scalar elements)"
                        } else if method == "map" || method == "filter" {
                            " (most `map`/`filter` chains ARE lowered — this \
                             specific shape defeated the fused desugar, e.g. a \
                             destructuring closure parameter)"
                        } else {
                            ""
                        };
                        return Err(format!(
                            "codegen: for-loop over the `.{method}()` iterator adaptor is \
                             not yet lowered (the loop body would otherwise be silently \
                             skipped, running zero times){hint}. Re-run with `--interp` \
                             (or `KARAC_RUN_JIT=0`) to use the tree-walk interpreter, \
                             which handles it."
                        ));
                    }
                }
                // Unknown iterable — skip body, return unit
                Ok(self.context.i64_type().const_int(0, false).into())
            }
        }
    }

    /// Iterate a value-producing iterable whose type is a `Vec[T]` (a
    /// method/function-call result that isn't a named variable or a
    /// peeled `.iter()`/`.chars()`/`.bytes()` source — the driver case is
    /// `for sub in t.iter_axis(n)`, whose result is `Vec[Tensor]`).
    /// Materializes the value into a synth local, registers it as a Vec
    /// (so `compile_for_vec_var` + `register_for_loop_bindings` drive the
    /// loop and re-register each element — a `Tensor` element gets its
    /// `tensor_var_infos` entry so `sub[i, j]` works in the body), queues
    /// the temp's scope-exit cleanup (tensor-element-aware), then iterates.
    /// Returns `Ok(None)` when the iterable isn't a Vec-typed value (the
    /// owned-temp side-table has no Vec entry at its span) — caller skips
    /// the body, preserving the prior unknown-iterable behaviour.
    /// Reconstruct a `Vec[elem]` `TypeExpr` from a bare element `TypeExpr`.
    /// Used by the fresh-temp `.iter()` for-loop path, where only the element
    /// type survives (span-keyed in `temp_recv_elem_types`) — the receiver's
    /// `Vec[T]` was clobbered to `Iterator[T]` in `expr_types`. The synthesized
    /// type drives `register_var_from_type_expr` (vec_elem_types +
    /// var_elem_type_exprs) and the `is_vec` gate identically to a real
    /// `owned_temp_drops` Vec entry.
    pub(super) fn vec_type_expr_from_element(elem_te: &TypeExpr) -> TypeExpr {
        TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(elem_te.clone())]),
                span: elem_te.span.clone(),
            }),
            span: elem_te.span.clone(),
        }
    }

    fn try_compile_for_vec_value(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        iterable: &Expr,
        body: &Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        use super::state::VarSlot;
        let key = (iterable.span.offset, iterable.span.length);
        // `temp_recv_elem_types` is checked BEFORE `owned_temp_drops`. For a
        // fresh-temp `.iter()`/`.into_iter()` peel it holds the receiver's
        // ELEMENT type, recorded span-keyed by the typechecker — authoritative
        // for the for-loop source and immune to the method-call-chain span
        // collision that can pollute `owned_temp_drops` at the SAME key. When a
        // fresh-temp `.iter()` feeds further adaptors before `collect`
        // (`mk().iter().enumerate().collect()`), the base call, `.iter()`,
        // `.enumerate()`, and `.collect()` all share the base callee's span, so
        // `expr_types` — hence `owned_temp_drops` — at that key holds the
        // OUTERMOST result (`Vec[(i64, T)]`) rather than the source `Vec[T]`.
        // Reading/dropping the source buffer at that wider element stride
        // corrupted the heap (B-2026-07-04-5: `pointer being freed was not
        // allocated`). Preferring the element table reconstructs the correct
        // `Vec[elem]`. For a non-peel Vec-value iterable (`t.iter_axis(n)`) the
        // element table has no entry and `owned_temp_drops` (correct there) is
        // used, so this reorder is a pure fix with no regression. The
        // element-drop threading below is identical for both sources, so heap
        // elements are freed once at scope exit.
        let te = if let Some(elem_te) = self.temp_recv_elem_types.get(&key).cloned() {
            super::Codegen::vec_type_expr_from_element(&elem_te)
        } else if let Some(te) = self.owned_temp_drops.get(&key).cloned() {
            te
        } else {
            return Ok(None);
        };
        let is_vec = matches!(
            &te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        );
        if !is_vec {
            return Ok(None);
        }
        let val = self.compile_expr(iterable)?;
        let fn_val = self.current_fn.unwrap();
        let synth = format!("__for_vec_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        let alloca = self.create_entry_alloca(fn_val, &synth, val.get_type());
        self.builder.build_store(alloca, val).unwrap();
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: alloca,
                ty: val.get_type(),
            },
        );
        self.register_var_from_type_expr(&synth, &te);
        // Queue the materialized temp's scope-exit cleanup. A `Vec[Tensor]`
        // element each owns a heap block (the iter_axis sub-tensors), so
        // route to the tensor-element cleanup; other element types free the
        // buffer (with the existing recursive drop for nested-heap elems).
        let is_tensor_elem = self
            .var_elem_type_exprs
            .get(synth.as_str())
            .cloned()
            .map(|et| self.tensor_var_info_from_type_expr(&et).is_some())
            .unwrap_or(false);
        let map_elem_drop = self
            .var_elem_type_exprs
            .get(synth.as_str())
            .cloned()
            .and_then(|et| self.vec_elem_map_drop_for_type_expr(&et));
        let agg_elem_drop = self
            .var_elem_type_exprs
            .get(synth.as_str())
            .cloned()
            .and_then(|et| self.vec_elem_agg_drop_for_type_expr(&et));
        if is_tensor_elem {
            self.track_vec_of_tensors_var(alloca);
        } else if let Some(map_drop) = map_elem_drop {
            // `Vec[Map]` / `Vec[Set]` iterable temp: the Vec owns its map
            // elements (Cluster 1) — free each handle on drop.
            self.track_vec_of_maps_var(alloca, map_drop);
        } else if let (Some(agg_drop), Some(&elem_ty)) =
            (agg_elem_drop, self.vec_elem_types.get(synth.as_str()))
        {
            // `Vec[<user struct/enum>]` iterable temp: run each element's own
            // drop fn so enum/heap fields the inline recursion misses are
            // freed (B-2026-06-12-6 cluster 2 gap 2).
            self.track_vec_of_aggs_var(alloca, elem_ty, agg_drop);
        } else if let Some(&elem_ty) = self.vec_elem_types.get(synth.as_str()) {
            self.track_vec_var(alloca, Some(elem_ty));
        }
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: iterable.span.clone(),
        };
        let result = self.compile_for(label, pattern, &synth_expr, body);
        // Drop synth registries (the queued cleanup references the alloca,
        // not the name, so it stays armed).
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        result.map(Some)
    }

    /// Iterate a value-producing iterable whose type is a `Map[K,V]` / `Set[T]`
    /// (a fresh-temp `make_map()` / `make_set()` — the Map/Set sibling of
    /// `try_compile_for_vec_value`). The bare form `for (k, v) in make_map()`
    /// reaches here with the receiver's `Map[K,V]` in `owned_temp_drops` (Map/Set
    /// are in the droppable set); the `.iter()` form `for (k, v) in
    /// make_map().iter()` peels `.iter()` and recurses on the receiver, whose
    /// span collides with the `.iter()` MethodCall — `expr_types` holds
    /// `Iterator[(K,V)]` there, so `owned_temp_drops` misses and the whole
    /// `Map[K,V]` / `Set[T]` is recovered from `temp_recv_mapset_types` (recorded
    /// by the fresh-temp Map/Set gate). Without this both forms silently skip the
    /// body (`try_compile_for_vec_value` returns None for a non-Vec) — the loop
    /// summed to 0 vs the interpreter. Materializes the handle into a synth local,
    /// registers it (map_key_types/map_val_types or set_elem_types via
    /// `register_var_from_type_expr`), queues the per-entry-heap-aware
    /// `FreeMapHandle` cleanup, then drives `compile_for_map_var` /
    /// `compile_for_set_var` via the recursed Identifier iterable.
    fn try_compile_for_mapset_value(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        iterable: &Expr,
        body: &Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        use super::state::VarSlot;
        let key = (iterable.span.offset, iterable.span.length);
        let te = if let Some(te) = self.owned_temp_drops.get(&key).cloned() {
            te
        } else if let Some(te) = self.temp_recv_mapset_types.get(&key).cloned() {
            te
        } else {
            return Ok(None);
        };
        let head = match &te.kind {
            TypeKind::Path(p) => p.segments.last().map(|s| s.as_str()),
            _ => None,
        };
        if !matches!(head, Some("Map") | Some("Set")) {
            return Ok(None);
        }
        let val = self.compile_expr(iterable)?;
        let fn_val = self.current_fn.unwrap();
        let synth = format!("__for_mapset_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        let alloca = self.create_entry_alloca(fn_val, &synth, val.get_type());
        self.builder.build_store(alloca, val).unwrap();
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: alloca,
                ty: val.get_type(),
            },
        );
        self.register_var_from_type_expr(&synth, &te);
        // Queue the materialized handle's scope-exit cleanup. `FreeMapHandle`
        // frees the handle and (per `map_temp_cleanup_parts`) drains each stored
        // String/Vec/shared key+value, so a `Map[String, _]` / `Set[String]` temp
        // doesn't leak its entry buffers. Note the arg order: cleanup_parts yields
        // (key_is_vec, val_is_vec, key_shared, val_shared) but `track_map_var`
        // takes (.., val_shared, key_shared).
        let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn) =
            self.map_temp_cleanup_parts(&te);
        self.track_map_var_with_val_drop(
            alloca,
            key_is_vec,
            val_is_vec,
            val_shared,
            key_shared,
            val_drop_fn,
        );
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: iterable.span.clone(),
        };
        let result = self.compile_for(label, pattern, &synth_expr, body);
        // Drop synth registries (the queued cleanup references the alloca).
        self.variables.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        result.map(Some)
    }

    /// `for x in obj.field [.iter() / .into_iter()] { body }` driver.
    /// Recovers the field's pointer (heap-GEP for shared structs,
    /// slot-GEP for plain structs), mints a synth identifier with the
    /// field's TypeExpr-derived registries populated through
    /// `register_var_from_type_expr`, and recurses into `compile_for`
    /// with the synth as the iterable. Returns `Ok(None)` when the
    /// shape isn't a known struct-field receiver — caller falls
    /// through to its own diagnostic. Sibling to
    /// `compile_for_indexed_iter` (Index-receiver path) and
    /// `try_compile_field_receiver_method` (method-call FR path).
    /// Closes the `for nb in curr.neighbors.iter()` surface used by
    /// the clone-graph kata (kata-133), 2026-05-16.
    pub(super) fn try_compile_for_field_iter(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        outer: &Expr,
        field: &str,
        body: &Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        use super::state::VarSlot;
        // `self` parses as `SelfValue`, not `Identifier("self")`. Normalise it
        // to a synthetic `Identifier("self")` (self is registered under the name
        // "self" in every per-binding registry) so the canonical resolver's
        // Identifier arm handles it — mirrors `compile_method_call`'s
        // `self.field.method()` normalisation. Without this, `for s in
        // self.items.iter()` inside an impl method fell through to the
        // dispatcher's silent `_ =>` skip (0-iteration miscompile: the loop
        // summed to the base only, while the interpreter iterated correctly).
        // `lower_field_access_ptr` deliberately leaves a bare `SelfValue` at
        // `Ok(None)` (load-bearing for the atomic-on-self method path), so we
        // must do the normalisation here rather than teach the resolver about
        // SelfValue — atomics are never iterable, so a self-field for-loop
        // source is unambiguously a collection field.
        let self_ident;
        let outer: &Expr = if matches!(outer.kind, ExprKind::SelfValue) {
            self_ident = Expr {
                kind: ExprKind::Identifier("self".to_string()),
                span: outer.span.clone(),
            };
            &self_ident
        } else {
            outer
        };
        // Resolve the field's pointer, LLVM type, and TypeExpr through the
        // canonical field-place resolver (shared with the method-call
        // field-receiver path, `try_compile_field_receiver_method`). It handles
        // every receiver-pointer shape uniformly — Identifier / `outer[i]`,
        // owned vs `ref` bindings vs shared handles, and the Phase-D headerless
        // shared layout — so the for-loop path can't drift from the method path
        // (the hand-rolled copy this replaced missed `self`, mis-GEP'd `ref
        // self`, and hardcoded the header-shifted shared layout). Returns
        // `Ok(None)` for an unrecognized shape (dispatcher falls through to the
        // silent-skip default) and `Err` for the deferred chained-receiver forms.
        let (field_ptr, field_ll_ty, field_te) =
            match self.lower_field_access_ptr(outer, field, "for-loop field iterable")? {
                Some(t) => t,
                None => return Ok(None),
            };
        // Mint a synth identifier aliasing the field storage and
        // populate its registries. `register_var_from_type_expr`
        // covers Vec/Slice/String/Map/Set element-type tables and
        // also propagates `var_type_names` for bare user-struct
        // types (the regression-fix in this same commit).
        let synth = format!("__for_field_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: field_ptr,
                ty: field_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &field_te);

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: outer.span.clone(),
        };
        let result = self.compile_for(label, pattern, &synth_expr, body);

        // Clean up synth registrations so they don't leak across
        // sibling for-loops at the same nesting depth.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);
        self.string_vars.remove(&synth);

        result.map(Some)
    }

    /// Compile a loop body inside a fresh per-iteration cleanup frame, then
    /// (on a normal fall-through) drop the owned heap locals declared in the
    /// body and branch to `continue_bb`.
    ///
    /// Without this, every for-over-collection variant leaked: body-local
    /// `let v = <owned Vec/String/…>` bindings registered their
    /// `FreeVecBuffer`/drop in the *enclosing* (function) frame, so only the
    /// final iteration's value was freed at the function tail — N-1
    /// iterations' worth leaked (B-2026-06-14-21; `for-over-range` already
    /// had this via its own push/drain, which is why only the collection
    /// variants leaked). A body terminator (break/continue/return) routes
    /// cleanup through the `loop_stack` `cleanup_depth` walk instead, so on
    /// that path the frame is popped WITHOUT emitting (it was already
    /// drained) and no trailing branch is added.
    pub(super) fn compile_loop_body_with_cleanup(
        &mut self,
        body: &Block,
        continue_bb: inkwell::basic_block::BasicBlock<'ctx>,
    ) -> Result<(), String> {
        self.scope_cleanup_actions.push(Vec::new());
        self.compile_block(body)?;
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            self.drain_top_frame_with_emit();
            self.builder
                .build_unconditional_branch(continue_bb)
                .unwrap();
        } else {
            self.scope_cleanup_actions.pop();
        }
        Ok(())
    }

    pub(super) fn compile_for_range(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.compile_for_range_with_step(label, pattern, start, end, inclusive, None, body)
    }

    /// Generic for-range codegen with an optional step expression.
    /// Step expr `Some(expr)` evaluates `expr` once before the loop
    /// and uses the result as the increment; `None` defaults to 1.
    /// Drives both the plain `for i in start..end` shape and the
    /// `for i in (start..end).step_by(n)` peel-off in `compile_for`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn compile_for_range_with_step(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
        step: Option<&Expr>,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();

        let start_val = if let Some(s) = start {
            self.compile_expr(s)?.into_int_value()
        } else {
            i64_t.const_int(0, false)
        };
        let end_val = if let Some(e) = end {
            self.compile_expr(e)?.into_int_value()
        } else {
            return Err("for-range loop requires an end bound".to_string());
        };
        // Evaluate the step expression once before the loop and stash
        // it. Default to 1 when absent.
        let step_val = if let Some(s) = step {
            self.compile_expr(s)?.into_int_value()
        } else {
            i64_t.const_int(1, false)
        };

        // `for i in (a..b).rev()` / `(a..=b).rev()` — reverse iteration over the
        // SAME value set, just descending (B-2026-07-18-41 residual). The rev
        // guard in `compile_for` strips `.rev()`, sets this one-shot signal, and
        // recurses on the bare range; consume+clear it here (nesting-safe, like
        // `compile_for_vec_var`). Reversal only reorders `[start, end)` — every
        // value is still visited — so the bounds-check-elision facts
        // (`collect_asserted_bounds_from_for_range`, `start <= i < end`) below
        // stay valid unchanged. `rev_chain_reverse_iterable` gates the signal to
        // a BARE range (no `step_by`), so `reverse` is only ever paired with the
        // default unit step.
        let reverse = std::mem::take(&mut self.pending_reverse_iter);

        // Allocate loop counter. Forward starts at `start`; reverse starts at
        // the last value visited (`end - 1` exclusive, `end` inclusive).
        let init_val = if reverse {
            if inclusive {
                end_val
            } else {
                self.builder
                    .build_int_sub(end_val, i64_t.const_int(1, false), "rev.init")
                    .unwrap()
            }
        } else {
            start_val
        };
        let counter = self.create_entry_alloca(fn_val, "for.i", i64_t.into());
        self.builder.build_store(counter, init_val).unwrap();

        // Monotone-variable BCE preheader loads (control_flow_bce.rs §
        // monotone scan) — the loop var itself is covered by the
        // for-range bounds below; this targets body-updated `let mut`
        // cursors (e.g. a compaction write head `k`).
        let mono_vars = self.collect_monotone_vars(None, body);
        let mono_inits = self.load_monotone_inits(&mono_vars);

        let cond_bb = self.context.append_basic_block(fn_val, "for.cond");
        let body_bb = self.context.append_basic_block(fn_val, "for.body");
        let incr_bb = self.context.append_basic_block(fn_val, "for.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "for.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        // Condition: i < end (or i <= end for inclusive)
        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        // Reverse descends until it passes `start` (`i >= start`); forward
        // ascends until it reaches `end` (`i < end`, or `<= end` inclusive).
        let (pred, bound) = if reverse {
            (IntPredicate::SGE, start_val)
        } else if inclusive {
            (IntPredicate::SLE, end_val)
        } else {
            (IntPredicate::SLT, end_val)
        };
        let cond = self
            .builder
            .build_int_compare(pred, cur, bound, "for.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        // Body: bind pattern, compile block
        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap();
        self.bind_pattern(pattern, cur)?;
        // Bounds-check elision: a for-range loop establishes `start <= i < end`
        // (or `<= end` for inclusive). Push the facts compile_vec_index /
        // compile_slice_index need to elide the bounds check on `v[i]`
        // inside the body. The conservative rules match what we can prove
        // without arithmetic reasoning: start = 0 / non-negative literal
        // gives a lower bound; end resolving to a Vec/Slice's `.len()`
        // (directly or via a local alias) gives an upper bound, only for
        // exclusive ranges (inclusive ranges include the end value, which
        // would be OOB on `v[end]`).
        let pushed_for_bounds =
            self.collect_asserted_bounds_from_for_range(pattern, start, end, inclusive);
        let pushed_for_count = pushed_for_bounds.len();
        self.asserted_index_bounds.extend(pushed_for_bounds);
        // Monotone facts at body entry (pairs with the preheader loads
        // above) — see compile_while's twin call for rationale.
        self.emit_monotone_assumes(&mono_inits);
        // Per-iteration scope frame for body-local lets — the alloca lives
        // for the whole function (entry-block one-shot), but a `let node
        // = SharedT { … }` rebound on every iteration must drop the
        // previous iteration's value before the next store, or the
        // refcount climbs N×K and the chain leaks. Pushing a frame here
        // and draining it just before the increment branch emits one
        // rc_dec per body-local shared-struct let per iteration. Matches
        // the match-arm push/drain pattern in `control_flow_match.rs`.
        // Function-tail `emit_scope_cleanup` no longer walks these
        // bindings (the frame is gone by the time control reaches the
        // function tail), so the slot's null sentinel (emitted by
        // `null_init_slot_in_entry_block` for nested-block shared-struct
        // lets) only matters for the unreachable-body case, not the
        // iterate-then-cleanup case.
        self.scope_cleanup_actions.push(Vec::new());
        self.compile_block(body)?;
        for _ in 0..pushed_for_count {
            self.asserted_index_bounds.pop();
        }
        let body_has_terminator = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_has_terminator {
            self.drain_top_frame_with_emit();
            self.builder.build_unconditional_branch(incr_bb).unwrap();
        } else {
            // Body ended with a terminator (break / continue / return) —
            // the early-exit path's own cleanup walk already handled
            // every frame in the stack including this one. Pop without
            // emitting so the frame doesn't shadow the surrounding
            // scope's bindings.
            self.scope_cleanup_actions.pop();
        }

        // Increment by `step_val`
        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        // Arithmetic-flags (Codegen Optimization § nsw/nuw): for the canonical
        // ascending exclusive range with the default step of 1 (`for i in a..b`),
        // the increment `cur + 1` is reached only when `cur < end` (the `for.cond`
        // guard), so `cur + 1 <= end <= i64::MAX` — it provably never
        // signed-overflows. Tagging it `nsw` lets ScalarEvolution model the
        // induction variable as a wrap-free affine recurrence, unlocking
        // trip-count analysis, IV widening, and loop vectorization (the win this
        // section targets). This is sound *despite* Kāra's "overflow is always
        // defined" guarantee — that guarantee governs USER arithmetic, whereas
        // this is a compiler-generated counter the compiler proves bounded.
        // NOT tagged `nuw`: a range with a negative start makes the counter
        // negative, where `+1` would unsigned-wrap. An explicit `step` or an
        // inclusive range (`..=end`, whose final `+1` can reach `end + 1`) stays
        // on the plain add — those cannot be proven wrap-free here.
        let next = if reverse {
            // Descending: `cur - step_val` (step is always the unit default in
            // the reverse path — see the `reverse` gate above).
            self.builder.build_int_sub(cur, step_val, "decr").unwrap()
        } else if step.is_none() && !inclusive {
            self.builder
                .build_int_nsw_add(cur, step_val, "incr")
                .unwrap()
        } else {
            self.builder.build_int_add(cur, step_val, "incr").unwrap()
        };
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        // Vec-length-pin activation for the `for i in 0..BOUND { v.push(..) }`
        // fill form (bce_length_pin.rs): the pin is keyed on the range END
        // expression's span. Now that the loop is fully emitted, move it live so
        // a later `while c < BOUND` guard elides `v[c]`'s upper bounds check.
        if let Some(end_expr) = end.as_deref() {
            let end_key = crate::resolver::SpanKey::from_span(&end_expr.span);
            if let Some(pin) = self.pending_vec_len_pins.remove(&end_key) {
                self.vec_len_pins.push((pin.bound, pin.vec_var));
            }
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_for_slice_var(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        var_name: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let slice_ty = self.slice_struct_type();
        let elem_ty = *self.slice_elem_types.get(var_name).unwrap();
        let slice_ptr = self.get_data_ptr(var_name).unwrap();

        let data_pp = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 0, "for.s.data.pp")
            .unwrap();
        let len_p = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 1, "for.s.len.p")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "for.s.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_p, "for.s.len")
            .unwrap()
            .into_int_value();

        let counter = self.create_entry_alloca(fn_val, "for.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_int(0, false))
            .unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "for.s.cond");
        let body_bb = self.context.append_basic_block(fn_val, "for.s.body");
        let incr_bb = self.context.append_basic_block(fn_val, "for.s.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "for.s.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur, len, "for.s.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[cur], "for.s.elem.ptr")
                .unwrap()
        };
        let elem_val = self
            .builder
            .build_load(elem_ty, elem_ptr, "for.s.elem")
            .unwrap();
        self.bind_pattern(pattern, elem_val)?;
        self.register_for_loop_bindings(pattern, var_name);
        self.bind_enumerate_index(cur)?;
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let one = i64_t.const_int(1, false);
        let next = self.builder.build_int_add(cur, one, "incr").unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// `for line in stdin.lines()` (phase-8 `Stdin.lines()` slice). Pulls one
    /// line per iteration from `karac_runtime_stdin_next_line`, which strips the
    /// trailing newline, writes `Result[String, IoError]` into a stack slot, and
    /// returns a 3-state code: `0` EOF (stop, no body), `1` Ok line (body then
    /// continue), `2` Err (body then stop) — matching the interpreter's
    /// `Value::StdinLines` drain exactly. `lower_kara_io_result` rebuilds the
    /// `Result` value each iteration; the body binds it as the loop variable and
    /// the loop's scope cleanup drops the owned payload per iteration.
    pub(super) fn compile_for_stdin_lines(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "codegen: stdin.lines() iterated outside a function".to_string())?;
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let i32_t = self.context.i32_type();
        let io_ty = self.kara_io_result_type();

        // Entry-block scratch: the KaracIoResult slot the extern writes each
        // line into, and the return code (reloaded in the latch to break after
        // an Err item).
        let slot = self.create_entry_alloca(fn_val, "stdin.lines.slot", io_ty.into());
        let rc_slot = self.create_entry_alloca(fn_val, "stdin.lines.rc", i32_t.into());

        let symbol = "karac_runtime_stdin_next_line";
        let next_fn = match self.module.get_function(symbol) {
            Some(f) => f,
            None => {
                let fn_ty = i32_t.fn_type(&[ptr_t.into()], false);
                self.module.add_function(symbol, fn_ty, None)
            }
        };

        let cond_bb = self.context.append_basic_block(fn_val, "stdin.lines.cond");
        let body_bb = self.context.append_basic_block(fn_val, "stdin.lines.body");
        let latch_bb = self.context.append_basic_block(fn_val, "stdin.lines.latch");
        let exit_bb = self.context.append_basic_block(fn_val, "stdin.lines.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: latch_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        // cond: pull the next line. `rc == 0` (EOF) → exit; else → body.
        self.builder.position_at_end(cond_bb);
        let rc = self
            .builder
            .build_call(next_fn, &[slot.into()], "stdin.lines.rc.call")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder.build_store(rc_slot, rc).unwrap();
        let is_eof = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                rc,
                i32_t.const_int(0, false),
                "stdin.lines.eof",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_eof, exit_bb, body_bb)
            .unwrap();

        // body: rebuild the `Result[String, IoError]`, bind it, run the body.
        self.builder.position_at_end(body_bb);
        let item = self.lower_kara_io_result(slot, super::file::FileOkKind::StringPayload)?;
        self.bind_pattern(pattern, item)?;
        self.compile_loop_body_with_cleanup(body, latch_bb)?;

        // latch: after the body, break on an Err item (`rc == 2`), else loop.
        self.builder.position_at_end(latch_bb);
        let rc2 = self
            .builder
            .build_load(i32_t, rc_slot, "stdin.lines.rc2")
            .unwrap()
            .into_int_value();
        let is_err = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                rc2,
                i32_t.const_int(2, false),
                "stdin.lines.err",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_err, exit_bb, cond_bb)
            .unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_for_vec_var(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        var_name: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.vec_elem_type_for_var(var_name);
        let vec_ptr = self.get_data_ptr(var_name).unwrap();
        // `Iterator.rev()` (B-2026-07-18-41): consume the one-shot reverse signal.
        // Loop control stays `0..len`; only the ELEMENT INDEX is mirrored to
        // `len-1-i`, so all the per-iteration cleanup/binding machinery is
        // untouched. Cleared here so a nested loop in the body isn't affected.
        let reverse = std::mem::take(&mut self.pending_reverse_iter);

        // Load len and data pointer.
        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 1, "for.v.len.ptr")
            .unwrap();
        let data_ptr_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 0, "for.v.data.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "for.v.len")
            .unwrap()
            .into_int_value();
        // `!range [0, 2^61)` — folds overflow checks on loop-bound
        // arithmetic derived from this len (see `annotate_len_load_range`,
        // B-2026-07-10-5).
        self.annotate_len_load_range(len.into(), Some(elem_ty));
        let data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "for.v.data")
            .unwrap()
            .into_pointer_value();

        let counter = self.create_entry_alloca(fn_val, "for.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_int(0, false))
            .unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "for.cond");
        let body_bb = self.context.append_basic_block(fn_val, "for.body");
        let incr_bb = self.context.append_basic_block(fn_val, "for.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "for.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        // Condition: i < len
        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur, len, "for.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        // Body: load data[idx], bind, execute — `idx = i` forward, `len-1-i`
        // reversed (`rev()`; loop still counts `0..len`).
        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let idx = if reverse {
            let len_m1 = self
                .builder
                .build_int_sub(len, i64_t.const_int(1, false), "for.v.rev.lm1")
                .unwrap();
            self.builder
                .build_int_sub(len_m1, cur, "for.v.rev.idx")
                .unwrap()
        } else {
            cur
        };
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx], "for.v.elem.ptr")
                .unwrap()
        };
        let elem_val = self
            .builder
            .build_load(elem_ty, elem_ptr, "for.v.elem")
            .unwrap();
        self.bind_pattern(pattern, elem_val)?;
        self.register_for_loop_bindings(pattern, var_name);
        self.bind_enumerate_index(cur)?;
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        // Increment
        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let one = i64_t.const_int(1, false);
        let next = self.builder.build_int_add(cur, one, "incr").unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// `for x in xs.iter_mut() { *x = … }` over a named Vec with a SCALAR
    /// element (B-2026-07-14-10). Mirrors `compile_for_vec_var`'s index-loop
    /// skeleton, but instead of loading the element BY VALUE, binds the loop
    /// variable as a MUT-REF: a single ptr alloca holds `data + i*stride`
    /// (restored each iteration), and the variable is registered in
    /// `entry_slot_ref_vars` so the existing deref machinery routes `*x`
    /// reads, `*x = v` stores, and `*x += 1` compound stores through the live
    /// element pointer — writes land in the Vec's storage in place. The
    /// element pointer stays valid across the body: the borrow forbids
    /// mutating `xs` itself inside the loop (no push/realloc), so `data` is
    /// loaded once up front like the by-value loop. Scalar elements only
    /// (caller-gated): a heap element written through the ref would need the
    /// old payload dropped first, which is the deferred heap leg.
    fn compile_for_vec_var_iter_mut(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        var_name: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        use super::state::VarSlot;
        let PatternKind::Binding(elem_name) = &pattern.kind else {
            return Err(
                "codegen: `for … in xs.iter_mut()` requires a simple binding pattern".to_string(),
            );
        };
        let elem_name = elem_name.clone();
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.vec_elem_type_for_var(var_name);
        let vec_ptr = self.get_data_ptr(var_name).unwrap();

        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 1, "form.len.ptr")
            .unwrap();
        let data_ptr_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 0, "form.data.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "form.len")
            .unwrap()
            .into_int_value();
        let data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "form.data")
            .unwrap()
            .into_pointer_value();

        let counter = self.create_entry_alloca(fn_val, "form.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_int(0, false))
            .unwrap();
        // The loop variable's slot holds the ELEMENT POINTER (mut ref), not
        // the element value. Save any shadowed binding/tag and restore after.
        let ref_alloca = self.create_entry_alloca(fn_val, &elem_name, ptr_ty.into());
        let saved_var = self.variables.insert(
            elem_name.clone(),
            VarSlot {
                ptr: ref_alloca,
                ty: ptr_ty.into(),
            },
        );
        let saved_slot_tag = self.entry_slot_ref_vars.insert(elem_name.clone(), elem_ty);

        let cond_bb = self.context.append_basic_block(fn_val, "form.cond");
        let body_bb = self.context.append_basic_block(fn_val, "form.body");
        let incr_bb = self.context.append_basic_block(fn_val, "form.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "form.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "form.i.cur")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur, len, "form.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        // Body: store the element's address into the loop var's ptr slot.
        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "form.i.body")
            .unwrap()
            .into_int_value();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[cur], "form.elem.ptr")
                .unwrap()
        };
        self.builder.build_store(ref_alloca, elem_ptr).unwrap();
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "form.i.incr")
            .unwrap()
            .into_int_value();
        let one = i64_t.const_int(1, false);
        let next = self.builder.build_int_add(cur, one, "form.next").unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        // Restore whatever the loop variable's name previously bound.
        match saved_var {
            Some(s) => {
                self.variables.insert(elem_name.clone(), s);
            }
            None => {
                self.variables.remove(&elem_name);
            }
        }
        match saved_slot_tag {
            Some(t) => {
                self.entry_slot_ref_vars.insert(elem_name, t);
            }
            None => {
                self.entry_slot_ref_vars.remove(&elem_name);
            }
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Peel a transparent `.iter()`/`.into_iter()` off `e` and return the
    /// receiver's name iff it is a registered Vec binding with a SCALAR
    /// (trivially-copyable) element — the shape the zip lockstep loop
    /// supports. `None` for every other shape (the caller falls through to
    /// the loud adaptor bail).
    fn peel_iter_to_scalar_vec_ident(&self, e: &Expr) -> Option<String> {
        self.peel_iter_to_vec_ident(e).filter(|n| {
            self.var_elem_type_exprs
                .get(n.as_str())
                .is_some_and(super::vec_method::is_trivially_copyable_te)
        })
    }

    /// Like `peel_iter_to_scalar_vec_ident` but element-type-agnostic: any
    /// named Vec binding qualifies. Used by the legs that pair the by-value
    /// element bind with `register_for_loop_bindings` — which borrow-marks
    /// heap elements exactly like the single-source Vec loop, making heap
    /// (String/Vec/struct) elements safe (B-2026-07-14-8, heap legs).
    fn peel_iter_to_vec_ident(&self, e: &Expr) -> Option<String> {
        let inner = match &e.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if args.is_empty() && (method == "iter" || method == "into_iter") => object.as_ref(),
            _ => e,
        };
        let ExprKind::Identifier(n) = &inner.kind else {
            return None;
        };
        if self.vec_elem_types.contains_key(n.as_str()) {
            Some(n.clone())
        } else {
            None
        }
    }

    /// `for p in xs.iter().enumerate()` with a SINGLE-VAR binding over a named
    /// scalar-element Vec (B-2026-07-14-8): the by-value Vec loop skeleton,
    /// but each iteration binds `p` to a fresh `{i64 index, T element}` tuple
    /// struct — `p.0` / `p.1` then extract through the normal TupleIndex path.
    /// Scalar elements only (caller-gated): a heap element inserted into the
    /// tuple would be owned by both the container and the tuple (double-drop).
    fn compile_for_enumerate_single_var(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        var_name: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.vec_elem_type_for_var(var_name);
        let tup_ty = self.context.struct_type(&[i64_t.into(), elem_ty], false);
        let vec_ptr = self.get_data_ptr(var_name).unwrap();

        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 1, "fore.len.ptr")
            .unwrap();
        let data_ptr_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 0, "fore.data.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "fore.len")
            .unwrap()
            .into_int_value();
        let data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "fore.data")
            .unwrap()
            .into_pointer_value();

        let counter = self.create_entry_alloca(fn_val, "fore.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_int(0, false))
            .unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "fore.cond");
        let body_bb = self.context.append_basic_block(fn_val, "fore.body");
        let incr_bb = self.context.append_basic_block(fn_val, "fore.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "fore.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "fore.i.cur")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur, len, "fore.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "fore.i.body")
            .unwrap()
            .into_int_value();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[cur], "fore.elem.ptr")
                .unwrap()
        };
        let elem_val = self
            .builder
            .build_load(elem_ty, elem_ptr, "fore.elem")
            .unwrap();
        let mut tup = tup_ty.get_undef();
        tup = self
            .builder
            .build_insert_value(tup, cur, 0, "fore.tup.i")
            .unwrap()
            .into_struct_value();
        tup = self
            .builder
            .build_insert_value(tup, elem_val, 1, "fore.tup.e")
            .unwrap()
            .into_struct_value();
        self.bind_pattern(pattern, tup.into())?;
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "fore.i.incr")
            .unwrap()
            .into_int_value();
        let one = i64_t.const_int(1, false);
        let next = self.builder.build_int_add(cur, one, "fore.next").unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// `for x in xs.iter().skip(a).take(b)…` over a named scalar-element Vec
    /// (B-2026-07-14-8, skip/take leg). Walks the adaptor chain outermost→
    /// innermost, collecting `skip`/`take` links; if the chain bottoms out at
    /// a named scalar Vec (through `.iter()`/`.into_iter()`), folds the links
    /// (innermost first) into a runtime `[start, end)` window —
    /// `skip(n)`: `start = min(start + n, end)`; `take(n)`:
    /// `end = min(start + n, end)` — and runs a by-value index loop over the
    /// window. Returns `Ok(None)` (caller falls through to the loud bail) for
    /// any non-skip/take link, non-Vec source, or heap element.
    fn try_compile_for_skip_take_chain(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        iterable: &Expr,
        body: &Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Collect the chain: outermost link first.
        let mut links: Vec<(bool, Expr)> = Vec::new(); // (is_skip, count-expr)
        let mut cur = iterable;
        loop {
            match &cur.kind {
                ExprKind::MethodCall {
                    object,
                    method,
                    args,
                    ..
                } if args.len() == 1 && (method == "skip" || method == "take") => {
                    links.push((method == "skip", args[0].value.clone()));
                    cur = object.as_ref();
                }
                _ => break,
            }
        }
        let Some(var_name) = self.peel_iter_to_vec_ident(cur) else {
            return Ok(None);
        };

        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.vec_elem_type_for_var(&var_name);
        let vec_ptr = self.get_data_ptr(&var_name).unwrap();

        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 1, "forw.len.ptr")
            .unwrap();
        let data_ptr_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 0, "forw.data.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "forw.len")
            .unwrap()
            .into_int_value();
        let data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "forw.data")
            .unwrap()
            .into_pointer_value();

        // Fold the window, INNERMOST link first (the collection is
        // outermost-first, so iterate in reverse). All values are lengths /
        // counts (non-negative by construction in well-typed programs); the
        // min-clamps keep the window inside `[0, len]` regardless.
        let mut start = i64_t.const_int(0, false);
        let mut end = len;
        for (i, (is_skip, count_expr)) in links.iter().rev().enumerate() {
            let n_raw = self.compile_expr(count_expr)?.into_int_value();
            // Clamp a NEGATIVE count to 0 first (signed compare) — the
            // interpreter clamps (`n.max(0)`, method_call_iter.rs): `take(-1)`
            // yields nothing, `skip(-1)` skips nothing. Without this, the
            // unsigned window arithmetic below treats -1 as a huge count and
            // INVERTS both semantics (take(-1) yielded everything).
            let zero = i64_t.const_int(0, false);
            let is_neg = self
                .builder
                .build_int_compare(IntPredicate::SLT, n_raw, zero, &format!("forw.neg{i}"))
                .unwrap();
            let n = self
                .builder
                .build_select(is_neg, zero, n_raw, &format!("forw.nclamp{i}"))
                .unwrap()
                .into_int_value();
            let moved = self
                .builder
                .build_int_add(start, n, &format!("forw.adj{i}"))
                .unwrap();
            let lt = self
                .builder
                .build_int_compare(IntPredicate::ULT, moved, end, &format!("forw.clamp{i}"))
                .unwrap();
            let clamped = self
                .builder
                .build_select(lt, moved, end, &format!("forw.sel{i}"))
                .unwrap()
                .into_int_value();
            if *is_skip {
                start = clamped;
            } else {
                end = clamped;
            }
        }

        let counter = self.create_entry_alloca(fn_val, "forw.i", i64_t.into());
        self.builder.build_store(counter, start).unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "forw.cond");
        let body_bb = self.context.append_basic_block(fn_val, "forw.body");
        let incr_bb = self.context.append_basic_block(fn_val, "forw.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "forw.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        self.builder.position_at_end(cond_bb);
        let cur_i = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "forw.i.cur")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur_i, end, "forw.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let cur_i = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "forw.i.body")
            .unwrap()
            .into_int_value();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[cur_i], "forw.elem.ptr")
                .unwrap()
        };
        let elem_val = self
            .builder
            .build_load(elem_ty, elem_ptr, "forw.elem")
            .unwrap();
        self.bind_pattern(pattern, elem_val)?;
        // Borrow-mark heap element bindings (B-2026-07-14-8, heap legs).
        self.register_for_loop_bindings(pattern, &var_name);
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        self.builder.position_at_end(incr_bb);
        let cur_i = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "forw.i.incr")
            .unwrap()
            .into_int_value();
        let one = i64_t.const_int(1, false);
        let next = self.builder.build_int_add(cur_i, one, "forw.next").unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(Some(self.context.i64_type().const_int(0, false).into()))
    }

    /// `for x in va.iter().chain(vb.iter())` over two named scalar-element
    /// Vecs (B-2026-07-14-8, chain leg): two sequential by-value index loops
    /// — all of `va`, then all of `vb` — binding each element to the same
    /// pattern and compiling the body once per source. Both loops share ONE
    /// exit block and each pushes its own `LoopFrame` with that shared
    /// `break_bb`, so a `break` in the first source's body leaves the WHOLE
    /// chain (it must not fall into the second source); `continue` targets
    /// the current source's own increment. Scalar elements only
    /// (caller-gated).
    fn compile_for_chain_vec_vars(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        va: &str,
        vb: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let shared_exit = self.context.append_basic_block(fn_val, "chain.exit");

        for (leg, name) in [(0usize, va), (1usize, vb)] {
            let elem_ty = self.vec_elem_type_for_var(name);
            let vptr = self
                .get_data_ptr(name)
                .ok_or_else(|| format!("codegen: chain source '{name}' has no storage"))?;
            let len_ptr = self
                .builder
                .build_struct_gep(vec_ty, vptr, 1, &format!("chain{leg}.len.ptr"))
                .unwrap();
            let data_ptr_ptr = self
                .builder
                .build_struct_gep(vec_ty, vptr, 0, &format!("chain{leg}.data.ptr"))
                .unwrap();
            let len = self
                .builder
                .build_load(i64_t, len_ptr, &format!("chain{leg}.len"))
                .unwrap()
                .into_int_value();
            let data = self
                .builder
                .build_load(ptr_ty, data_ptr_ptr, &format!("chain{leg}.data"))
                .unwrap()
                .into_pointer_value();

            let counter = self.create_entry_alloca(fn_val, &format!("chain{leg}.i"), i64_t.into());
            self.builder
                .build_store(counter, i64_t.const_int(0, false))
                .unwrap();

            let cond_bb = self
                .context
                .append_basic_block(fn_val, &format!("chain{leg}.cond"));
            let body_bb = self
                .context
                .append_basic_block(fn_val, &format!("chain{leg}.body"));
            let incr_bb = self
                .context
                .append_basic_block(fn_val, &format!("chain{leg}.incr"));
            let next_bb = if leg == 0 {
                self.context.append_basic_block(fn_val, "chain.between")
            } else {
                shared_exit
            };

            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.loop_stack.push(LoopFrame {
                label: label.map(str::to_string),
                continue_bb: incr_bb,
                break_bb: shared_exit,
                result_slot: None,
                cleanup_depth: self.scope_cleanup_actions.len(),
            });

            self.builder.position_at_end(cond_bb);
            let cur = self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(
                    i64_t.into(),
                    counter,
                    &format!("chain{leg}.i.cur"),
                )
                .unwrap()
                .into_int_value();
            let cond = self
                .builder
                .build_int_compare(IntPredicate::ULT, cur, len, &format!("chain{leg}.cond"))
                .unwrap();
            self.builder
                .build_conditional_branch(cond, body_bb, next_bb)
                .unwrap();

            self.builder.position_at_end(body_bb);
            let cur = self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(
                    i64_t.into(),
                    counter,
                    &format!("chain{leg}.i.body"),
                )
                .unwrap()
                .into_int_value();
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(elem_ty, data, &[cur], &format!("chain{leg}.elem.ptr"))
                    .unwrap()
            };
            let elem_val = self
                .builder
                .build_load(elem_ty, elem_ptr, &format!("chain{leg}.elem"))
                .unwrap();
            self.bind_pattern(pattern, elem_val)?;
            // Borrow-mark heap element bindings per leg (B-2026-07-14-8,
            // heap legs) — both sources share the element type.
            self.register_for_loop_bindings(pattern, name);
            self.compile_loop_body_with_cleanup(body, incr_bb)?;

            self.builder.position_at_end(incr_bb);
            let cur = self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(
                    i64_t.into(),
                    counter,
                    &format!("chain{leg}.i.incr"),
                )
                .unwrap()
                .into_int_value();
            let one = i64_t.const_int(1, false);
            let next = self
                .builder
                .build_int_add(cur, one, &format!("chain{leg}.next"))
                .unwrap();
            self.builder.build_store(counter, next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.loop_stack.pop();
            // Position at the between-block so the second leg's preamble
            // (len/data loads, counter init) lands there; the final leg
            // positions at the shared exit below.
            self.builder.position_at_end(next_bb);
        }
        // Builder is already at `shared_exit` (the second leg's next_bb).
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// `for (a, b) in va.iter().zip(vb.iter())` over two named scalar-element
    /// Vecs (B-2026-07-14-8, zip leg): a lockstep index loop over
    /// `0..min(lenA, lenB)` binding `A[i]` to `pat_a` and `B[i]` to `pat_b`
    /// each iteration. Scalar elements only (caller-gated), so the by-value
    /// binds need no loop-borrow / defensive-copy machinery.
    fn compile_for_zip_vec_vars(
        &mut self,
        label: Option<&str>,
        pat_a: &Pattern,
        pat_b: &Pattern,
        va: &str,
        vb: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();

        let load_parts = |name: &str,
                          tag: &str|
         -> Result<
            (IntValue<'ctx>, PointerValue<'ctx>, BasicTypeEnum<'ctx>),
            String,
        > {
            let elem_ty = self.vec_elem_type_for_var(name);
            let vptr = self
                .get_data_ptr(name)
                .ok_or_else(|| format!("codegen: zip source '{name}' has no storage"))?;
            let len_ptr = self
                .builder
                .build_struct_gep(vec_ty, vptr, 1, &format!("zip.{tag}.len.ptr"))
                .unwrap();
            let data_ptr_ptr = self
                .builder
                .build_struct_gep(vec_ty, vptr, 0, &format!("zip.{tag}.data.ptr"))
                .unwrap();
            let len = self
                .builder
                .build_load(i64_t, len_ptr, &format!("zip.{tag}.len"))
                .unwrap()
                .into_int_value();
            let data = self
                .builder
                .build_load(ptr_ty, data_ptr_ptr, &format!("zip.{tag}.data"))
                .unwrap()
                .into_pointer_value();
            Ok((len, data, elem_ty))
        };
        let (len_a, data_a, elem_ty_a) = load_parts(va, "a")?;
        let (len_b, data_b, elem_ty_b) = load_parts(vb, "b")?;
        // n = min(lenA, lenB) — zip stops at the shorter source.
        let a_lt_b = self
            .builder
            .build_int_compare(IntPredicate::ULT, len_a, len_b, "zip.minsel")
            .unwrap();
        let n = self
            .builder
            .build_select(a_lt_b, len_a, len_b, "zip.n")
            .unwrap()
            .into_int_value();

        let counter = self.create_entry_alloca(fn_val, "zip.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_int(0, false))
            .unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "zip.cond");
        let body_bb = self.context.append_basic_block(fn_val, "zip.body");
        let incr_bb = self.context.append_basic_block(fn_val, "zip.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "zip.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "zip.i.cur")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur, n, "zip.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "zip.i.body")
            .unwrap()
            .into_int_value();
        let a_ptr = unsafe {
            self.builder
                .build_gep(elem_ty_a, data_a, &[cur], "zip.a.ptr")
                .unwrap()
        };
        let a_val = self.builder.build_load(elem_ty_a, a_ptr, "zip.a").unwrap();
        let b_ptr = unsafe {
            self.builder
                .build_gep(elem_ty_b, data_b, &[cur], "zip.b.ptr")
                .unwrap()
        };
        let b_val = self.builder.build_load(elem_ty_b, b_ptr, "zip.b").unwrap();
        self.bind_pattern(pat_a, a_val)?;
        self.bind_pattern(pat_b, b_val)?;
        // Borrow-mark heap element bindings like the single-source Vec loop
        // (B-2026-07-14-8, heap legs) — each side against its own source.
        self.register_for_loop_bindings(pat_a, va);
        self.register_for_loop_bindings(pat_b, vb);
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "zip.i.incr")
            .unwrap()
            .into_int_value();
        let one = i64_t.const_int(1, false);
        let next = self.builder.build_int_add(cur, one, "zip.next").unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// `for pair in va.iter().zip(vb.iter())` over two named SCALAR-element Vecs
    /// (B-2026-07-15-10, single-binding zip leg): the lockstep `0..min(lenA,
    /// lenB)` loop of `compile_for_zip_vec_vars`, but instead of destructuring
    /// into two sub-patterns it binds the WHOLE `(EA, EB)` tuple to `pattern`,
    /// so a body reading `pair.0` / `pair.1` works — the two-source sibling of
    /// `compile_for_enumerate_single_var`. Scalar elements only (caller-gated
    /// via `peel_iter_to_scalar_vec_ident`), so the by-value tuple binds need no
    /// loop-borrow / defensive-copy machinery.
    fn compile_for_zip_single_var(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        va: &str,
        vb: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();

        let load_parts = |me: &Self,
                          name: &str,
                          tag: &str|
         -> Result<
            (IntValue<'ctx>, PointerValue<'ctx>, BasicTypeEnum<'ctx>),
            String,
        > {
            let elem_ty = me.vec_elem_type_for_var(name);
            let vptr = me
                .get_data_ptr(name)
                .ok_or_else(|| format!("codegen: zip source '{name}' has no storage"))?;
            let len_ptr = me
                .builder
                .build_struct_gep(vec_ty, vptr, 1, &format!("zip1.{tag}.len.ptr"))
                .unwrap();
            let data_ptr_ptr = me
                .builder
                .build_struct_gep(vec_ty, vptr, 0, &format!("zip1.{tag}.data.ptr"))
                .unwrap();
            let len = me
                .builder
                .build_load(i64_t, len_ptr, &format!("zip1.{tag}.len"))
                .unwrap()
                .into_int_value();
            let data = me
                .builder
                .build_load(ptr_ty, data_ptr_ptr, &format!("zip1.{tag}.data"))
                .unwrap()
                .into_pointer_value();
            Ok((len, data, elem_ty))
        };
        let (len_a, data_a, elem_ty_a) = load_parts(self, va, "a")?;
        let (len_b, data_b, elem_ty_b) = load_parts(self, vb, "b")?;
        let tup_ty = self.context.struct_type(&[elem_ty_a, elem_ty_b], false);
        // n = min(lenA, lenB) — zip stops at the shorter source.
        let a_lt_b = self
            .builder
            .build_int_compare(IntPredicate::ULT, len_a, len_b, "zip1.minsel")
            .unwrap();
        let n = self
            .builder
            .build_select(a_lt_b, len_a, len_b, "zip1.n")
            .unwrap()
            .into_int_value();

        let counter = self.create_entry_alloca(fn_val, "zip1.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_int(0, false))
            .unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "zip1.cond");
        let body_bb = self.context.append_basic_block(fn_val, "zip1.body");
        let incr_bb = self.context.append_basic_block(fn_val, "zip1.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "zip1.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "zip1.i.cur")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur, n, "zip1.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "zip1.i.body")
            .unwrap()
            .into_int_value();
        let a_ptr = unsafe {
            self.builder
                .build_gep(elem_ty_a, data_a, &[cur], "zip1.a.ptr")
                .unwrap()
        };
        let a_val = self.builder.build_load(elem_ty_a, a_ptr, "zip1.a").unwrap();
        let b_ptr = unsafe {
            self.builder
                .build_gep(elem_ty_b, data_b, &[cur], "zip1.b.ptr")
                .unwrap()
        };
        let b_val = self.builder.build_load(elem_ty_b, b_ptr, "zip1.b").unwrap();
        let mut tup = tup_ty.get_undef();
        tup = self
            .builder
            .build_insert_value(tup, a_val, 0, "zip1.tup.a")
            .unwrap()
            .into_struct_value();
        tup = self
            .builder
            .build_insert_value(tup, b_val, 1, "zip1.tup.b")
            .unwrap()
            .into_struct_value();
        self.bind_pattern(pattern, tup.into())?;
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "zip1.i.incr")
            .unwrap()
            .into_int_value();
        let one = i64_t.const_int(1, false);
        let next = self.builder.build_int_add(cur, one, "zip1.next").unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Compile `for <pattern> in <s>` and `for <pattern> in <s>.chars()` for
    /// a String variable `<s>`. Loads the `{ptr, len}` from the variable's
    /// String struct alloca and delegates to `compile_for_string_chars_inner`
    /// which emits the actual per-Unicode-scalar-value loop.
    pub(super) fn compile_for_string_chars(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        var_name: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let str_ptr = self.get_data_ptr(var_name).unwrap();
        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, str_ptr, 1, "for.s.len.ptr")
            .unwrap();
        let data_ptr_ptr = self
            .builder
            .build_struct_gep(vec_ty, str_ptr, 0, "for.s.data.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "for.s.len")
            .unwrap()
            .into_int_value();
        let data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "for.s.data")
            .unwrap()
            .into_pointer_value();
        self.compile_for_string_chars_inner(label, pattern, data, len, body)
    }

    /// Inner per-char loop driver — takes already-extracted `data` and `len`
    /// from any String value (variable alloca, string literal, interpolated
    /// string, function return). Iterates per Unicode scalar value via the
    /// `karac_string_decode_char` runtime helper. The codepoint is bound as
    /// `i32` (LLVM type for `char`).
    ///
    /// Shape:
    /// - `byte_offset` alloca, initialised to 0.
    /// - `out_codepoint` alloca (i32), populated each iteration by the helper.
    /// - cond block: `byte_offset < len`.
    /// - body block: call `karac_string_decode_char(data, len, byte_offset,
    ///   &out_codepoint)`; bind the pattern to the loaded `i32` codepoint;
    ///   run the user body; store the returned byte offset back.
    /// - incr block: branch back to cond.
    pub(super) fn compile_for_string_chars_inner(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();

        let byte_offset = self.create_entry_alloca(fn_val, "for.s.offset", i64_t.into());
        self.builder
            .build_store(byte_offset, i64_t.const_int(0, false))
            .unwrap();
        let out_codepoint = self.create_entry_alloca(fn_val, "for.s.cp", i32_t.into());

        let cond_bb = self.context.append_basic_block(fn_val, "for.s.cond");
        let body_bb = self.context.append_basic_block(fn_val, "for.s.body");
        let incr_bb = self.context.append_basic_block(fn_val, "for.s.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "for.s.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        // Condition: byte_offset < len. (Empty string: len == 0, falls
        // straight through to exit.)
        self.builder.position_at_end(cond_bb);
        let cur_off = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), byte_offset, "for.s.off")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, cur_off, len, "for.s.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        // Body: decode the next char, bind, execute. The decode helper
        // returns the post-char byte offset; stash it for the incr block
        // via the alloca write below.
        self.builder.position_at_end(body_bb);
        let cur_off = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), byte_offset, "for.s.off")
            .unwrap()
            .into_int_value();
        // ASCII fast-path: peek the byte at `cur_off`; a byte < 0x80 is a
        // complete 1-byte UTF-8 scalar, so set the codepoint to the byte and
        // advance the offset by 1 INLINE — skipping the per-char
        // `karac_string_decode_char` CALL. This is the symmetric read-side
        // counterpart of the `push(char)` ASCII fast-path (B-2026-06-18-6): on a
        // pure-ASCII workload the decode was the top non-allocation cost in the
        // `for c in s.chars()` loop (kata:38 profile). Multibyte (>= 0x80) takes
        // the slow runtime call, which decodes the scalar and returns the new
        // offset exactly as before. `cur_off < len` is guaranteed by the loop
        // condition, so the byte peek is in bounds.
        let byte_ptr = unsafe {
            self.builder
                .build_gep(self.context.i8_type(), data, &[cur_off], "for.s.byte.ptr")
                .unwrap()
        };
        let byte = self
            .builder
            .build_load(self.context.i8_type(), byte_ptr, "for.s.byte")
            .unwrap()
            .into_int_value();
        let byte_i32 = self
            .builder
            .build_int_z_extend(byte, i32_t, "for.s.byte.z")
            .unwrap();
        let is_ascii = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                byte_i32,
                i32_t.const_int(0x80, false),
                "for.s.ascii",
            )
            .unwrap();
        let ascii_bb = self.context.append_basic_block(fn_val, "for.s.ascii");
        let slow_bb = self.context.append_basic_block(fn_val, "for.s.slow");
        let cont_bb = self.context.append_basic_block(fn_val, "for.s.cont");
        self.builder
            .build_conditional_branch(is_ascii, ascii_bb, slow_bb)
            .unwrap();

        // ASCII: codepoint = byte; new offset = cur_off + 1.
        self.builder.position_at_end(ascii_bb);
        self.builder.build_store(out_codepoint, byte_i32).unwrap();
        let ascii_off = self
            .builder
            .build_int_add(cur_off, i64_t.const_int(1, false), "for.s.ascii.off")
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Multibyte: the runtime decoder writes the scalar to `out_codepoint`
        // and returns the post-char byte offset.
        self.builder.position_at_end(slow_bb);
        let slow_off = self
            .builder
            .build_call(
                self.karac_string_decode_char_fn,
                &[
                    data.into(),
                    len.into(),
                    cur_off.into(),
                    out_codepoint.into(),
                ],
                "for.s.decode",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Merge: `new_off` is the chosen path's offset; the codepoint is in
        // `out_codepoint` either way.
        self.builder.position_at_end(cont_bb);
        let new_off_phi = self.builder.build_phi(i64_t, "for.s.new_off").unwrap();
        new_off_phi.add_incoming(&[(&ascii_off, ascii_bb), (&slow_off, slow_bb)]);
        let new_off = new_off_phi.as_basic_value().into_int_value();
        let cp_val = self
            .builder
            .build_load(i32_t, out_codepoint, "for.s.cp.load")
            .unwrap();
        self.bind_pattern(pattern, cp_val)?;
        // Tag the loop binding's source type as `char` so the print and
        // f-string arms render the value as a glyph rather than the
        // integer codepoint. `bind_pattern` doesn't populate
        // `var_type_names` by itself (it only owns the LLVM-side slot
        // registration), and the typechecker doesn't write a binding
        // entry for the loop variable through the codegen-visible
        // `pattern_binding_types` table either, so the tag has to come
        // from the call site that knows the source-level type.
        if let PatternKind::Binding(bind_name) = &pattern.kind {
            self.var_type_names
                .insert(bind_name.clone(), "char".to_string());
        }
        self.scope_cleanup_actions.push(Vec::new());
        self.compile_block(body)?;
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // Per-iteration cleanup of body-local owned heap values
            // (B-2026-06-14-21), then stash new_off in the offset alloca
            // so the incr block picks it up. Written here at body-tail
            // rather than at the call site so a mid-body `break` doesn't
            // corrupt the offset (the break path skips this store and
            // exits via exit_bb).
            self.drain_top_frame_with_emit();
            self.builder.build_store(byte_offset, new_off).unwrap();
            self.builder.build_unconditional_branch(incr_bb).unwrap();
        } else {
            self.scope_cleanup_actions.pop();
        }

        // Increment: no-op — body already wrote the new offset. Kept as
        // a separate block so `continue` (which branches to incr_bb)
        // routes through one stable label.
        self.builder.position_at_end(incr_bb);
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Inner per-byte loop driver — the `.bytes()` sibling of
    /// [`compile_for_string_chars_inner`]. Takes already-extracted `data`
    /// and `len` from a String value and iterates the raw bytes, binding
    /// each as a `u8` (LLVM `i8`) — no UTF-8 decode.
    ///
    /// Shape:
    /// - `idx` alloca (i64), initialised to 0.
    /// - cond block: `idx < len` (empty string falls straight to exit).
    /// - body block: load `data[idx]` as `i8`, bind the pattern, run the
    ///   user body, then `idx += 1`.
    /// - incr block: branch back to cond.
    pub(super) fn compile_for_string_bytes_inner(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();

        let idx = self.create_entry_alloca(fn_val, "for.b.idx", i64_t.into());
        self.builder
            .build_store(idx, i64_t.const_int(0, false))
            .unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "for.b.cond");
        let body_bb = self.context.append_basic_block(fn_val, "for.b.body");
        let incr_bb = self.context.append_basic_block(fn_val, "for.b.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "for.b.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        // Condition: idx < len.
        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), idx, "for.b.i")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, cur, len, "for.b.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        // Body: load data[idx] as i8, bind, execute.
        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), idx, "for.b.i")
            .unwrap()
            .into_int_value();
        let byte_ptr = unsafe {
            self.builder
                .build_gep(i8_t, data, &[cur], "for.b.ptr")
                .unwrap()
        };
        let byte_val = self
            .builder
            .build_load(i8_t, byte_ptr, "for.b.byte")
            .unwrap();
        self.bind_pattern(pattern, byte_val)?;
        // Tag the binding as `u8` so downstream rendering / dispatch treats
        // it as an integer byte (not a `char` glyph like the chars loop).
        if let PatternKind::Binding(bind_name) = &pattern.kind {
            self.var_type_names
                .insert(bind_name.clone(), "u8".to_string());
        }
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        // Increment: idx += 1, branch back to cond.
        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), idx, "for.b.i")
            .unwrap()
            .into_int_value();
        let next = self
            .builder
            .build_int_add(cur, i64_t.const_int(1, false), "for.b.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Compile `for <pattern> in <map_var> { body }`.
    ///
    /// Uses the `karac_map_iter_*` runtime functions:
    /// - `karac_map_iter_new` creates the iterator before the loop.
    /// - `karac_map_iter_next` drives the loop; returns `false` when exhausted.
    /// - `karac_map_iter_free` runs unconditionally in the exit block so it fires
    ///   on both normal exit and `break`.
    ///
    /// The `(K, V)` pair delivered to `bind_pattern` is a two-field struct so
    /// tuple patterns like `for (k, v) in m` work via the existing struct-extract
    /// path in `bind_pattern`.
    pub(super) fn compile_for_map_var(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        var_name: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // No `self.variables` precheck: `get_data_ptr` below gates
        // existence and resolves module-binding globals too.
        // Use `get_data_ptr` so `for (k, v) in mut_ref_map` unwraps one
        // ref-level before the handle load. Owned bindings yield
        // `slot.ptr` directly.
        let handle_ptr = self
            .get_data_ptr(var_name)
            .ok_or_else(|| format!("unknown map variable '{var_name}'"))?;
        let map_handle = self
            .builder
            .build_load(ptr_ty, handle_ptr, "map.handle")
            .unwrap()
            .into_pointer_value();

        let key_ty = self
            .map_key_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());
        let val_ty = self
            .map_val_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());

        // `SortedMap[K, V]`: iterate `(k, v)` in ASCENDING key order. Walk the
        // sorted-key buffer, look each value up by key (`karac_map_get`), and
        // bind `{k, v}` with the same borrow-like semantics as the hash path
        // (aliases into the map's data; the freed header buffer owns nothing).
        if self.sorted_collection_vars.contains(var_name) {
            let key_te = self
                .map_key_type_exprs
                .get(var_name)
                .cloned()
                .ok_or_else(|| format!("SortedMap iteration: unknown key type for '{var_name}'"))?;
            let (kbuf, len) = self.emit_sorted_keys_buf(map_handle, &key_te)?;
            let out_val = self.create_entry_alloca(fn_val, "smf.outv", val_ty);
            let idx_slot = self.create_entry_alloca(fn_val, "smf.i", i64_t.into());
            self.builder
                .build_store(idx_slot, i64_t.const_zero())
                .unwrap();

            let cond_bb = self.context.append_basic_block(fn_val, "smf.cond");
            let body_bb = self.context.append_basic_block(fn_val, "smf.body");
            let cont_bb = self.context.append_basic_block(fn_val, "smf.cont");
            let exit_bb = self.context.append_basic_block(fn_val, "smf.exit");
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.loop_stack.push(LoopFrame {
                label: label.map(str::to_string),
                continue_bb: cont_bb,
                break_bb: exit_bb,
                result_slot: None,
                cleanup_depth: self.scope_cleanup_actions.len(),
            });

            self.builder.position_at_end(cond_bb);
            let i = self
                .builder
                .build_load(i64_t, idx_slot, "smf.i.v")
                .unwrap()
                .into_int_value();
            let more = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, i, len, "smf.more")
                .unwrap();
            self.builder
                .build_conditional_branch(more, body_bb, exit_bb)
                .unwrap();

            self.builder.position_at_end(body_bb);
            let kptr = unsafe {
                self.builder
                    .build_gep(key_ty, kbuf, &[i], "smf.kptr")
                    .unwrap()
            };
            let key_val = self.builder.build_load(key_ty, kptr, "smf.k").unwrap();
            self.builder
                .build_call(
                    self.karac_map_get_fn,
                    &[map_handle.into(), kptr.into(), out_val.into()],
                    "smf.get",
                )
                .unwrap();
            let val_val = self.builder.build_load(val_ty, out_val, "smf.v").unwrap();
            let kv_ty = self.context.struct_type(&[key_ty, val_ty], false);
            let mut kv = kv_ty.get_undef();
            kv = self
                .builder
                .build_insert_value(kv, key_val, 0, "smf.kv.k")
                .unwrap()
                .into_struct_value();
            kv = self
                .builder
                .build_insert_value(kv, val_val, 1, "smf.kv.v")
                .unwrap()
                .into_struct_value();
            self.bind_pattern(pattern, kv.into())?;
            self.register_for_loop_bindings(pattern, var_name);
            self.compile_loop_body_with_cleanup(body, cont_bb)?;

            self.builder.position_at_end(cont_bb);
            let i2 = self
                .builder
                .build_load(i64_t, idx_slot, "smf.i2")
                .unwrap()
                .into_int_value();
            let inc = self
                .builder
                .build_int_add(i2, i64_t.const_int(1, false), "smf.inc")
                .unwrap();
            self.builder.build_store(idx_slot, inc).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.loop_stack.pop();
            self.builder.position_at_end(exit_bb);
            self.builder
                .build_call(self.free_fn, &[kbuf.into()], "")
                .unwrap();
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        // Create the iterator (opaque ptr, lives for the duration of the loop).
        let iter_ptr = self
            .builder
            .build_call(self.karac_map_iter_new_fn, &[map_handle.into()], "map.iter")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Persistent allocas for out_key / out_val — overwritten each iteration.
        let out_key = self.create_entry_alloca(fn_val, "map.iter.key", key_ty);
        let out_val = self.create_entry_alloca(fn_val, "map.iter.val", val_ty);

        let loop_bb = self.context.append_basic_block(fn_val, "map.for.loop");
        let body_bb = self.context.append_basic_block(fn_val, "map.for.body");
        let exit_bb = self.context.append_basic_block(fn_val, "map.for.exit");

        self.builder.build_unconditional_branch(loop_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: loop_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        // loop_bb: advance iterator; branch on result.
        self.builder.position_at_end(loop_bb);
        let has_next = self
            .builder
            .build_call(
                self.karac_map_iter_next_fn,
                &[iter_ptr.into(), out_key.into(), out_val.into()],
                "map.iter.next",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_conditional_branch(has_next, body_bb, exit_bb)
            .unwrap();

        // body_bb: load key/val, build {K,V} struct, bind pattern, compile body.
        self.builder.position_at_end(body_bb);
        let key_val = self.builder.build_load(key_ty, out_key, "map.k").unwrap();
        let val_val = self.builder.build_load(val_ty, out_val, "map.v").unwrap();
        let kv_ty = self.context.struct_type(&[key_ty, val_ty], false);
        let mut kv = kv_ty.get_undef();
        kv = self
            .builder
            .build_insert_value(kv, key_val, 0, "kv.k")
            .unwrap()
            .into_struct_value();
        kv = self
            .builder
            .build_insert_value(kv, val_val, 1, "kv.v")
            .unwrap()
            .into_struct_value();
        self.bind_pattern(pattern, kv.into())?;
        self.register_for_loop_bindings(pattern, var_name);
        self.compile_loop_body_with_cleanup(body, loop_bb)?;

        self.loop_stack.pop();

        // exit_bb: free iterator — runs on both normal exhaustion and break.
        self.builder.position_at_end(exit_bb);
        self.builder
            .build_call(self.karac_map_iter_free_fn, &[iter_ptr.into()], "")
            .unwrap();

        Ok(i64_t.const_int(0, false).into())
    }

    /// Compile `for x in s { ... }` for a `Set[T]` variable. Mirror of
    /// `compile_for_map_var` — Set lowers to `Map[T, ()]` so the runtime
    /// iterator is the same; the value out-slot is sized 0 (a single
    /// shared `i8` alloca) and discarded since Set iteration produces only
    /// the element. The element pattern is bound directly (no `(k, v)`
    /// destructuring like Map's tuple-shaped iteration delivery).
    pub(super) fn compile_for_set_var(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        var_name: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // No `self.variables` precheck: `get_data_ptr` below gates
        // existence and resolves module-binding globals too.
        // Use `get_data_ptr` so `for x in mut_ref_set` unwraps one
        // ref-level before the handle load. Owned bindings yield
        // `slot.ptr` directly.
        let handle_ptr = self
            .get_data_ptr(var_name)
            .ok_or_else(|| format!("unknown set variable '{var_name}'"))?;
        let set_handle = self
            .builder
            .build_load(ptr_ty, handle_ptr, "set.handle")
            .unwrap()
            .into_pointer_value();

        let elem_ty = self
            .set_elem_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());

        // `SortedSet[T]`: iterate keys in ASCENDING order. Materialize the
        // sorted-key buffer once and walk it by index, then free it — instead
        // of the hash-order `karac_map_iter`. The element binding is identical
        // to the hash path (a borrow-like alias into the set's key data, which
        // the freed header buffer does not own), so `Set[String]` semantics
        // carry over unchanged.
        if self.sorted_collection_vars.contains(var_name) {
            let elem_te = self
                .set_elem_type_exprs
                .get(var_name)
                .cloned()
                .ok_or_else(|| {
                    format!("SortedSet iteration: unknown element type for '{var_name}'")
                })?;
            let (buf, len) = self.emit_sorted_keys_buf(set_handle, &elem_te)?;
            let idx_slot = self.create_entry_alloca(fn_val, "sset.for.i", i64_t.into());
            self.builder
                .build_store(idx_slot, i64_t.const_zero())
                .unwrap();

            let cond_bb = self.context.append_basic_block(fn_val, "sset.for.cond");
            let body_bb = self.context.append_basic_block(fn_val, "sset.for.body");
            let cont_bb = self.context.append_basic_block(fn_val, "sset.for.cont");
            let exit_bb = self.context.append_basic_block(fn_val, "sset.for.exit");
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.loop_stack.push(LoopFrame {
                label: label.map(str::to_string),
                continue_bb: cont_bb,
                break_bb: exit_bb,
                result_slot: None,
                cleanup_depth: self.scope_cleanup_actions.len(),
            });

            self.builder.position_at_end(cond_bb);
            let i = self
                .builder
                .build_load(i64_t, idx_slot, "sset.for.i.v")
                .unwrap()
                .into_int_value();
            let in_range = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, i, len, "sset.for.more")
                .unwrap();
            self.builder
                .build_conditional_branch(in_range, body_bb, exit_bb)
                .unwrap();

            self.builder.position_at_end(body_bb);
            let eptr = unsafe {
                self.builder
                    .build_gep(elem_ty, buf, &[i], "sset.for.eptr")
                    .unwrap()
            };
            let elem_val = self
                .builder
                .build_load(elem_ty, eptr, "sset.for.elem")
                .unwrap();
            self.bind_pattern(pattern, elem_val)?;
            if let PatternKind::Binding(elem_name) = &pattern.kind {
                if let Some(elem_te2) = self.set_elem_type_exprs.get(var_name).cloned() {
                    self.register_var_from_type_expr(elem_name, &elem_te2);
                }
            }
            self.compile_loop_body_with_cleanup(body, cont_bb)?;

            self.builder.position_at_end(cont_bb);
            let i2 = self
                .builder
                .build_load(i64_t, idx_slot, "sset.for.i2")
                .unwrap()
                .into_int_value();
            let inc = self
                .builder
                .build_int_add(i2, i64_t.const_int(1, false), "sset.for.inc")
                .unwrap();
            self.builder.build_store(idx_slot, inc).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.loop_stack.pop();

            self.builder.position_at_end(exit_bb);
            // `free(NULL)` is a no-op, so freeing an empty-set (len == 0) null
            // buffer is safe.
            self.builder
                .build_call(self.free_fn, &[buf.into()], "")
                .unwrap();
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        let iter_ptr = self
            .builder
            .build_call(self.karac_map_iter_new_fn, &[set_handle.into()], "set.iter")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let out_elem = self.create_entry_alloca(fn_val, "set.iter.elem", elem_ty);
        // val_size = 0 in the runtime; the val out-slot is overwritten
        // with zero bytes per iteration so a single `i8` is sufficient.
        let dummy_val = self.create_entry_alloca(fn_val, "set.iter.dummy", i8_t.into());

        let loop_bb = self.context.append_basic_block(fn_val, "set.for.loop");
        let body_bb = self.context.append_basic_block(fn_val, "set.for.body");
        let exit_bb = self.context.append_basic_block(fn_val, "set.for.exit");

        self.builder.build_unconditional_branch(loop_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: loop_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        self.builder.position_at_end(loop_bb);
        let has_next = self
            .builder
            .build_call(
                self.karac_map_iter_next_fn,
                &[iter_ptr.into(), out_elem.into(), dummy_val.into()],
                "set.iter.next",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_conditional_branch(has_next, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let elem_val = self
            .builder
            .build_load(elem_ty, out_elem, "set.elem")
            .unwrap();
        self.bind_pattern(pattern, elem_val)?;
        // Re-derive collection side-tables for the bound element so
        // `for x in s.union(t) { x.len() }` etc. dispatch correctly when
        // the element type itself is a Vec/Slice/Map (currently a no-op
        // for scalar Set elements; cheap insurance for the future).
        if let PatternKind::Binding(elem_name) = &pattern.kind {
            if let Some(elem_te) = self.set_elem_type_exprs.get(var_name).cloned() {
                self.register_var_from_type_expr(elem_name, &elem_te);
            }
        }
        self.compile_loop_body_with_cleanup(body, loop_bb)?;

        self.loop_stack.pop();

        self.builder.position_at_end(exit_bb);
        self.builder
            .build_call(self.karac_map_iter_free_fn, &[iter_ptr.into()], "")
            .unwrap();

        Ok(i64_t.const_int(0, false).into())
    }

    pub(super) fn compile_for_array_var(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        arr_ptr: PointerValue<'ctx>,
        arr_ty: inkwell::types::ArrayType<'ctx>,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let len = arr_ty.len() as u64;
        let elem_ty = arr_ty.get_element_type();

        let counter = self.create_entry_alloca(fn_val, "for.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_int(0, false))
            .unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "for.cond");
        let body_bb = self.context.append_basic_block(fn_val, "for.body");
        let incr_bb = self.context.append_basic_block(fn_val, "for.incr");
        let exit_bb = self.context.append_basic_block(fn_val, "for.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: incr_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        // Condition: i < N
        self.builder.position_at_end(cond_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let end_val = i64_t.const_int(len, false);
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur, end_val, "for.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        // Body: load arr[i], bind to pattern, compile block
        self.builder.position_at_end(body_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let zero = i64_t.const_int(0, false);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(
                    BasicTypeEnum::ArrayType(arr_ty),
                    arr_ptr,
                    &[zero, cur],
                    "for.elem.ptr",
                )
                .unwrap()
        };
        let elem_val = self
            .builder
            .build_load(elem_ty, elem_ptr, "for.elem")
            .unwrap();
        self.bind_pattern(pattern, elem_val)?;
        self.bind_enumerate_index(cur)?;
        self.compile_loop_body_with_cleanup(body, incr_bb)?;

        // Increment
        self.builder.position_at_end(incr_bb);
        let cur = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), counter, "i")
            .unwrap()
            .into_int_value();
        let one = i64_t.const_int(1, false);
        let next = self.builder.build_int_add(cur, one, "incr").unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_for_array_values(
        &mut self,
        pattern: &Pattern,
        elems: &[BasicValueEnum<'ctx>],
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        for &elem in elems {
            if self
                .builder
                .get_insert_block()
                .unwrap()
                .get_terminator()
                .is_some()
            {
                break;
            }
            self.bind_pattern(pattern, elem)?;
            // Per-iteration cleanup of body-local owned heap values
            // (B-2026-06-14-21). Unrolled straight-line bodies fall
            // through to the next element, so drain-emit in place with no
            // branch; a body terminator pops without emitting (the
            // early-exit cleanup walk already drained it).
            self.scope_cleanup_actions.push(Vec::new());
            self.compile_block(body)?;
            if self
                .builder
                .get_insert_block()
                .unwrap()
                .get_terminator()
                .is_none()
            {
                self.drain_top_frame_with_emit();
            } else {
                self.scope_cleanup_actions.pop();
            }
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }
}

// ── flat_map for-loop desugar (B-2026-07-14-8, flat_map leg) ──────────

/// Rewrite plan for loop-control statements inside a desugared loop body
/// (flat_map / cycle nested desugars). Two independent rules:
///
/// - `retarget_unlabeled_break`: an UNLABELED `break` that would bind the
///   loop this block is the body of is rewritten to `break <label>` — used
///   so a user break inside the synthesized INNER loop exits the whole
///   structure. Applies only OUTSIDE nested loop bodies (an unlabeled break
///   inside a nested loop binds that loop).
/// - `rename_labeled_continue`: `continue <from>` → `continue <to>` — used
///   when the user LABELED the flat_map/cycle loop: the label lands on the
///   synthesized OUTER loop (so `break <label>` exits everything), but a
///   labeled continue means "next flat element", which is the INNER loop's
///   continue — so it renames to the inner loop's synthesized label. Labels
///   are unique in scope (the resolver rejects duplicates), so this applies
///   EVERYWHERE in the body, including inside nested user loops.
///
/// Closures and comptime blocks are opaque (loop control cannot cross
/// them); defer/errdefer bodies are skipped (a break/continue there is
/// illegal upstream). The matches are deliberately EXHAUSTIVE (no `_` arm)
/// so a future ExprKind/StmtKind variant fails the build here instead of
/// silently escaping the walk.
struct LoopCtlRewrite {
    retarget_unlabeled_break: Option<String>,
    rename_labeled_continue: Option<(String, String)>,
}

impl LoopCtlRewrite {
    fn is_noop(&self) -> bool {
        self.retarget_unlabeled_break.is_none() && self.rename_labeled_continue.is_none()
    }
    /// The cfg that applies inside a NESTED loop body: unlabeled breaks bind
    /// the nested loop (rule off); labeled-continue renaming still applies.
    fn inside_nested_loop(&self) -> LoopCtlRewrite {
        LoopCtlRewrite {
            retarget_unlabeled_break: None,
            rename_labeled_continue: self.rename_labeled_continue.clone(),
        }
    }
}

fn rewrite_loop_ctl_block(block: &mut Block, cfg: &LoopCtlRewrite) {
    if cfg.is_noop() {
        return;
    }
    for stmt in &mut block.stmts {
        rewrite_loop_ctl_stmt(stmt, cfg);
    }
    if let Some(fe) = &mut block.final_expr {
        rewrite_loop_ctl_expr(fe, cfg);
    }
}

fn rewrite_loop_ctl_stmt(stmt: &mut Stmt, cfg: &LoopCtlRewrite) {
    match &mut stmt.kind {
        StmtKind::Let { value, .. } => rewrite_loop_ctl_expr(value, cfg),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            rewrite_loop_ctl_expr(value, cfg);
            rewrite_loop_ctl_block(else_block, cfg);
        }
        StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } => {}
        StmtKind::Assign { target, value, .. } => {
            rewrite_loop_ctl_expr(target, cfg);
            rewrite_loop_ctl_expr(value, cfg);
        }
        StmtKind::MultiAssign { targets, values } => {
            for e in targets.iter_mut().chain(values.iter_mut()) {
                rewrite_loop_ctl_expr(e, cfg);
            }
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            rewrite_loop_ctl_expr(target, cfg);
            rewrite_loop_ctl_expr(value, cfg);
        }
        StmtKind::Expr(e) => rewrite_loop_ctl_expr(e, cfg),
    }
}

fn rewrite_loop_ctl_expr(e: &mut Expr, cfg: &LoopCtlRewrite) {
    let walk = rewrite_loop_ctl_expr;
    match &mut e.kind {
        // ── the point of the walk ──
        ExprKind::Break { label, value } => {
            if label.is_none() {
                if let Some(t) = &cfg.retarget_unlabeled_break {
                    *label = Some(t.clone());
                }
            }
            if let Some(v) = value {
                walk(v, cfg);
            }
        }
        ExprKind::Continue { label, .. } => {
            if let (Some(l), Some((from, to))) = (&label, &cfg.rename_labeled_continue) {
                if l == from {
                    *label = Some(to.clone());
                }
            }
        }
        // ── loop boundaries: headers evaluate in the outer context; bodies
        //    own their unlabeled breaks but labeled-continue renaming still
        //    reaches inside ──
        ExprKind::For { iterable, body, .. } => {
            walk(iterable, cfg);
            rewrite_loop_ctl_block(body, &cfg.inside_nested_loop());
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk(condition, cfg);
            rewrite_loop_ctl_block(body, &cfg.inside_nested_loop());
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk(value, cfg);
            rewrite_loop_ctl_block(body, &cfg.inside_nested_loop());
        }
        ExprKind::Loop { body, .. } => {
            rewrite_loop_ctl_block(body, &cfg.inside_nested_loop());
        }
        // ── opaque boundaries ──
        ExprKind::Closure { .. } | ExprKind::Comptime(_) => {}
        // ── leaves ──
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}
        // ── plain descents ──
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(inner, _) = p {
                    walk(inner, cfg);
                }
            }
        }
        ExprKind::Binary { left, right, .. } => {
            walk(left, cfg);
            walk(right, cfg);
        }
        ExprKind::Unary { operand, .. } => walk(operand, cfg),
        ExprKind::Question(inner) => walk(inner, cfg),
        ExprKind::OptionalChain { object, args, .. } => {
            walk(object, cfg);
            if let Some(args) = args {
                for a in args {
                    walk(&mut a.value, cfg);
                }
            }
        }
        ExprKind::NilCoalesce { left, right } => {
            walk(left, cfg);
            walk(right, cfg);
        }
        ExprKind::Call { callee, args } => {
            walk(callee, cfg);
            for a in args {
                walk(&mut a.value, cfg);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk(object, cfg);
            for a in args {
                walk(&mut a.value, cfg);
            }
        }
        ExprKind::FieldAccess { object, .. } => walk(object, cfg),
        ExprKind::TupleIndex { object, .. } => walk(object, cfg),
        ExprKind::Index { object, index } => {
            walk(object, cfg);
            walk(index, cfg);
        }
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => rewrite_loop_ctl_block(b, cfg),
        ExprKind::LabeledBlock { body, .. } => rewrite_loop_ctl_block(body, cfg),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk(condition, cfg);
            rewrite_loop_ctl_block(then_block, cfg);
            if let Some(eb) = else_branch {
                walk(eb, cfg);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk(value, cfg);
            rewrite_loop_ctl_block(then_block, cfg);
            if let Some(eb) = else_branch {
                walk(eb, cfg);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk(scrutinee, cfg);
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    walk(g, cfg);
                }
                walk(&mut arm.body, cfg);
            }
        }
        ExprKind::Return(v) => {
            if let Some(v) = v {
                walk(v, cfg);
            }
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for it in items {
                walk(it, cfg);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for it in items {
                walk(it, cfg);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk(value, cfg);
            walk(count, cfg);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                walk(k, cfg);
                walk(v, cfg);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk(&mut f.value, cfg);
            }
            if let Some(s) = spread {
                walk(s, cfg);
            }
        }
        ExprKind::Pipe { left, right } => {
            walk(left, cfg);
            walk(right, cfg);
        }
        ExprKind::Cast { expr, .. } => walk(expr, cfg),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk(s, cfg);
            }
            if let Some(en) = end {
                walk(en, cfg);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            walk(mutex, cfg);
            rewrite_loop_ctl_block(body, cfg);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                walk(&mut b.value, cfg);
            }
            rewrite_loop_ctl_block(body, cfg);
        }
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `for <pat> in <recv>.flat_map(|p| <inner>) { <body> }` into a
    /// nested pair of loops (B-2026-07-14-8, flat_map leg):
    ///
    /// ```text
    /// __fml_N: for <p> in <recv> {
    ///     for <pat> in <inner> {
    ///         <body, unlabeled breaks retargeted to __fml_N>
    ///     }
    /// }
    /// ```
    ///
    /// The closure param IS the outer loop var, so `<inner>` (which references
    /// it) resolves; the user pattern binds each inner element — the flat
    /// sequence order is exactly outer-then-inner. Unlabeled `continue` in the
    /// body needs no rewrite (the next flat element IS the inner loop's next
    /// iteration); unlabeled `break` is retargeted onto the synthesized outer
    /// label so it exits the WHOLE flat sequence, not just the current batch
    /// (`retarget_unlabeled_breaks_block`).
    ///
    /// A USER label lands on the OUTER loop (`break <label>` exits the whole
    /// flat sequence) and `continue <label>` is renamed to the inner loop's
    /// synthesized label (next flat element). Fails closed (`Ok(None)` → the
    /// loud `.flat_map()` adaptor bail) for: a non-single-Binding
    /// closure param, and an inner iterable outside the proven-to-iterate
    /// whitelist (array/Vec literal, bounded range, named binding, or a chain
    /// the fused peel accepts) — an unproven inner shape could hit
    /// `compile_for`'s silent unknown-iterable arm and drop elements.
    /// True iff `e` is a `<recv>.flat_map(|p| <inner>)` call whose shape the
    /// nested-loop desugar (`try_compile_for_flat_map`) accepts: a single
    /// simple closure param, an inner iterable on the proven whitelist, and a
    /// receiver the fused peel understands. Used by the fused TERMINALS
    /// (fold/sum/count/reduce/for_each/any/all) to treat a peel-rejected
    /// flat_map receiver as a zero-step base — the synthesized
    /// `for <elem> in <e>` then routes through the flat_map desugar.
    pub(super) fn for_loop_iterates_flat_map(e: &Expr) -> bool {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &e.kind
        else {
            return false;
        };
        if method != "flat_map" || args.len() != 1 {
            return false;
        }
        let ExprKind::Closure { params, body, .. } = &args[0].value.kind else {
            return false;
        };
        if params.len() != 1
            || !matches!(
                &params[0].pattern.kind,
                PatternKind::Binding(_) | PatternKind::Wildcard
            )
        {
            return false;
        }
        Self::flat_map_inner_iterable_ok(body)
            && Self::peel_fused_map_filter_chain(object).is_some()
    }

    /// True iff `e` is a `<recv>.flatten()` call whose shape the nested-loop
    /// desugar (`try_compile_for_flatten`) accepts: a bare `flatten()` over a
    /// receiver on the proven inner-iterable whitelist. Used by the fused
    /// TERMINALS (fold/sum/count/reduce/for_each/any/all and the collect engine)
    /// via `peel_base_is_structural_adaptor` to treat a flatten receiver as a
    /// zero-step base — the synthesized `for <elem> in <e>` then routes through
    /// the flatten desugar (B-2026-07-19-12 slice 3).
    pub(super) fn for_loop_iterates_flatten(e: &Expr) -> bool {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &e.kind
        else {
            return false;
        };
        method == "flatten" && args.is_empty() && Self::flat_map_inner_iterable_ok(object)
    }

    /// Inner-iterable whitelist for the flat_map desugar: shapes `compile_for`
    /// provably iterates (verified against the JIT: array literal, `Vec[…]`
    /// prefix literal, bounded range, named binding incl. an outer Vec-of-Vec
    /// loop element, and fused-peel-accepted chains). The typechecker already
    /// restricts flat_map inners to `Iterator[U]`-typed exprs (the MethodCall
    /// arm); the literal/range/identifier arms defend against future
    /// typechecker widening. Anything else fails closed.
    fn flat_map_inner_iterable_ok(inner: &Expr) -> bool {
        match &inner.kind {
            ExprKind::ArrayLiteral(_) => true,
            ExprKind::PrefixCollectionLiteral { type_name, .. } => type_name == "Vec",
            ExprKind::Range {
                start: Some(_),
                end: Some(_),
                ..
            } => true,
            ExprKind::Identifier(_) => true,
            ExprKind::MethodCall { .. } => Self::peel_fused_map_filter_chain(inner).is_some(),
            _ => false,
        }
    }

    pub(super) fn try_compile_for_flat_map(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        recv: &Expr,
        closure_arg: &Expr,
        body: &Block,
        span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let ExprKind::Closure {
            params,
            body: inner,
            ..
        } = &closure_arg.kind
        else {
            return Ok(None);
        };
        if params.len() != 1 {
            return Ok(None);
        }
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let p = match &params[0].pattern.kind {
            PatternKind::Binding(n) => n.clone(),
            PatternKind::Wildcard => format!("__fmp_{uid}"),
            _ => return Ok(None),
        };
        if !Self::flat_map_inner_iterable_ok(inner) {
            return Ok(None);
        }

        // A USER label lands on the OUTER loop (so `break <label>` exits the
        // whole flat sequence), and `continue <label>` — which means "next
        // flat element", i.e. the INNER loop's continue — is renamed to the
        // inner loop's synthesized label (labels are unique in scope, so the
        // rename applies everywhere in the body).
        let outer_label = label
            .map(str::to_string)
            .unwrap_or_else(|| format!("__fml_{uid}"));
        let inner_label = format!("__fmi_{uid}");
        let mut user_body = body.clone();
        rewrite_loop_ctl_block(
            &mut user_body,
            &LoopCtlRewrite {
                retarget_unlabeled_break: Some(outer_label.clone()),
                rename_labeled_continue: label.map(|l| (l.to_string(), inner_label.clone())),
            },
        );

        let inner_for = Expr {
            kind: ExprKind::For {
                label: label.map(|_| inner_label),
                pattern: pattern.clone(),
                iterable: inner.clone(),
                attributes: Vec::new(),
                body: user_body,
            },
            span: span.clone(),
        };
        let outer_for = Expr {
            kind: ExprKind::For {
                label: Some(outer_label),
                pattern: Pattern {
                    kind: PatternKind::Binding(p),
                    span: span.clone(),
                },
                iterable: Box::new(recv.clone()),
                attributes: Vec::new(),
                body: Block {
                    stmts: vec![Stmt {
                        kind: StmtKind::Expr(inner_for),
                        span: span.clone(),
                    }],
                    final_expr: None,
                    span: span.clone(),
                },
            },
            span: span.clone(),
        };
        Ok(Some(self.compile_expr(&outer_for)?))
    }

    /// Lower `for <pat> in <recv>.flatten() { <body> }` (B-2026-07-19-12 slice 2)
    /// into a nested loop — flatten is `flat_map` with an identity inner, so the
    /// outer loop binds each inner iterable and the inner loop yields its
    /// elements:
    ///
    /// ```text
    /// for __flt in <recv> {          // recv: Iterator[Inner]
    ///     for <pat> in __flt {       // __flt: the inner iterable
    ///         <body>
    ///     }
    /// }
    /// ```
    ///
    /// Label / break / continue handling mirrors `try_compile_for_flat_map`: a
    /// user label lands on the OUTER loop (`break <label>` exits the whole flat
    /// sequence); a labeled `continue <label>` (next flat element) is renamed to
    /// the inner loop. Fails closed (`None`) — to the loud flatten defer — when
    /// the receiver isn't a shape `compile_for` provably iterates.
    pub(super) fn try_compile_for_flatten(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        recv: &Expr,
        body: &Block,
        span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // The outer loop iterates `recv` directly; reuse the flat_map
        // inner-iterable whitelist to gate the shapes we can prove.
        if !Self::flat_map_inner_iterable_ok(recv) {
            return Ok(None);
        }
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let inner_var = format!("__flt_{uid}");

        let outer_label = label
            .map(str::to_string)
            .unwrap_or_else(|| format!("__fll_{uid}"));
        let inner_label = format!("__fli_{uid}");
        let mut user_body = body.clone();
        rewrite_loop_ctl_block(
            &mut user_body,
            &LoopCtlRewrite {
                retarget_unlabeled_break: Some(outer_label.clone()),
                rename_labeled_continue: label.map(|l| (l.to_string(), inner_label.clone())),
            },
        );

        let inner_for = Expr {
            kind: ExprKind::For {
                label: label.map(|_| inner_label),
                pattern: pattern.clone(),
                iterable: Box::new(Expr {
                    kind: ExprKind::Identifier(inner_var.clone()),
                    span: span.clone(),
                }),
                attributes: Vec::new(),
                body: user_body,
            },
            span: span.clone(),
        };
        let outer_for = Expr {
            kind: ExprKind::For {
                label: Some(outer_label),
                pattern: Pattern {
                    kind: PatternKind::Binding(inner_var),
                    span: span.clone(),
                },
                iterable: Box::new(recv.clone()),
                attributes: Vec::new(),
                body: Block {
                    stmts: vec![Stmt {
                        kind: StmtKind::Expr(inner_for),
                        span: span.clone(),
                    }],
                    final_expr: None,
                    span: span.clone(),
                },
            },
            span: span.clone(),
        };
        Ok(Some(self.compile_expr(&outer_for)?))
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `for <pat> in <src>.cycle() { <body> }` (B-2026-07-14-8, cycle
    /// leg) into a restart loop around one full pass of the source chain:
    ///
    /// ```text
    /// __cyl_N: loop {
    ///     let mut __cyp_N = false;
    ///     for <pat> in <src> {
    ///         __cyp_N = true;
    ///         <body, unlabeled breaks retargeted to __cyl_N>
    ///     }
    ///     if !__cyp_N { break }
    /// }
    /// ```
    ///
    /// Semantics match the interpreter's `Cycle` source (iter_eval.rs): each
    /// restart re-runs the chain from scratch — the fused desugar re-emits
    /// its adaptor state prelude INSIDE the loop body, so per-pass counters
    /// (`take(2).cycle()`) reset exactly like the interpreter's fresh
    /// template clone — and a pass that yields NOTHING ends the loop (the
    /// interpreter's empty-template stop; without it an empty source would
    /// spin forever). The yielded flag is set first thing in the loop body,
    /// so filtered chains count only elements that actually reach the body.
    /// User `break` exits the WHOLE cycle via the retargeted label
    /// (`retarget_unlabeled_breaks_block`); user `continue` is the inner
    /// `for`'s continue — the next flat element, crossing the restart
    /// boundary naturally when the pass ends. The trailing empty-pass
    /// `break` is unlabeled and sits at loop-body level (outside the `for`),
    /// so it binds the `loop` correctly without a label.
    ///
    /// A USER label lands on the outer restart `loop` (`break <label>` exits
    /// the whole cycle) and `continue <label>` is renamed to the pass-`for`'s
    /// synthesized label (next flat element) — same scheme as flat_map.
    /// Fails closed (`Ok(None)` → the loud `.cycle()` adaptor bail) for a
    /// source chain the fused peel rejects.
    pub(super) fn try_compile_for_cycle(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        src: &Expr,
        body: &Block,
        span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if Self::peel_fused_map_filter_chain(src).is_none() {
            return Ok(None);
        }
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        // A USER label lands on the outer restart `loop` (so `break <label>`
        // exits the whole cycle); `continue <label>` means "next flat
        // element" — the pass-`for`'s continue — and is renamed to its
        // synthesized label (same scheme as flat_map).
        let cyl = label
            .map(str::to_string)
            .unwrap_or_else(|| format!("__cyl_{uid}"));
        let cyi = format!("__cyi_{uid}");
        let cyp = format!("__cyp_{uid}");
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: span.clone(),
        };

        let mut user_body = body.clone();
        rewrite_loop_ctl_block(
            &mut user_body,
            &LoopCtlRewrite {
                retarget_unlabeled_break: Some(cyl.clone()),
                rename_labeled_continue: label.map(|l| (l.to_string(), cyi.clone())),
            },
        );

        // `__cyp_N = true;` prepended to the user body.
        let mut inner_stmts = vec![Stmt {
            kind: StmtKind::Assign {
                target: ident(&cyp),
                value: Expr {
                    kind: ExprKind::Bool(true),
                    span: span.clone(),
                },
            },
            span: span.clone(),
        }];
        inner_stmts.extend(user_body.stmts);
        let inner_body = Block {
            stmts: inner_stmts,
            final_expr: user_body.final_expr,
            span: span.clone(),
        };

        let pass_for = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: label.map(|_| cyi),
                    pattern: pattern.clone(),
                    iterable: Box::new(src.clone()),
                    attributes: Vec::new(),
                    body: inner_body,
                },
                span: span.clone(),
            }),
            span: span.clone(),
        };
        // `if !__cyp_N { break }` — unlabeled: binds the enclosing loop.
        let empty_guard = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::If {
                    condition: Box::new(Expr {
                        kind: ExprKind::Unary {
                            op: UnaryOp::Not,
                            operand: Box::new(ident(&cyp)),
                        },
                        span: span.clone(),
                    }),
                    then_block: Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Expr(Expr {
                                kind: ExprKind::Break {
                                    label: None,
                                    value: None,
                                },
                                span: span.clone(),
                            }),
                            span: span.clone(),
                        }],
                        final_expr: None,
                        span: span.clone(),
                    },
                    else_branch: None,
                },
                span: span.clone(),
            }),
            span: span.clone(),
        };
        let flag_let = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(cyp.clone()),
                    span: span.clone(),
                },
                ty: None,
                value: Expr {
                    kind: ExprKind::Bool(false),
                    span: span.clone(),
                },
            },
            span: span.clone(),
        };
        let cycle_loop = Expr {
            kind: ExprKind::Loop {
                label: Some(cyl),
                body: Block {
                    stmts: vec![flag_let, pass_for, empty_guard],
                    final_expr: None,
                    span: span.clone(),
                },
                attributes: Vec::new(),
            },
            span: span.clone(),
        };
        Ok(Some(self.compile_expr(&cycle_loop)?))
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `for <pat> in <src>.scan(init, |acc, x| Some((new, out))) {
    /// <body> }` (B-2026-07-14-8, scan leg) into a single accumulator loop:
    ///
    /// ```text
    /// { let mut __sacc_N = <init>;
    ///   [label:] for <x> in <src> {
    ///       let <acc> = __sacc_N;
    ///       let __st_N = (<new>, <out>);
    ///       __sacc_N = __st_N.0;
    ///       let <pat> = __st_N.1;
    ///       <body>
    ///   } }
    /// ```
    ///
    /// This is the scan-collect desugar (`try_compile_scan_collect`) with the
    /// user's body as the sink instead of a `push`. Because the result is ONE
    /// loop, the user's `break`/`continue`/label semantics need no rewriting —
    /// the user label rides on the `for` directly. The accumulator advances
    /// BEFORE the user body runs, so a body `continue` does not desync state.
    ///
    /// Same body-shape restriction as the collect desugar: only a DIRECT
    /// `Some((new, out))` closure body (which never stops early — no break
    /// machinery needed). A conditional-`None` body (`scan`'s early-stop form)
    /// has no `.is_none()`/`.unwrap()` dispatch on synthetic AST and fails
    /// closed to the loud `.scan()` adaptor bail; the interpreter handles it.
    /// The source may be any fused-peel-accepted chain (broader than the
    /// collect desugar's identity-source gate — the inner `for` re-runs the
    /// full fusion).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_compile_for_scan(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        src: &Expr,
        init: &Expr,
        closure_arg: &Expr,
        body: &Block,
        span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let ExprKind::Closure {
            params,
            body: cbody,
            ..
        } = &closure_arg.kind
        else {
            return Ok(None);
        };
        if params.len() != 2 {
            return Ok(None);
        }
        let (PatternKind::Binding(acc_p), PatternKind::Binding(x_p)) =
            (&params[0].pattern.kind, &params[1].pattern.kind)
        else {
            return Ok(None);
        };
        if Self::peel_fused_map_filter_chain(src).is_none() {
            return Ok(None);
        }
        // Direct `Some(<tuple>)` body only (see doc comment).
        let callee_is_some = |callee: &Expr| -> bool {
            match &callee.kind {
                ExprKind::Identifier(n) => n == "Some",
                ExprKind::Path { segments, .. } => {
                    segments.last().map(|s| s.as_str()) == Some("Some")
                }
                _ => false,
            }
        };
        let inner_tuple = match &cbody.kind {
            ExprKind::Call { callee, args } if args.len() == 1 && callee_is_some(callee) => {
                args[0].value.clone()
            }
            _ => return Ok(None),
        };

        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let accname = format!("__sacc_{uid}");
        let tname = format!("__st_{uid}");
        let sp = span.clone();
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp.clone(),
        };
        let let_stmt = |is_mut: bool, name: &str, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp.clone(),
                },
                ty: None,
                value,
            },
            span: sp.clone(),
        };
        let tuple_idx = |recv: Expr, idx: u64| Expr {
            kind: ExprKind::TupleIndex {
                object: Box::new(recv),
                index: idx,
            },
            span: sp.clone(),
        };

        let mut for_body = vec![
            let_stmt(false, acc_p, ident(&accname)),
            let_stmt(false, &tname, inner_tuple),
            Stmt {
                kind: StmtKind::Assign {
                    target: ident(&accname),
                    value: tuple_idx(ident(&tname), 0),
                },
                span: sp.clone(),
            },
            Stmt {
                kind: StmtKind::Let {
                    is_mut: false,
                    pattern: pattern.clone(),
                    ty: None,
                    value: tuple_idx(ident(&tname), 1),
                },
                span: sp.clone(),
            },
        ];
        for_body.extend(body.stmts.iter().cloned());
        if let Some(fe) = &body.final_expr {
            for_body.push(Stmt {
                kind: StmtKind::Expr((**fe).clone()),
                span: sp.clone(),
            });
        }

        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: label.map(str::to_string),
                    pattern: Pattern {
                        kind: PatternKind::Binding(x_p.clone()),
                        span: sp.clone(),
                    },
                    iterable: Box::new(src.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp.clone(),
                    },
                },
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_stmt(true, &accname, init.clone()), for_loop],
                final_expr: None,
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        Ok(Some(self.compile_expr(&block)?))
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `for w in xs.iter().windows(k)` / `for w in xs.iter().chunks(k)`
    /// over a NAMED SCALAR-element Vec (B-2026-07-14-8, windows/chunks legs).
    /// Both adaptors yield a FRESHLY ALLOCATED `Vec[T]` per group (the
    /// typechecker types the element `Vec[T]`, the interpreter allocates per
    /// pull — method_call_seq.rs), so each iteration materializes the group
    /// with an index push-loop and binds it to the user pattern:
    ///
    /// ```text
    /// windows: { let __wn: i64 = <k>; let __wl: i64 = xs.len();
    ///            let __we: i64 = if __wn < 1 { 0 }
    ///                            else { if __wn > __wl { 0 } else { __wl - __wn + 1 } };
    ///            [label:] for __wi in 0..__we {
    ///                let mut __wv: Vec[T] = Vec.new();
    ///                for __wj in __wi..(__wi + __wn) { __wv.push(xs[__wj]); }
    ///                let <pat> = __wv;  <body>
    ///            } }
    /// chunks:  same shape with `for __wi in (0..__wl).step_by(__wn_clamped)`
    ///          and the group end clamped to `min(__wi + __wn, __wl)`.
    /// ```
    ///
    /// Clamp parity with the interpreter: `chunks` clamps k to ≥ 1
    /// (`n.max(1)`); `windows` with k ≤ 0 or k > len yields NOTHING (the
    /// zero end-bound). The user body's statements are direct children of
    /// the outer loop (the push-loop finishes before them), so user
    /// `break`/`continue`/labels bind naturally with no rewriting — the
    /// label rides on the outer `for`. Scalar elements only (the group push
    /// copies elements); heap shapes fail closed to the loud adaptor bail.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_compile_for_windows_chunks(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        src: &Expr,
        count: &Expr,
        is_windows: bool,
        body: &Block,
        span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if matches!(&count.kind, ExprKind::Closure { .. }) {
            return Ok(None);
        }
        let Some(var_name) = self.peel_iter_to_vec_ident(src) else {
            return Ok(None);
        };
        let Some(elem_te) = self.var_elem_type_exprs.get(var_name.as_str()).cloned() else {
            return Ok(None);
        };
        let vec_te = Self::vec_type_expr_from_element(&elem_te);

        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = span.clone();
        let n_name = format!("__wcn_{uid}");
        let len_name = format!("__wcl_{uid}");
        let end_name = format!("__wce_{uid}");
        let i_name = format!("__wci_{uid}");
        let j_name = format!("__wcj_{uid}");
        let v_name = format!("__wcv_{uid}");
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp.clone(),
        };
        let i64_lit = |n: i64| Expr {
            kind: ExprKind::Integer(n, Some(crate::token::IntSuffix::I64)),
            span: sp.clone(),
        };
        let i64_ty = || TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["i64".to_string()],
                generic_args: None,
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        let let_stmt = |is_mut: bool, name: &str, ty: Option<TypeExpr>, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp.clone(),
                },
                ty,
                value,
            },
            span: sp.clone(),
        };
        let bin = |op: BinOp, l: Expr, r: Expr| Expr {
            kind: ExprKind::Binary {
                op,
                left: Box::new(l),
                right: Box::new(r),
            },
            span: sp.clone(),
        };
        let if_expr = |cond: Expr, then_e: Expr, else_e: Expr| Expr {
            kind: ExprKind::If {
                condition: Box::new(cond),
                then_block: Block {
                    stmts: Vec::new(),
                    final_expr: Some(Box::new(then_e)),
                    span: sp.clone(),
                },
                else_branch: Some(Box::new(Expr {
                    kind: ExprKind::Block(Block {
                        stmts: Vec::new(),
                        final_expr: Some(Box::new(else_e)),
                        span: sp.clone(),
                    }),
                    span: sp.clone(),
                })),
            },
            span: sp.clone(),
        };
        let range = |start: Expr, end: Expr| Expr {
            kind: ExprKind::Range {
                start: Some(Box::new(start)),
                end: Some(Box::new(end)),
                inclusive: false,
            },
            span: sp.clone(),
        };
        let len_call = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(ident(&var_name)),
                method: "len".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp.clone(),
            },
            span: sp.clone(),
        };
        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp.clone(),
                }),
                args: vec![],
            },
            span: sp.clone(),
        };
        let push_elem = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(ident(&v_name)),
                    method: "push".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        value: Expr {
                            kind: ExprKind::Index {
                                object: Box::new(ident(&var_name)),
                                index: Box::new(ident(&j_name)),
                            },
                            span: sp.clone(),
                        },
                        span: sp.clone(),
                    }],
                    args_close_span: sp.clone(),
                },
                span: sp.clone(),
            }),
            span: sp.clone(),
        };

        // Pre-loop bindings + the outer position iterable + the group end.
        let mut prelude: Vec<Stmt> = Vec::new();
        let (outer_iterable, group_end): (Expr, Expr) = if is_windows {
            // k clamped to >= 1 (the interpreter's ITERATOR-variant windows
            // clamps `n.max(1)` at the dispatch site — windows(0) behaves as
            // windows(1); method_call_seq.rs), then the window-count
            // end-bound: 0 when k > len (no window fits), else len - k + 1.
            let raw = format!("__wcr_{uid}");
            prelude.push(let_stmt(false, &raw, Some(i64_ty()), count.clone()));
            prelude.push(let_stmt(
                false,
                &n_name,
                Some(i64_ty()),
                if_expr(
                    bin(BinOp::Lt, ident(&raw), i64_lit(1)),
                    i64_lit(1),
                    ident(&raw),
                ),
            ));
            prelude.push(let_stmt(false, &len_name, Some(i64_ty()), len_call));
            let end_val = if_expr(
                bin(BinOp::Gt, ident(&n_name), ident(&len_name)),
                i64_lit(0),
                bin(
                    BinOp::Add,
                    bin(BinOp::Sub, ident(&len_name), ident(&n_name)),
                    i64_lit(1),
                ),
            );
            prelude.push(let_stmt(false, &end_name, Some(i64_ty()), end_val));
            (
                range(i64_lit(0), ident(&end_name)),
                bin(BinOp::Add, ident(&i_name), ident(&n_name)),
            )
        } else {
            // Clamped k (>= 1), len; positions stride by k; group end clamped
            // to len (the final chunk may be short).
            let raw = format!("__wcr_{uid}");
            prelude.push(let_stmt(false, &raw, Some(i64_ty()), count.clone()));
            prelude.push(let_stmt(
                false,
                &n_name,
                Some(i64_ty()),
                if_expr(
                    bin(BinOp::Lt, ident(&raw), i64_lit(1)),
                    i64_lit(1),
                    ident(&raw),
                ),
            ));
            prelude.push(let_stmt(false, &len_name, Some(i64_ty()), len_call));
            let stride_positions = Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(range(i64_lit(0), ident(&len_name))),
                    method: "step_by".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        value: ident(&n_name),
                        span: sp.clone(),
                    }],
                    args_close_span: sp.clone(),
                },
                span: sp.clone(),
            };
            let raw_end = bin(BinOp::Add, ident(&i_name), ident(&n_name));
            (
                stride_positions,
                if_expr(
                    bin(BinOp::Gt, raw_end.clone(), ident(&len_name)),
                    ident(&len_name),
                    raw_end,
                ),
            )
        };

        // Outer loop body: materialize the group, bind, run the user body.
        let mut outer_body = vec![
            let_stmt(false, &end_name.replace("__wce", "__wcge"), None, group_end),
            let_stmt(true, &v_name, Some(vec_te.clone()), vec_new),
            Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::For {
                        label: None,
                        pattern: Pattern {
                            kind: PatternKind::Binding(j_name.clone()),
                            span: sp.clone(),
                        },
                        iterable: Box::new(range(
                            ident(&i_name),
                            ident(&end_name.replace("__wce", "__wcge")),
                        )),
                        attributes: Vec::new(),
                        body: Block {
                            stmts: vec![push_elem],
                            final_expr: None,
                            span: sp.clone(),
                        },
                    },
                    span: sp.clone(),
                }),
                span: sp.clone(),
            },
            Stmt {
                kind: StmtKind::Let {
                    is_mut: false,
                    pattern: pattern.clone(),
                    // Explicit Vec[T] annotation: when this desugar runs UNDER
                    // the fused-chain desugar (adaptors after windows/chunks),
                    // the binding sits at a synthesized span with no
                    // typechecker record — without the annotation the group
                    // binding never registers as a Vec and `w[0]` / `w.len()`
                    // in the downstream stages fail to dispatch.
                    ty: Some(vec_te.clone()),
                    value: ident(&v_name),
                },
                span: sp.clone(),
            },
        ];
        outer_body.extend(body.stmts.iter().cloned());
        if let Some(fe) = &body.final_expr {
            outer_body.push(Stmt {
                kind: StmtKind::Expr((**fe).clone()),
                span: sp.clone(),
            });
        }

        let outer_for = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: label.map(str::to_string),
                    pattern: Pattern {
                        kind: PatternKind::Binding(i_name.clone()),
                        span: sp.clone(),
                    },
                    iterable: Box::new(outer_iterable),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: outer_body,
                        final_expr: None,
                        span: sp.clone(),
                    },
                },
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        let mut block_stmts = prelude;
        block_stmts.push(outer_for);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: None,
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        Ok(Some(self.compile_expr(&block)?))
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `for g in xs.iter().chunk_by(|x| <key>) { <body> }` over a NAMED
    /// SCALAR-element Vec (B-2026-07-14-8, chunk_by leg). Two phases:
    ///
    /// ```text
    /// { let __cbl: i64 = xs.len();
    ///   let mut __cbst: Vec[i64] = Vec.new();          // group START indices
    ///   let mut __cbi: i64 = 0;
    ///   while __cbi < __cbl {
    ///       if __cbi == 0 { __cbst.push(0); }
    ///       else if !({let x = xs[__cbi - 1]; key} == {let x = xs[__cbi]; key})
    ///            { __cbst.push(__cbi); }
    ///       __cbi = __cbi + 1;
    ///   }
    ///   __cbst.push(__cbl);                            // sentinel end
    ///   [label:] for __cbg in 0..(__cbst.len() - 1) {
    ///       <group Vec build from __cbst[__cbg] .. __cbst[__cbg + 1]>
    ///       let <pat>: Vec[T] = __cbv;  <body>
    ///   } }
    /// ```
    ///
    /// The boundary walk is fully synthesized (no user code inside the
    /// `while`), so the user body sits in an ordinary `for` — break /
    /// continue / labels bind naturally. Groups are fresh `Vec[T]`s like the
    /// interpreter's `ChunkBy` source. Key equality uses `==` on the key
    /// values (String keys compare by content like the interpreter's
    /// `Value::PartialEq`). Known, documented divergence: the key closure
    /// re-evaluates at both sides of each boundary (≤ 2× per element) where
    /// the interpreter caches one key per element — observable only with a
    /// side-effecting key closure. Scalar elements only; other shapes fail
    /// closed to the loud `.chunk_by()` bail.
    pub(super) fn try_compile_for_chunk_by(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        src: &Expr,
        key_closure: &Expr,
        body: &Block,
        span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let ExprKind::Closure {
            params,
            body: key_body,
            ..
        } = &key_closure.kind
        else {
            return Ok(None);
        };
        if params.len() != 1 {
            return Ok(None);
        }
        let PatternKind::Binding(key_p) = &params[0].pattern.kind else {
            return Ok(None);
        };
        let Some(var_name) = self.peel_iter_to_vec_ident(src) else {
            return Ok(None);
        };
        let Some(elem_te) = self.var_elem_type_exprs.get(var_name.as_str()).cloned() else {
            return Ok(None);
        };
        // Key-shape gate. An IDENTITY key (`|x| x`) compares the elements
        // directly (`xs[i-1] == xs[i]`) with NO synthesized binding block —
        // safe for any element type. A COMPUTED key must bind the element
        // into the synthesized `{ let x = xs[i]; <key> }` block, and for a
        // HEAP element that binding is an unregistered clone the block never
        // frees (leaks one buffer per boundary compare — found by valgrind on
        // the heap-element leg) — so computed keys require SCALAR elements;
        // heap-element computed-key chunk_by fails closed to the loud bail.
        let identity_key = matches!(&key_body.kind, ExprKind::Identifier(n) if n == key_p);
        if !identity_key && !super::vec_method::is_trivially_copyable_te(&elem_te) {
            return Ok(None);
        }
        let vec_te = Self::vec_type_expr_from_element(&elem_te);

        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = span.clone();
        let len_n = format!("__cbl_{uid}");
        let starts_n = format!("__cbst_{uid}");
        let i_n = format!("__cbi_{uid}");
        let g_n = format!("__cbg_{uid}");
        let j_n = format!("__cbj_{uid}");
        let s_n = format!("__cbs_{uid}");
        let e_n = format!("__cbe_{uid}");
        let v_n = format!("__cbv_{uid}");
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp.clone(),
        };
        let i64_lit = |n: i64| Expr {
            kind: ExprKind::Integer(n, Some(crate::token::IntSuffix::I64)),
            span: sp.clone(),
        };
        let i64_ty = || TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["i64".to_string()],
                generic_args: None,
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        let vec_i64_ty = || TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(i64_ty())]),
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        let let_stmt = |is_mut: bool, name: &str, ty: Option<TypeExpr>, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp.clone(),
                },
                ty,
                value,
            },
            span: sp.clone(),
        };
        let bin = |op: BinOp, l: Expr, r: Expr| Expr {
            kind: ExprKind::Binary {
                op,
                left: Box::new(l),
                right: Box::new(r),
            },
            span: sp.clone(),
        };
        let index = |obj: Expr, idx: Expr| Expr {
            kind: ExprKind::Index {
                object: Box::new(obj),
                index: Box::new(idx),
            },
            span: sp.clone(),
        };
        let call1 = |obj: Expr, method: &str, arg: Option<Expr>| Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(obj),
                method: method.to_string(),
                turbofish: None,
                args: arg
                    .map(|a| {
                        vec![CallArg {
                            label: None,
                            mut_marker: false,
                            value: a,
                            span: sp.clone(),
                        }]
                    })
                    .unwrap_or_default(),
                args_close_span: sp.clone(),
            },
            span: sp.clone(),
        };
        // The key of one element: for an identity key, the element itself
        // (`xs[<idx>]` — compared in place, no binding); for a computed key,
        // `{ let <key_p> = xs[<idx>]; <key_body> }` (scalar elements only,
        // per the gate above).
        let key_of = |idx: Expr| -> Expr {
            if identity_key {
                return index(ident(&var_name), idx);
            }
            Expr {
                kind: ExprKind::Block(Block {
                    stmts: vec![let_stmt(false, key_p, None, index(ident(&var_name), idx))],
                    final_expr: Some(Box::new((**key_body).clone())),
                    span: sp.clone(),
                }),
                span: sp.clone(),
            }
        };
        let expr_stmt = |e: Expr| Stmt {
            kind: StmtKind::Expr(e),
            span: sp.clone(),
        };
        let if_stmt = |cond: Expr, then_stmts: Vec<Stmt>, else_stmts: Option<Vec<Stmt>>| {
            expr_stmt(Expr {
                kind: ExprKind::If {
                    condition: Box::new(cond),
                    then_block: Block {
                        stmts: then_stmts,
                        final_expr: None,
                        span: sp.clone(),
                    },
                    else_branch: else_stmts.map(|s| {
                        Box::new(Expr {
                            kind: ExprKind::Block(Block {
                                stmts: s,
                                final_expr: None,
                                span: sp.clone(),
                            }),
                            span: sp.clone(),
                        })
                    }),
                },
                span: sp.clone(),
            })
        };
        let assign = |name: &str, value: Expr| Stmt {
            kind: StmtKind::Assign {
                target: ident(name),
                value,
            },
            span: sp.clone(),
        };
        let push_to = |vec: &str, val: Expr| expr_stmt(call1(ident(vec), "push", Some(val)));

        // Phase 1: boundary walk.
        let boundary_check = if_stmt(
            bin(BinOp::Eq, ident(&i_n), i64_lit(0)),
            vec![push_to(&starts_n, i64_lit(0))],
            Some(vec![if_stmt(
                Expr {
                    kind: ExprKind::Unary {
                        op: UnaryOp::Not,
                        operand: Box::new(bin(
                            BinOp::Eq,
                            key_of(bin(BinOp::Sub, ident(&i_n), i64_lit(1))),
                            key_of(ident(&i_n)),
                        )),
                    },
                    span: sp.clone(),
                },
                vec![push_to(&starts_n, ident(&i_n))],
                None,
            )]),
        );
        let while_walk = expr_stmt(Expr {
            kind: ExprKind::While {
                label: None,
                condition: Box::new(bin(BinOp::Lt, ident(&i_n), ident(&len_n))),
                body: Block {
                    stmts: vec![
                        boundary_check,
                        assign(&i_n, bin(BinOp::Add, ident(&i_n), i64_lit(1))),
                    ],
                    final_expr: None,
                    span: sp.clone(),
                },
                attributes: Vec::new(),
            },
            span: sp.clone(),
        });

        // Phase 2: the group loop.
        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp.clone(),
                }),
                args: vec![],
            },
            span: sp.clone(),
        };
        let mut group_body = vec![
            let_stmt(false, &s_n, None, index(ident(&starts_n), ident(&g_n))),
            let_stmt(
                false,
                &e_n,
                None,
                index(ident(&starts_n), bin(BinOp::Add, ident(&g_n), i64_lit(1))),
            ),
            let_stmt(true, &v_n, Some(vec_te.clone()), vec_new),
            expr_stmt(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(j_n.clone()),
                        span: sp.clone(),
                    },
                    iterable: Box::new(Expr {
                        kind: ExprKind::Range {
                            start: Some(Box::new(ident(&s_n))),
                            end: Some(Box::new(ident(&e_n))),
                            inclusive: false,
                        },
                        span: sp.clone(),
                    }),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: vec![push_to(&v_n, index(ident(&var_name), ident(&j_n)))],
                        final_expr: None,
                        span: sp.clone(),
                    },
                },
                span: sp.clone(),
            }),
            Stmt {
                kind: StmtKind::Let {
                    is_mut: false,
                    pattern: pattern.clone(),
                    ty: Some(vec_te),
                    value: ident(&v_n),
                },
                span: sp.clone(),
            },
        ];
        group_body.extend(body.stmts.iter().cloned());
        if let Some(fe) = &body.final_expr {
            group_body.push(expr_stmt((**fe).clone()));
        }
        let group_for = expr_stmt(Expr {
            kind: ExprKind::For {
                label: label.map(str::to_string),
                pattern: Pattern {
                    kind: PatternKind::Binding(g_n.clone()),
                    span: sp.clone(),
                },
                iterable: Box::new(Expr {
                    kind: ExprKind::Range {
                        start: Some(Box::new(i64_lit(0))),
                        end: Some(Box::new(bin(
                            BinOp::Sub,
                            call1(ident(&starts_n), "len", None),
                            i64_lit(1),
                        ))),
                        inclusive: false,
                    },
                    span: sp.clone(),
                }),
                attributes: Vec::new(),
                body: Block {
                    stmts: group_body,
                    final_expr: None,
                    span: sp.clone(),
                },
            },
            span: sp.clone(),
        });

        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![
                    let_stmt(
                        false,
                        &len_n,
                        Some(i64_ty()),
                        call1(ident(&var_name), "len", None),
                    ),
                    let_stmt(true, &starts_n, Some(vec_i64_ty()), vec_new_expr(&sp)),
                    let_stmt(true, &i_n, Some(i64_ty()), i64_lit(0)),
                    while_walk,
                    push_to(&starts_n, ident(&len_n)),
                    group_for,
                ],
                final_expr: None,
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        Ok(Some(self.compile_expr(&block)?))
    }
}

/// A `Vec.new()` call expr — shared by the synthesized-group desugars.
fn vec_new_expr(sp: &crate::token::Span) -> Expr {
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Path {
                    segments: vec!["Vec".to_string(), "new".to_string()],
                    generic_args: None,
                },
                span: sp.clone(),
            }),
            args: vec![],
        },
        span: sp.clone(),
    }
}
