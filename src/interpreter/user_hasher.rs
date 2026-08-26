//! Running a USER-written hasher from the interpreter — the twin of
//! `Codegen::emit_hash_bytes_call`'s user arm (B-2026-08-22-6).
//!
//! design.md § `Hash` and `Hasher`, "User-extensible hashers": a container may
//! name any type that `impl BuildHasher for` in its trailing slot, and both
//! backends then dispatch through that type's own `build` / `write` / `finish`
//! instead of calling a runtime entry point. Codegen emits three direct calls
//! into the user's LLVM functions. This module is what the interpreter does
//! instead.
//!
//! # Two problems, and what solves each
//!
//! **Getting BYTES out of a `Value`.** `Hasher::write` takes a `Slice[u8]`, but
//! the interpreter's keys are `Value` trees, not memory images. [`encode_value`]
//! flattens one into a canonical byte string, walking exactly the shape
//! `value::hash_value_generic` walks and obeying the same contract: `a == b`
//! must produce identical bytes. Field maps are emitted in sorted-name order
//! because `Value::Struct`'s fields live in a `HashMap`, whose iteration order
//! is not stable — the generic hasher solves that by XOR-combining per-field
//! hashes (commutative), and a byte string cannot, so it sorts instead.
//!
//! **Getting an INTERPRETER where the hash happens.** `MapData::hash_key` is a
//! `&self` method reached through an `RwLock` guard, with no interpreter in
//! sight and no way to thread one in without changing the signature of every
//! map and set operation in the tree. So this module keeps its own: a leaked
//! `&'static Program` / `&'static TypeCheckResult` (one clone per process,
//! installed the first time an interpreter is built for a program that names a
//! user hasher) and a per-thread sub-`Interpreter` built from them.
//!
//! Leaking is what makes the sub-interpreter storable in a `thread_local` at
//! all — `Interpreter<'a>` borrows its program, and a thread-local demands
//! `'static`. It is bounded and one-shot: at most one `Program` +
//! `TypeCheckResult` clone for the life of the process, and only for a program
//! that actually uses the feature. The alternative shapes are worse: a raw
//! pointer to the live interpreter would hand out a second `&mut` to a value
//! already borrowed further up the stack, and rebuilding an interpreter per
//! key hash would put `Interpreter::new` + `register_items` on the map hot
//! path.
//!
//! # What a separate interpreter means
//!
//! The hasher runs against the same program with a fresh global environment, so
//! it sees every function, struct and impl the main run does. It does NOT see
//! the main run's mutable state. For a hasher that is what the `Eq` consistency
//! contract already demands — a hash that varied with unrelated program state
//! would break lookups under any implementation — so the isolation costs
//! nothing a correct hasher relies on, and it buys the guarantee that hashing a
//! key can never perturb the program being run.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::OnceLock;

use super::{EnumData, Interpreter, Value};
use crate::ast::{CallArg, Expr, ExprKind, Program};
use crate::token::Span;
use crate::typechecker::TypeCheckResult;

/// Bindings the synthesized `build` / `write` / `finish` calls read through.
/// The names are not valid Kāra identifiers, so they cannot collide with a
/// user binding even though they live in an ordinary pushed scope.
const BUILDER_VAR: &str = " hasher.builder";
const STATE_VAR: &str = " hasher.state";
const BYTES_VAR: &str = " hasher.bytes";

/// Span stamped on every synthesized node. Far past the end of any real source,
/// so a span-keyed side table (`expr_types`, `container_hashers`, the
/// method-dispatch maps) cannot be hit by accident — the lookups miss and the
/// interpreter falls back to the runtime value's own type, which is what a
/// concrete receiver wants anyway.
const SYNTH_SPAN: Span = Span {
    line: 0,
    column: 0,
    offset: usize::MAX / 2,
    length: 0,
};

/// The leaked program every hasher sub-interpreter runs against.
static WORLD: OnceLock<(&'static Program, &'static TypeCheckResult)> = OnceLock::new();

/// Latches the first time a user hasher call falls back, so the warning is
/// printed once rather than once per key.
static REPORTED_FALLBACK: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// This thread's hasher interpreter, built on first use.
    static SUB: RefCell<Option<Interpreter<'static>>> = const { RefCell::new(None) };
}

/// Install the leaked world, if `program` names a user hasher anywhere.
///
/// Called from `Interpreter::new`, which is the universal chokepoint every eval
/// path funnels through (including each par-branch interpreter on its own
/// thread) — the same reason `type_order::install` lives there. A program with
/// no user hasher pays one `values().any(…)` scan of a table that is empty for
/// almost every program, and leaks nothing.
pub(crate) fn install(program: &Program, typecheck_result: &TypeCheckResult) {
    if WORLD.get().is_some() {
        return;
    }
    // A user `impl Hash for K` needs the same world for the same reason a user
    // BUILDER does — `MapData::hash_key` runs deep inside a container with no
    // interpreter in sight — so either one arms it (B-2026-08-26-10).
    let hash_impls = user_hash_impl_targets(program);
    if !program
        .container_hashers
        .values()
        .any(|k| k.user_builder().is_some())
        && hash_impls.is_empty()
    {
        return;
    }
    let _ = HASH_IMPLS.set(hash_impls);
    let _ = WORLD.set((
        Box::leak(Box::new(program.clone())),
        Box::leak(Box::new(typecheck_result.clone())),
    ));
}

/// Type names carrying a user `impl Hash`, computed once at [`install`].
///
/// Recomputing this per key hash would put an AST walk on the map hot path;
/// asking the typechecker's tables instead would work equally well but this is
/// already the module that owns a leaked `Program`.
static HASH_IMPLS: OnceLock<std::collections::HashSet<String>> = OnceLock::new();

/// The sink the user's `hash` writes into — `runtime/stdlib/hash.kara`'s
/// `KeyByteSink`. Its `bytes` field is read back out after the call; its
/// `finish` is never invoked, because the digest is the CONTAINER's business.
const SINK_TYPE: &str = "KeyByteSink";
const SINK_VAR: &str = "__karac_hash_sink";
const KEY_VAR: &str = "__karac_hash_key";

fn user_hash_impl_targets(program: &Program) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for item in &program.items {
        if let crate::ast::Item::ImplBlock(imp) = item {
            let is_hash = imp
                .trait_name
                .as_ref()
                .and_then(|t| t.segments.last())
                .is_some_and(|n| n == "Hash");
            if !is_hash {
                continue;
            }
            // The sink's own `impl Hasher` is not a `Hash` impl, but a user
            // could legitimately write `impl Hash` for a type the sink is built
            // from; guarding on the trait name alone is enough.
            if let crate::ast::TypeKind::Path(p) = &imp.target_type.kind {
                if let Some(name) = p.segments.last() {
                    out.insert(name.clone());
                }
            }
        }
    }
    out
}

/// The bytes a user `impl Hash` writes for `v`, or `None` when `v`'s type has
/// no such impl and the caller should fall back to its own encoding.
///
/// This is the `Hash` half of the split design.md § `Hash` and `Hasher` draws:
/// the impl decides WHICH BYTES a key contributes, and the container's hasher —
/// unchanged, whichever it is — decides how they become a digest. So this
/// composes with a user `BuildHasher` rather than competing with it.
pub(crate) fn user_hash_bytes(v: &Value) -> Option<Vec<u8>> {
    let impls = HASH_IMPLS.get()?;
    let type_name = match v {
        Value::Struct { name, .. } => name.clone(),
        Value::EnumVariant { enum_name, .. } => enum_name.clone(),
        Value::SharedStruct(s) => s.name.clone(),
        _ => return None,
    };
    if !impls.contains(&type_name) {
        return None;
    }
    let &(program, tc) = WORLD.get()?;
    // Same re-entrancy shape as `hash_bytes`: a key whose `hash` body itself
    // touches a user-hashed container re-enters here while this thread's cached
    // interpreter is borrowed. A throwaway interpreter keeps the answer correct
    // on that pathological path rather than returning a different digest for
    // the same key, which would corrupt the outer container's index.
    let nested = SUB.with(|cell| match cell.try_borrow_mut() {
        Ok(mut slot) => {
            let interp = slot.get_or_insert_with(|| {
                let mut i = Interpreter::new(program, tc);
                i.register_items();
                i
            });
            Some(run_hash(interp, v))
        }
        Err(_) => None,
    });
    match nested {
        Some(r) => r,
        None => {
            let mut interp = Interpreter::new(program, tc);
            interp.register_items();
            run_hash(&mut interp, v)
        }
    }
}

/// `key.hash(sink)` through ordinary method dispatch, then read `sink.bytes`.
///
/// Dispatched rather than called directly for the reason [`run`] documents:
/// `mut ref self` is only observable through the write-back `eval_method_call`
/// performs on its receiver BINDING, so the sink has to be a binding.
fn run_hash(interp: &mut Interpreter<'static>, key: &Value) -> Option<Vec<u8>> {
    interp.env.push_scope();
    interp.env.define(KEY_VAR.to_string(), key.clone());
    interp.env.define(
        SINK_VAR.to_string(),
        Value::Struct {
            name: SINK_TYPE.to_string(),
            fields: HashMap::from([("bytes".to_string(), Value::array_of(Vec::new()))]),
        },
    );
    interp.eval_method_call(
        &ident(KEY_VAR),
        "hash",
        &[CallArg {
            label: None,
            mut_marker: false,
            mut_marker_span: None,
            value: ident(SINK_VAR),
            span: SYNTH_SPAN,
        }],
        &SYNTH_SPAN,
        &SYNTH_SPAN,
    );
    let sink = interp.env.get(SINK_VAR);
    interp.env.pop_scope();

    // A `hash` body that raised leaves this CACHED interpreter poisoned for the
    // next key, so the signal is drained here — same as `run`.
    let faulted = interp.pending_cf.take().is_some() || !interp.runtime_errors.is_empty();
    // Carry the first error's TEXT into the warning. A bare "a user `hash` body
    // raised" names the symptom and hides the cause, and this path has no other
    // way to surface one — the error belongs to a sub-interpreter with no user
    // program to attribute it to.
    let why = interp
        .runtime_errors
        .first()
        .map(|e| e.message.clone())
        .unwrap_or_else(|| "control flow escaped the body".to_string());
    interp.runtime_errors.clear();
    if faulted {
        report_fallback(&format!("a user `hash` body raised: {why}"));
        return None;
    }
    let Some(Value::Struct { fields, .. }) = sink else {
        return None;
    };
    let bytes = match fields.get("bytes") {
        Some(Value::Array(rc)) => rc
            .read()
            .ok()?
            .iter()
            .map(|b| match b {
                Value::Int(i) => *i as u8,
                _ => 0,
            })
            .collect(),
        _ => return None,
    };
    Some(bytes)
}

/// Hash `v` through the user hasher built by `builder`.
///
/// Falls back to `0` — a legal, useless hash that keeps every lookup CORRECT by
/// collapsing the index to one bucket, where `==` still decides — if the world
/// was never installed or the hasher returns a non-integer. Both are compiler
/// bugs rather than user errors: `install` runs from `Interpreter::new` before
/// any container exists, and `finish`'s `-> u64` return is typechecked. So the
/// fallback warns on stderr rather than degrading in silence — a map that
/// suddenly behaves like an association list is otherwise invisible.
pub(crate) fn hash_value(builder: &str, v: &Value) -> u64 {
    let mut bytes = Vec::new();
    encode_value(v, &mut bytes);
    hash_bytes(builder, &bytes)
}

/// `hash_bytes` for a caller that already has the bytes — the user-`impl Hash`
/// path, whose bytes come from the key's own impl rather than from
/// [`encode_value`].
pub(crate) fn hash_bytes_for(builder: &str, bytes: &[u8]) -> u64 {
    hash_bytes(builder, bytes)
}

fn hash_bytes(builder: &str, bytes: &[u8]) -> u64 {
    let Some(&(program, tc)) = WORLD.get() else {
        report_fallback("no hasher program was installed");
        return 0;
    };
    // A hasher that itself hashes into a user-hashed container re-enters here
    // while this thread's cached interpreter is borrowed. Building a throwaway
    // one for the nested call keeps the answer CORRECT (the cost lands only on
    // the pathological shape) instead of returning a different hash for the
    // same key, which would corrupt the outer container's index.
    let nested = SUB.with(|cell| match cell.try_borrow_mut() {
        Ok(mut slot) => {
            let interp = slot.get_or_insert_with(|| {
                let mut i = Interpreter::new(program, tc);
                i.register_items();
                i
            });
            Some(run(interp, builder, bytes))
        }
        Err(_) => None,
    });
    let result = match nested {
        Some(r) => r,
        None => {
            let mut interp = Interpreter::new(program, tc);
            interp.register_items();
            run(&mut interp, builder, bytes)
        }
    };
    match result {
        Some(h) => h,
        None => {
            report_fallback(&format!(
                "'{builder}' raised, or did not produce an integer digest"
            ));
            0
        }
    }
}

/// Warn once per process that hashing has degraded to a constant.
fn report_fallback(why: &str) {
    if !REPORTED_FALLBACK.swap(true, AtomicOrdering::Relaxed) {
        eprintln!(
            "warning: user hasher fell back to a constant digest ({why}); \
             lookups stay correct but every key lands in one bucket"
        );
    }
}

/// `builder.build()` → `state.write(bytes)` → `state.finish()`, through the
/// interpreter's ordinary method dispatch.
///
/// Going through `eval_method_call` rather than calling the impl functions
/// directly is what makes `mut ref self` work: `Value::Struct` is a by-value
/// carrier, so a mutating method is only observable through the write-back the
/// method-call path performs on its receiver binding. That is also why the
/// three values live in an env scope instead of being passed around as `Value`s
/// — the receiver has to BE a binding for the write-back to land somewhere.
fn run(interp: &mut Interpreter<'static>, builder: &str, bytes: &[u8]) -> Option<u64> {
    interp.env.push_scope();
    interp.env.define(
        BUILDER_VAR.to_string(),
        Value::Struct {
            name: builder.to_string(),
            fields: HashMap::new(),
        },
    );
    let state =
        interp.eval_method_call(&ident(BUILDER_VAR), "build", &[], &SYNTH_SPAN, &SYNTH_SPAN);
    interp.env.define(STATE_VAR.to_string(), state);
    interp.env.define(
        BYTES_VAR.to_string(),
        Value::array_of(bytes.iter().map(|b| Value::Int(i128::from(*b))).collect()),
    );
    interp.eval_method_call(
        &ident(STATE_VAR),
        "write",
        &[CallArg {
            label: None,
            mut_marker: false,
            mut_marker_span: None,
            value: ident(BYTES_VAR),
            span: SYNTH_SPAN,
        }],
        &SYNTH_SPAN,
        &SYNTH_SPAN,
    );
    let out = interp.eval_method_call(&ident(STATE_VAR), "finish", &[], &SYNTH_SPAN, &SYNTH_SPAN);
    interp.env.pop_scope();

    // A hasher that panics (an overflow in the mixing step is the likely one)
    // leaves `pending_cf` set and a row in `runtime_errors`. This interpreter is
    // CACHED and about to hash the next key, so the signal has to be drained
    // here or every later hash would run inside a poisoned one. Drained into a
    // `None`, which the caller turns into the fallback digest and one warning —
    // the raised error itself belongs to the sub-interpreter and has no user
    // program to be attributed to.
    let faulted = interp.pending_cf.take().is_some() || !interp.runtime_errors.is_empty();
    interp.runtime_errors.clear();
    if faulted {
        return None;
    }
    match out {
        Value::Int(i) => Some(i as u64),
        _ => None,
    }
}

fn ident(name: &str) -> Expr {
    Expr {
        kind: ExprKind::Identifier(name.to_string()),
        span: SYNTH_SPAN,
    }
}

// ── Value → bytes ─────────────────────────────────────────────────────

/// One tag byte per `Value` shape, so two differently-shaped values cannot
/// encode to the same string. Explicit rather than `mem::discriminant` because
/// the bytes reach user code, and a value derived from field ordering in this
/// enum would change meaning under an unrelated refactor.
mod tag {
    pub const INT: u8 = 1;
    pub const STRING: u8 = 2;
    pub const CHAR: u8 = 3;
    pub const BOOL: u8 = 4;
    pub const UNIT: u8 = 5;
    pub const CBYTES: u8 = 6;
    pub const TUPLE: u8 = 7;
    pub const SEQ: u8 = 8;
    pub const ENUM: u8 = 9;
    pub const STRUCT: u8 = 10;
    pub const OPAQUE: u8 = 11;
}

/// Flatten `v` into `out`, preserving `a == b ⟹ identical bytes`.
///
/// Every variable-length part is LENGTH-PREFIXED, which is what stops
/// `("ab", "c")` and `("a", "bc")` from colliding — the property a bare
/// concatenation would lose and a tree of sub-hashes gets for free.
///
/// The trailing `_` arm mirrors `hash_value_generic`'s: a handle-shaped value
/// (a channel, a file, a task) encodes to its tag alone. Deliberately coarse
/// and still correct — those are unreachable as keys, and if one ever became a
/// key it would land in a single bucket and be decided by `==`.
fn encode_value(v: &Value, out: &mut Vec<u8>) {
    fn push_len(out: &mut Vec<u8>, n: usize) {
        out.extend_from_slice(&(n as u64).to_le_bytes());
    }
    fn push_bytes(out: &mut Vec<u8>, b: &[u8]) {
        push_len(out, b.len());
        out.extend_from_slice(b);
    }
    // `Value::Struct` and `EnumData::Struct` hold fields in a `HashMap`, whose
    // walk order is not stable within a run, let alone across them. Sorting by
    // field name is what makes two equal structs encode identically.
    fn push_fields(fields: &HashMap<String, Value>, out: &mut Vec<u8>) {
        let mut names: Vec<&String> = fields.keys().collect();
        names.sort();
        push_len(out, names.len());
        for name in names {
            push_bytes(out, name.as_bytes());
            encode_value(&fields[name], out);
        }
    }
    fn push_items<'v>(items: impl ExactSizeIterator<Item = &'v Value>, out: &mut Vec<u8>) {
        push_len(out, items.len());
        for item in items {
            encode_value(item, out);
        }
    }

    match v {
        Value::Int(i) => {
            out.push(tag::INT);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::String(s) => {
            out.push(tag::STRING);
            push_bytes(out, s.as_bytes());
        }
        Value::Char(c) => {
            out.push(tag::CHAR);
            out.extend_from_slice(&(*c as u32).to_le_bytes());
        }
        Value::Bool(b) => {
            out.push(tag::BOOL);
            out.push(u8::from(*b));
        }
        Value::Unit => out.push(tag::UNIT),
        Value::CStr(bytes) | Value::CString(bytes) => {
            out.push(tag::CBYTES);
            push_bytes(out, bytes);
        }
        Value::Tuple(items) => {
            out.push(tag::TUPLE);
            push_items(items.iter(), out);
        }
        Value::Array(rc) => {
            out.push(tag::SEQ);
            push_items(rc.read().unwrap().iter(), out);
        }
        Value::Slice {
            storage,
            start,
            len,
            ..
        } => {
            out.push(tag::SEQ);
            let items = storage.read().unwrap();
            push_items(items[*start..*start + *len].iter(), out);
        }
        Value::EnumVariant {
            enum_name,
            variant,
            data,
        } => {
            out.push(tag::ENUM);
            push_bytes(out, enum_name.as_bytes());
            push_bytes(out, variant.as_bytes());
            match data {
                EnumData::Unit => push_len(out, 0),
                EnumData::Tuple(items) => push_items(items.iter(), out),
                EnumData::Struct(fields) => push_fields(fields, out),
            }
        }
        Value::Struct { name, fields } => {
            out.push(tag::STRUCT);
            push_bytes(out, name.as_bytes());
            push_fields(fields, out);
        }
        _ => out.push(tag::OPAQUE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, RwLock};

    fn enc(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        encode_value(v, &mut out);
        out
    }

    /// The one obligation the encoding carries: equal values, equal bytes.
    /// Field-map order is the case that can break it, and the only one a
    /// single run can observe non-deterministically, so build the same struct
    /// twice with the fields inserted in opposite orders.
    #[test]
    fn equal_structs_encode_identically_regardless_of_field_insertion_order() {
        let mut a = HashMap::new();
        a.insert("x".to_string(), Value::Int(1));
        a.insert("y".to_string(), Value::String("two".into()));
        let mut b = HashMap::new();
        b.insert("y".to_string(), Value::String("two".into()));
        b.insert("x".to_string(), Value::Int(1));
        let sa = Value::Struct {
            name: "P".into(),
            fields: a,
        };
        let sb = Value::Struct {
            name: "P".into(),
            fields: b,
        };
        assert_eq!(sa, sb);
        assert_eq!(enc(&sa), enc(&sb));
    }

    /// Length prefixes, not concatenation: the classic split-boundary
    /// collision must not happen.
    #[test]
    fn tuple_split_boundaries_do_not_collide() {
        let ab_c = Value::Tuple(vec![Value::String("ab".into()), Value::String("c".into())]);
        let a_bc = Value::Tuple(vec![Value::String("a".into()), Value::String("bc".into())]);
        assert_ne!(enc(&ab_c), enc(&a_bc));
    }

    /// Distinct shapes carrying the same payload must not encode alike.
    #[test]
    fn tags_separate_shapes() {
        assert_ne!(enc(&Value::Int(1)), enc(&Value::Bool(true)));
        assert_ne!(enc(&Value::Unit), enc(&Value::Int(0)));
        assert_ne!(
            enc(&Value::String("a".into())),
            enc(&Value::CString(Arc::new(b"a".to_vec())))
        );
    }

    /// A `Slice` view and the `Array` it spans hold equal contents, so they
    /// have to encode alike — `hash_value_generic` gives them the same
    /// treatment for the same reason.
    #[test]
    fn slice_encodes_like_the_array_it_views() {
        let storage = Arc::new(RwLock::new(vec![
            Value::Int(7),
            Value::Int(8),
            Value::Int(9),
        ]));
        let slice = Value::Slice {
            storage: Arc::clone(&storage),
            start: 1,
            len: 2,
            mutable: false,
        };
        let array = Value::array_of(vec![Value::Int(8), Value::Int(9)]);
        assert_eq!(enc(&slice), enc(&array));
    }
}
