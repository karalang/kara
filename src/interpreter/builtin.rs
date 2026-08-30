//! Built-in function evaluation: `panic`/`unreachable`/`todo` (diverge),
//! `print`/`println`/`eprintln`, `dbg!`, and the three assert flavors.
//!
//! Houses `eval_builtin_diverge` (effect: `panics`, sets `ExitUnwind`),
//! `eval_builtin_print` (formats + routes through the
//! `Stdout.print` / `Stderr.println` provider arms), `write_stdout` /
//! `write_stderr` (the BuiltinDefault arms that honor the test
//! harness's captured-output buffer), `eval_builtin_dbg` (formatted
//! source-location-aware debug print), and `eval_builtin_assert*`
//! (the three assert flavors with structured failure-trace records).
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::type_display;

use super::value::{upgrade_weak_to_option, EnumData, Value};
use super::{dbg_json_escape, ConsoleSeg, ConsoleStream, DbgOutputMode};

impl<'a> super::Interpreter<'a> {
    // ── Built-in functions ───────────────────────────────────────

    pub(crate) fn eval_builtin_diverge(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        self.track_effect("panics");
        let msg = if let Some(arg) = args.first() {
            match self.eval_expr_inner(&arg.value) {
                Value::String(s) => s,
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let default_msg = match name {
            "todo" => "not yet implemented",
            "panic" => "explicit panic",
            _ => "entered unreachable code",
        };
        let full_msg = if msg.is_empty() {
            default_msg.to_string()
        } else if name == "panic" {
            // `panic("msg")` surfaces the user message verbatim (mirrors
            // codegen's `compile_diverge`); todo/unreachable annotate instead.
            msg
        } else {
            format!("{}: {}", default_msg, msg)
        };
        self.record_runtime_error(full_msg, span)
    }

    /// User-facing Display rendering. Differs from `Value`'s context-free
    /// `std::fmt::Display` only in that **struct fields render in declaration
    /// order** — the `Value::Struct` payload is a `HashMap` that has lost
    /// source order, so its bare `Display` iterates in (random) hash order.
    /// Declaration order is recovered from `typecheck_result.struct_info`.
    /// Recurses through the container shapes so a struct nested inside a
    /// `Vec` / tuple / map / slice is ordered too; every other value
    /// (scalars, String, enums, …) delegates to the unchanged `Display`.
    /// Routed through the user-facing surfaces — `print`/`println`,
    /// `.to_string()`, and f-string interpolation — while `Display` itself
    /// stays for debug / diagnostic contexts. Codegen renders structs in the
    /// same declaration order (see `synth_display.rs`), so the two backends
    /// agree.
    /// Render a value the way the user-facing surfaces do — `print`/`println`,
    /// `.to_string()`, and f-string interpolation — with the value's STATIC
    /// TYPE threaded alongside it, peeled one layer per level of the recursion.
    ///
    /// B-2026-08-19-27. Rendering an integer needs its type, because the
    /// interpreter holds an unsigned value as its two's-complement bit pattern
    /// in a signed carrier — a `u64` at or above 2^63, or a `u128` above
    /// `i128::MAX`, is a NEGATIVE `Value::Int`. The scalar `to_string` arm has
    /// always known this and consults the receiver's span. This renderer never
    /// did: it walks `Value`s structurally, so every nested integer was
    /// formatted signed and `println(o)` on an `Option[u64]` holding `u64::MAX`
    /// printed `Some(-1)` while both compiled backends printed the value. The
    /// divergence covered every container and payload shape — `Vec`, tuple,
    /// `Map`, struct field, enum payload, and any nesting of them.
    ///
    /// `ty` is `None` when the caller has no static type to offer (a `Value`
    /// reached from a context with no span, and the recursive positions whose
    /// parent type was itself unknown). That degrades to exactly the previous
    /// behaviour — signed — so an unresolved type is never worse than before.
    pub(crate) fn display_render_typed(
        &mut self,
        v: &Value,
        ty: Option<&crate::typechecker::Type>,
    ) -> String {
        self.render_typed_mode(v, ty, false)
    }

    /// The `Debug` twin of `display_render_typed`, and the renderer `dbg()`
    /// reports through (design.md § `dbg()` — "expression text, and value").
    ///
    /// B-2026-08-23-18. `Value::debug_fmt` walks a struct's `HashMap` of
    /// fields directly, so it emitted a DIFFERENT field order on every run of
    /// the same binary — 6 distinct orderings observed in 12 runs of a
    /// three-field struct. That made `dbg()`'s own output nondeterministic and
    /// left the compiled backends with no oracle to match, since codegen
    /// renders struct fields in DECLARATION order (`emit_struct_debug_display_fn`
    /// walks `struct_field_names`). Routing `dbg()` through this renderer buys
    /// declaration order, `Secret` redaction, and the unsigned-integer width
    /// handling — all of which the compiled backends already do — so the two
    /// backends agree by construction rather than by convention.
    ///
    /// `Debug` differs from `Display` at exactly two leaves: `String` and
    /// `Char` are quoted (`"hi"`, `'c'`) via Rust's own `{:?}`, which is what
    /// `Value::debug_fmt` calls. Every compound shape renders identically in
    /// the two modes, which is why one walker serves both.
    pub(crate) fn debug_render_typed(
        &mut self,
        v: &Value,
        ty: Option<&crate::typechecker::Type>,
    ) -> String {
        self.render_typed_mode(v, ty, true)
    }

    /// Shared body of `display_render_typed` / `debug_render_typed`. `debug`
    /// selects the quoted-leaf rendering; the structural arms are common.
    fn render_typed_mode(
        &mut self,
        v: &Value,
        ty: Option<&crate::typechecker::Type>,
        debug: bool,
    ) -> String {
        use crate::typechecker::Type;
        // A user `impl Display` wins at EVERY depth, not just at the top level
        // (B-2026-08-26-29). The depth-0 dispatch in `eval_method_call` already
        // reaches the user body, so `f"{e}"` printed `aye 7` — but the
        // recursive positions below rendered the DERIVED shape, so the same
        // value one level down inside a `Vec` / `Option` / struct field printed
        // `A { n: 7 }`. That made the one mechanism the language offers for
        // overriding a rendering stop applying inside a container, and leaked
        // the internals of any type whose `Display` exists to hide them.
        //
        // The typechecker already demands the element be `Display` to
        // interpolate a container at all (`Vec[P]` for a non-`Display` `P` is
        // rejected outright), so honoring the impl here is what makes the
        // renderer agree with the trait bound the gate enforces — not a new
        // policy. Never in `debug` mode: `Debug` is a different trait and keeps
        // the field-name shape, which is what `dbg()` reports and what
        // design.md calls the `{:?}` form.
        if !debug {
            if let Some(key) = self.user_display_impl_to_string_key(v) {
                if let Value::String(s) = self.call_function(&key, std::slice::from_ref(v)) {
                    return s;
                }
            }
        }
        match v {
            Value::Struct { name, fields } => {
                // std.secret: never render a `Secret[T]`'s wrapped value in a
                // built-in / derived Debug/Display. Redacting the whole value
                // here (rather than only at containing-struct field sites)
                // covers every render path uniformly — as a field, an array /
                // map element, or a direct `println(secret)` — and matches
                // codegen's field-level `<redacted>` on the tested surface
                // (a struct with a `Secret` field). Scoped to the stdlib type
                // via `defining_stdlib_origin` so a user's own `struct Secret`
                // renders normally.
                if name == "Secret"
                    && self
                        .typecheck_result
                        .struct_info
                        .get("Secret")
                        .is_some_and(|si| si.defining_stdlib_origin)
                {
                    return "<redacted>".to_string();
                }
                let order: Vec<String> = self
                    .typecheck_result
                    .struct_info
                    .get(name)
                    .map(|si| si.fields.iter().map(|(n, _, _)| n.clone()).collect())
                    .unwrap_or_else(|| fields.keys().cloned().collect());
                let field_tys = self.struct_field_display_types(name, ty);
                let name = name.clone();
                let mut parts: Vec<String> = Vec::new();
                for fname in &order {
                    let Some(fv) = fields.get(fname).cloned() else {
                        continue;
                    };
                    let fty = field_tys.get(fname).cloned();
                    let rendered = self.render_typed_mode(&fv, fty.as_ref(), debug);
                    parts.push(format!("{}: {}", fname, rendered));
                }
                format!("{} {{ {} }}", name, parts.join(", "))
            }
            Value::Tuple(vals) => {
                let elem_tys: Option<&Vec<Type>> = match ty {
                    Some(Type::Tuple(ts)) => Some(ts),
                    _ => None,
                };
                let elem_tys = elem_tys.cloned();
                let vals = vals.clone();
                let mut parts: Vec<String> = Vec::new();
                for (i, x) in vals.iter().enumerate() {
                    let t = elem_tys.as_ref().and_then(|ts| ts.get(i)).cloned();
                    parts.push(self.render_typed_mode(x, t.as_ref(), debug));
                }
                format!("({})", parts.join(", "))
            }
            Value::Array(rc) => {
                let elem = Self::display_element_type(ty).cloned();
                // Copy the elements OUT of the lock before rendering. A user
                // `impl Display` runs arbitrary Kāra code, which may read (or
                // write) the very container being printed — holding the read
                // guard across that call would deadlock on the write.
                let vals: Vec<Value> = rc.read().unwrap().clone();
                let mut parts: Vec<String> = Vec::new();
                for x in &vals {
                    parts.push(self.render_typed_mode(x, elem.as_ref(), debug));
                }
                format!("[{}]", parts.join(", "))
            }
            // `Vector[T, N]` renders lane-by-lane through this same typed
            // recursion, for one reason: a lane is a `Value::Int` whose
            // carrier is signed, so the untyped `Display` arm in `value.rs`
            // prints `u64::MAX` as `-1`. The element type is exactly what the
            // `Value::Int` arm below needs to read it back as unsigned, and
            // `display_element_type` already peels `Type::Vector` — so the
            // whole fix is being ON this path rather than falling through to
            // the catch-all (B-2026-08-30-9).
            //
            // Both compiled backends already print the unsigned value, so this
            // is the interpreter catching up to them, not a new convention.
            // Recursing (rather than formatting the lane inline) is what makes
            // a `Vector` nested in a `Vec` / tuple / `Option` come out right
            // too: those arms recurse into this one.
            Value::Vector(lanes) => {
                let elem = Self::display_element_type(ty).cloned();
                let lanes = lanes.clone();
                let mut parts: Vec<String> = Vec::new();
                for x in &lanes {
                    parts.push(self.render_typed_mode(x, elem.as_ref(), debug));
                }
                format!("Vector({})", parts.join(", "))
            }
            Value::Slice {
                storage,
                start,
                len,
                ..
            } => {
                let elem = Self::display_element_type(ty).cloned();
                // Same lock-release rationale as the `Array` arm above.
                let vals: Vec<Value> = storage.read().unwrap()[*start..*start + *len].to_vec();
                let mut parts: Vec<String> = Vec::new();
                for x in &vals {
                    parts.push(self.render_typed_mode(x, elem.as_ref(), debug));
                }
                format!("[{}]", parts.join(", "))
            }
            Value::Map(entries) => {
                // Copy the entries OUT of the lock first — see the `Array`
                // arm: a user `impl Display` on a key or value runs Kāra code
                // that may touch this same map.
                let entries: Vec<(Value, Value)> =
                    entries.read().unwrap().iter_observable().cloned().collect();
                // `Map[K, V]` / `SortedMap[K, V]` — both K and V can be an
                // unsigned scalar, so both sides peel.
                let (kt, vt) = match ty {
                    Some(Type::Named { name, args })
                        if (name == "Map" || name == "SortedMap") && args.len() == 2 =>
                    {
                        (Some(&args[0]), Some(&args[1]))
                    }
                    _ => (None, None),
                };
                // Hash order — the `Display` / `for` / `.iter()` walks all
                // agree, and design.md § Map requires it to vary per process
                // (B-2026-08-21-6).
                let (kt, vt) = (kt.cloned(), vt.cloned());
                let mut parts: Vec<String> = Vec::new();
                for (k, val) in &entries {
                    let ks = self.render_typed_mode(k, kt.as_ref(), debug);
                    let vs = self.render_typed_mode(val, vt.as_ref(), debug);
                    parts.push(format!("{}: {}", ks, vs));
                }
                format!("{{{}}}", parts.join(", "))
            }
            // Enum variants render `Variant` / `Variant(f0, f1)` /
            // `Variant { name: v }`, recursing so nested payloads format the
            // same way (and struct-variant fields in DECLARATION order, from
            // `enum_info`, not the payload `HashMap`'s hash order). This is the
            // enum sibling of the `Value::Struct` declaration-order fix above
            // and must match codegen's `emit_enum_display_fn` byte-for-byte.
            Value::EnumVariant {
                enum_name,
                variant,
                data,
            } => match data {
                EnumData::Unit => variant.clone(),
                EnumData::Tuple(vals) => {
                    let ptys = self.variant_payload_display_types(enum_name, variant, ty);
                    let vals = vals.clone();
                    let mut parts: Vec<String> = Vec::new();
                    for (i, x) in vals.iter().enumerate() {
                        let t = ptys.as_ref().and_then(|(_, t)| t.get(i)).cloned();
                        parts.push(self.render_typed_mode(x, t.as_ref(), debug));
                    }
                    format!("{}({})", variant, parts.join(", "))
                }
                EnumData::Struct(fields) => {
                    let order: Vec<String> = self
                        .typecheck_result
                        .enum_info
                        .get(enum_name)
                        .and_then(|ei| ei.variants.iter().find(|(n, _)| n == variant))
                        .and_then(|(_, vt)| match vt {
                            crate::typechecker::VariantTypeInfo::Struct(fs) => {
                                Some(fs.iter().map(|(n, _)| n.clone()).collect())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| fields.keys().cloned().collect());
                    let ptys = self.variant_payload_display_types(enum_name, variant, ty);
                    let variant = variant.clone();
                    let mut parts: Vec<String> = Vec::new();
                    for fname in &order {
                        let Some(fv) = fields.get(fname).cloned() else {
                            continue;
                        };
                        let fty = ptys
                            .as_ref()
                            .and_then(|(names, t)| {
                                names.iter().position(|n| n == fname).and_then(|i| t.get(i))
                            })
                            .cloned();
                        let rendered = self.render_typed_mode(&fv, fty.as_ref(), debug);
                        parts.push(format!("{}: {}", fname, rendered));
                    }
                    format!("{} {{ {} }}", variant, parts.join(", "))
                }
            },
            // The one leaf that depends on its type. An unsigned value at a
            // width whose top half does not fit the signed carrier is held as a
            // negative bit pattern, so the signed `Display` prints the wrong
            // reading — `-1` for `u64::MAX`. Narrower unsigned widths fit
            // non-negatively, so signed and unsigned coincide and they are
            // absent from `type_unsigned_int_width` rather than forgotten.
            Value::Int(n) => match ty.and_then(Self::type_unsigned_int_width) {
                Some(64) => format!("{}", *n as u64),
                Some(128) => format!("{}", *n as u128),
                _ => format!("{}", v),
            },
            // The two leaves where `Debug` and `Display` part ways. Rust's own
            // `{:?}` does the quoting and escaping, which is exactly what
            // `Value::debug_fmt` calls and what the runtime's
            // `karac_dbg_quote_str` / `_quote_char` call on the compiled side —
            // so all three agree by construction, not by convention.
            Value::String(s) if debug => format!("{:?}", s),
            Value::Char(c) if debug => format!("{:?}", c),
            // B-2026-08-24-2 — a `shared struct` walked in DECLARATION order,
            // the shared sibling of the `Value::Struct` arm above.
            //
            // Without this arm a shared struct fell to the `debug_fmt`
            // catch-all below, whose `SharedStruct` arm iterates its four
            // field `HashMap`s in sequence. That made `dbg(shared)` output
            // nondeterministic -- MEASURED 5 distinct field orders in 12 runs
            // of one binary -- and left the compiled backends with no stable
            // oracle to match, which is why `dbg` of a shared value refused to
            // compile at all rather than render something that disagreed.
            //
            // The four maps are a REPRESENTATION detail (mutability and
            // weakness), not something the user wrote, so rebuilding the
            // declared order from `struct_info` also stops the rendering from
            // grouping fields by a property the source never mentions. Weak
            // fields render through the upgrade, so a live target prints as
            // the struct it points at and a dead one prints `None`
            // (B-2026-08-08-14), same as every other read site.
            Value::SharedStruct(inner) => {
                let si = self.typecheck_result.struct_info.get(&inner.name);
                let order: Vec<String> = match si {
                    Some(si) => si.fields.iter().map(|(n, _, _)| n.clone()).collect(),
                    // Unknown type (ad-hoc harness, or a decl the typechecker
                    // never saw): fall back to the field names sorted, which is
                    // what `Display`'s own shared arm does. Deterministic is the
                    // contract; declaration order is the improvement on it.
                    None => {
                        let mut names: Vec<String> = inner
                            .immutable_fields
                            .keys()
                            .chain(inner.mut_fields.keys())
                            .chain(inner.weak_immutable_fields.keys())
                            .chain(inner.weak_mut_fields.keys())
                            .cloned()
                            .collect();
                        names.sort();
                        names
                    }
                };
                let field_tys = self.struct_field_display_types(&inner.name, ty);
                let inner = inner.clone();
                let mut parts: Vec<String> = Vec::new();
                for fname in &order {
                    let fty = field_tys.get(fname).cloned();
                    // Read each field's value OUT of its cell before rendering:
                    // a user `impl Display` runs Kāra code that may touch this
                    // same shared struct, and holding the borrow across that
                    // call would trip the `try_read` on re-entry.
                    let owned: Value = if let Some(v) = inner.immutable_fields.get(fname) {
                        v.clone()
                    } else if let Some(cell) = inner.mut_fields.get(fname) {
                        cell.value.try_read().expect(
                            "shared struct field write-locked during debug render — unreachable in single-task interpreter",
                        ).clone()
                    } else if let Some(weak) = inner.weak_immutable_fields.get(fname) {
                        upgrade_weak_to_option(weak)
                    } else if let Some(slot) = inner.weak_mut_fields.get(fname) {
                        let weak = slot.try_read().expect(
                            "shared struct weak field write-locked during debug render — unreachable in single-task interpreter",
                        );
                        upgrade_weak_to_option(&weak)
                    } else {
                        // Declared but absent: skip rather than invent a
                        // placeholder, matching the `Struct` arm's
                        // `fields.get(fname)` filter.
                        continue;
                    };
                    // A weak field renders through the upgrade, whose
                    // `Option[T]` shape carries no declared type here — pass
                    // `None`, as the closure form did.
                    let is_weak = inner.weak_immutable_fields.contains_key(fname)
                        || inner.weak_mut_fields.contains_key(fname);
                    let fty = if is_weak { None } else { fty };
                    let rendered = self.render_typed_mode(&owned, fty.as_ref(), debug);
                    parts.push(format!("{}: {}", fname, rendered));
                }
                format!("{} {{ {} }}", inner.name, parts.join(", "))
            }
            // Sets and the sorted collections, in DISPLAY mode only
            // (B-2026-08-26-29). These three fell to the catch-all below, which
            // formats through `Value`'s RUST `Display` — a renderer that cannot
            // reach a user `impl Display`, so a `Set[Ue]` printed `Set{B}` while
            // codegen, whose Set renderer recurses through the shared
            // `emit_display_fn_for_type_expr` dispatcher, printed `Set{bee}`.
            // Honoring the impl on one backend and not the other turned a
            // consistent-but-wrong rendering into a run-vs-build divergence, so
            // the walker has to destructure them too.
            //
            // Punctuation is copied verbatim from the `Value` `Display` arms
            // these replace (`Set{…}` / `SortedSet{…}` / `SortedMap{k: v}`,
            // `, `-separated) so nothing but the element dispatch changes.
            //
            // `debug` keeps the old path deliberately: `Debug` is a different
            // trait that must NOT reach the user impl, and routing these three
            // through the typed walker in that mode would also start quoting
            // their leaves — a real improvement, but a separate change from
            // this one, and not one to make silently.
            Value::Set(items) if !debug => {
                let items: Vec<Value> = items.read().unwrap().iter_observable().cloned().collect();
                let et = Self::display_element_type(ty).cloned();
                let mut parts: Vec<String> = Vec::new();
                for x in &items {
                    parts.push(self.render_typed_mode(x, et.as_ref(), debug));
                }
                format!("Set{{{}}}", parts.join(", "))
            }
            Value::SortedSet(set) if !debug => {
                let items: Vec<Value> = set.keys().map(|k| k.0.clone()).collect();
                let et = Self::display_element_type(ty).cloned();
                let mut parts: Vec<String> = Vec::new();
                for x in &items {
                    parts.push(self.render_typed_mode(x, et.as_ref(), debug));
                }
                format!("SortedSet{{{}}}", parts.join(", "))
            }
            Value::SortedMap(map) if !debug => {
                let entries: Vec<(Value, Value)> =
                    map.iter().map(|(k, v)| (k.0.clone(), v.clone())).collect();
                let (kt, vt) = match ty {
                    Some(Type::Named { name, args })
                        if (name == "Map" || name == "SortedMap") && args.len() == 2 =>
                    {
                        (Some(args[0].clone()), Some(args[1].clone()))
                    }
                    _ => (None, None),
                };
                let mut parts: Vec<String> = Vec::new();
                for (k, v) in &entries {
                    let ks = self.render_typed_mode(k, kt.as_ref(), debug);
                    let vs = self.render_typed_mode(v, vt.as_ref(), debug);
                    parts.push(format!("{}: {}", ks, vs));
                }
                format!("SortedMap{{{}}}", parts.join(", "))
            }
            // `debug_fmt` for the shapes this walker does not destructure
            // (shared structs, sets, sorted collections): still quoted at the
            // leaves, just without the declaration-order/type threading.
            other if debug => other.debug_fmt(),
            other => format!("{}", other),
        }
    }

    /// Declared field types for `struct_name`, with the struct's generic
    /// parameters substituted from the concrete `ty` when there is one — so a
    /// `Wrap[u64]`'s `T` field renders as a `u64`, not as an unresolved param.
    /// Empty when the struct is unknown.
    fn struct_field_display_types(
        &self,
        struct_name: &str,
        ty: Option<&crate::typechecker::Type>,
    ) -> std::collections::HashMap<String, crate::typechecker::Type> {
        let Some(si) = self.typecheck_result.struct_info.get(struct_name) else {
            return std::collections::HashMap::new();
        };
        let subs = Self::display_type_subs(&si.generic_params, ty);
        si.fields
            .iter()
            .map(|(n, t, _)| {
                (
                    n.clone(),
                    crate::typechecker::inference::substitute_type_params(t, &subs),
                )
            })
            .collect()
    }

    /// Declared payload types for `enum_name::variant` — the field NAMES (empty
    /// for a tuple variant) and their types, with the enum's generic parameters
    /// substituted from the concrete `ty`. That substitution is what makes the
    /// seeded generics work: `Option`'s `Some` declares a bare `T`, so without
    /// it an `Option[u64]` payload carries no width at all.
    #[allow(clippy::type_complexity)]
    fn variant_payload_display_types(
        &self,
        enum_name: &str,
        variant: &str,
        ty: Option<&crate::typechecker::Type>,
    ) -> Option<(Vec<String>, Vec<crate::typechecker::Type>)> {
        use crate::typechecker::VariantTypeInfo;
        let ei = self.typecheck_result.enum_info.get(enum_name)?;
        let (_, vt) = ei.variants.iter().find(|(n, _)| n == variant)?;
        let subs = Self::display_type_subs(&ei.generic_params, ty);
        let sub = |t: &crate::typechecker::Type| {
            crate::typechecker::inference::substitute_type_params(t, &subs)
        };
        Some(match vt {
            VariantTypeInfo::Unit => (Vec::new(), Vec::new()),
            VariantTypeInfo::Tuple(ts) => (Vec::new(), ts.iter().map(sub).collect()),
            VariantTypeInfo::Struct(fs) => (
                fs.iter().map(|(n, _)| n.clone()).collect(),
                fs.iter().map(|(_, t)| sub(t)).collect(),
            ),
        })
    }

    /// Positional generic substitution from a concrete `Named` type onto a
    /// declaration's parameter list. Empty when either side is missing, which
    /// leaves `substitute_type_params` a no-op and the rendering signed —
    /// the same degradation as an unknown type.
    fn display_type_subs(
        params: &[String],
        ty: Option<&crate::typechecker::Type>,
    ) -> std::collections::HashMap<String, crate::typechecker::SubstValue> {
        use crate::typechecker::{SubstValue, Type};
        let mut subs = std::collections::HashMap::new();
        if let Some(Type::Named { args, .. }) = ty {
            for (p, a) in params.iter().zip(args.iter()) {
                subs.insert(p.clone(), SubstValue::Type(a.clone()));
            }
        }
        subs
    }

    /// The element type of a sequence type, for the `Array` / `Slice` arms.
    /// Covers every spelling those two `Value`s can carry — `Vec[T]` and the
    /// sorted/set family arrive as `Named`, fixed arrays and slices as their
    /// own variants.
    fn display_element_type(
        ty: Option<&crate::typechecker::Type>,
    ) -> Option<&crate::typechecker::Type> {
        use crate::typechecker::Type;
        match ty? {
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Slice { element, .. } => Some(element),
            Type::Named { name, args }
                if matches!(
                    name.as_str(),
                    "Vec" | "VecDeque" | "Set" | "SortedSet" | "Column" | "Tensor"
                ) && !args.is_empty() =>
            {
                Some(&args[0])
            }
            _ => None,
        }
    }

    /// Return the impl-method key (`<TypeName>.to_string`) when `v` is a
    /// user-declared nominal type (struct / enum) carrying a user
    /// `impl Display` — i.e. a registered `to_string` method, as opposed to the
    /// built-in `display_render` renderer or a `#[derive(Display)]`. Used to let
    /// a user `impl Display` win over the built-in `to_string` path so it takes
    /// effect for `x.to_string()`, `f"{x}"`, and `println(x)`. GAP-W4.
    pub(crate) fn user_display_impl_to_string_key(&self, v: &Value) -> Option<String> {
        match v {
            // `SharedStruct` belongs here for the same reason the other two do
            // — it is a user-declared nominal type that can carry an
            // `impl Display` — and its absence was a run-vs-build divergence at
            // DEPTH 0, not just under a container (B-2026-08-26-29): codegen's
            // `user_display_impl_type` resolves a shared struct through
            // `expr_user_struct_name` (shared types are registered in
            // `struct_field_names`), so `println(a)` on a `shared struct Sh`
            // with an `impl Display` printed `sh(1)` under `karac build` and
            // `Sh { v: 1 }` under `--interp`. `value_type_name` already names a
            // `SharedStruct` by `inner.name`, so the key lookup needed nothing.
            //
            // A shared ENUM needs no entry: it rides as a `Value::EnumVariant`
            // and was already matched.
            Value::Struct { .. } | Value::EnumVariant { .. } | Value::SharedStruct(_) => {}
            _ => return None,
        }
        let key = format!("{}.to_string", self.value_type_name(v));
        self.env.get(&key).is_some().then_some(key)
    }

    /// Render `v` the way the language says a value renders: through its user
    /// `impl Display` when it has one, and through the built-in / derived
    /// renderer otherwise.
    ///
    /// B-2026-08-23-21. The expression-driven twin of this is
    /// `eval_method_call(expr, "to_string", ..)`, which `println(x)` and
    /// f-string interpolation use — but it needs an AST expression to recover
    /// the receiver's type from. Sites that hold only a `Value` had no
    /// equivalent, so they reached for `format!("{v}")`, i.e. `Value`'s RUST
    /// `Display`. That is a different renderer: it ignores the user impl
    /// entirely, and for a struct it walks a `HashMap`, so field order is
    /// randomised per process by Rust's `RandomState` (NOT the Kāra hasher, so
    /// `KARAC_HASH_SEED` cannot pin it). The `main() -> Result[(), E]` error
    /// exit did exactly that and printed a different string from the compiled
    /// backends — and a different one on each run.
    pub(crate) fn render_value_via_display(&mut self, v: &Value) -> String {
        if let Some(key) = self.user_display_impl_to_string_key(v) {
            if let Value::String(s) = self.call_function(&key, std::slice::from_ref(v)) {
                return s;
            }
        }
        self.display_render_typed(v, None)
    }

    pub(crate) fn eval_builtin_print(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        // Route through the Stdout / Stderr provider stack so a
        // `with_provider[Stdout]` / `[Stderr]` install can intercept idiomatic
        // `println(x)` calls — not just direct `Stdout.println(s)` calls.
        // The user's provider method receives an already-formatted String;
        // the BuiltinDefault arm writes through `write_stdout` /
        // `write_stderr` (honoring `captured_output` for the test harness).
        let val = if let Some(arg) = args.first() {
            // Render through the unified `to_string` dispatch so `println(x)`
            // honors a user `impl Display` (built-in types fall through to
            // `display_render` inside that dispatch). GAP-W4.
            // `args_close_span` is the ARGUMENT's span, not the `println`
            // call's. The dispatch uses that span to recover the RECEIVER's
            // type — for a real `x.to_string()` the close paren is the only
            // leaf the parser has not aliased to the receiver — and passing the
            // call's span instead resolved it to the call's own `Unit`, a hit
            // that shadowed the correct fallback. That is why `println(o)` on
            // an `Option[u64]` still rendered signed after the renderer learned
            // to take a type (B-2026-08-19-27).
            match self.eval_method_call(&arg.value, "to_string", &[], span, &arg.value.span) {
                Value::String(s) => s,
                other => {
                    self.display_render_typed(&other, self.span_expr_type(&arg.value.span).as_ref())
                }
            }
        } else {
            String::new()
        };
        if self.check_cf() {
            return Value::Unit;
        }
        let (resource, method) = match name {
            "eprintln" => ("Stderr", "println"),
            "println" => ("Stdout", "println"),
            _ => ("Stdout", "print"),
        };
        self.dispatch_resource_method_with_values(resource, method, vec![Value::String(val)], span)
    }

    /// Write to stdout, honoring `captured_output` when the test harness
    /// installed it. Used by both the free `print` / `println` router
    /// and the `Stdout.print` / `Stdout.println` resource methods so the
    /// two surfaces share one capture path.
    pub(crate) fn write_stdout(&mut self, s: &str, newline: bool) {
        // A `par {}` branch's capture comes first: it defers BOTH streams so
        // the join can replay them in source order (B-2026-08-23-15). The two
        // buffers are never both installed on one interpreter.
        if let Some(ref mut segs) = self.captured_console {
            segs.push(ConsoleSeg {
                stream: ConsoleStream::Stdout,
                text: if newline {
                    format!("{s}\n")
                } else {
                    s.to_string()
                },
            });
        } else if let Some(ref mut output) = self.captured_output {
            if newline {
                output.push(format!("{}\n", s));
            } else {
                output.push(s.to_string());
            }
        } else {
            Self::write_program_stdout(s, newline);
        }
    }

    /// The interpreter's one real write to the program's stdout.
    ///
    /// NOT `println!` — B-2026-08-19-2. That macro panics when the write
    /// fails, and the write fails routinely: `karac run --interp prog | head`
    /// closes the reader after a couple of lines, and the next write returns
    /// `BrokenPipe`. The panic then spilled 55 lines of Rust backtrace naming
    /// `karac::interpreter::builtin::write_stdout`, which reads as a compiler
    /// crash rather than as the reader having gone away, and exited 101.
    ///
    /// WHY THE INTERPRETER AND NOT THE OTHER BACKENDS: an AOT binary is the
    /// process, so it inherits the default `SIGPIPE` disposition and the kernel
    /// kills it — silently, with status 141. `karac` is a Rust program, and
    /// Rust sets `SIGPIPE` to `SIG_IGN` at startup, so inside the interpreter
    /// the signal never arrives and the write reports `EPIPE` instead.
    ///
    /// So this reproduces what the kernel would have done: exit 141
    /// (`128 + SIGPIPE`) without a message. Skipping destructors is not a
    /// shortcut — a signal death runs none either, so the abrupt exit is the
    /// faithful part rather than the lossy part.
    ///
    /// Every OTHER io error is left to `expect`. A full disk or a closed
    /// terminal is a real failure the user needs told about; only the
    /// reader-went-away case is normal enough to be silent.
    fn write_program_stdout(s: &str, newline: bool) {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        let res = if newline {
            writeln!(lock, "{s}")
        } else {
            write!(lock, "{s}").and_then(|()| lock.flush())
        };
        match res {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                std::process::exit(141);
            }
            Err(e) => panic!("failed printing to stdout: {e}"),
        }
    }

    /// Write to stderr, honoring a `par {}` branch's `captured_console` so
    /// the join can replay it in source order (B-2026-08-23-15) — design.md
    /// § dbg() promises exactly that of `eprintln`, and the compiled backends
    /// already deliver it. Mirrors `write_stdout` so the `Stderr` arms have
    /// the same shape as `Stdout`'s.
    ///
    /// `captured_output` is deliberately NOT consulted: it is the test
    /// harness's stdout-only snapshot buffer, and folding stderr into it
    /// would silently rewrite what every existing stdout assertion sees.
    pub(crate) fn write_stderr(&mut self, s: &str, newline: bool) {
        if let Some(ref mut segs) = self.captured_console {
            segs.push(ConsoleSeg {
                stream: ConsoleStream::Stderr,
                text: if newline {
                    format!("{s}\n")
                } else {
                    s.to_string()
                },
            });
            return;
        }
        if newline {
            eprintln!("{}", s);
        } else {
            eprint!("{}", s);
        }
    }

    pub(crate) fn eval_builtin_dbg(&mut self, args: &[CallArg], span: &Span) -> Value {
        // dbg() uses the transparent `debugs` effect (design.md § dbg() —
        // transparent and stripped in release builds), but the underlying
        // I/O still writes stderr. The track_effect call records that for
        // any future runtime instrumentation; transparency is enforced by
        // the static effect checker, not here.
        self.track_effect("writes(Stderr)");
        let arg_expr = args.first().map(|a| &a.value);
        let val = if let Some(expr) = arg_expr {
            self.eval_expr_inner(expr)
        } else {
            Value::Unit
        };

        // Source slice for the `expr` field. Falls back to "<expr>" when
        // the interpreter was constructed without a source-text setter
        // (some unit tests bypass the CLI) or the slice would be empty.
        let expr_text = arg_expr
            .and_then(|e| {
                let off = e.span.offset;
                let end = off.saturating_add(e.span.length);
                self.source_text.get(off..end)
            })
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "<expr>".to_string());

        // Type lookup via the typecheck side table. "?" when unavailable;
        // not all expression kinds reach the typechecker's recording path,
        // and ad-hoc test harnesses sometimes synthesize a TypeCheckResult
        // without populating expr_types.
        let arg_ty = arg_expr.and_then(|e| {
            self.typecheck_result
                .expr_types
                .get(&SpanKey::from_span(&e.span))
                .cloned()
        });
        let type_text = arg_ty
            .as_ref()
            .map(type_display)
            .unwrap_or_else(|| "?".to_string());

        let file = if self.source_filename.is_empty() {
            "<unknown>".to_string()
        } else {
            self.source_filename.clone()
        };
        // B-2026-08-23-18: render through the DECLARATION-ORDERED, type-aware
        // debug walker, not the bare `Value::debug_fmt`. The latter walks a
        // struct's field `HashMap` directly, so it reported a different field
        // order on every run — nondeterministic output, and no stable oracle
        // for the compiled backends to match.
        let value_str = self.debug_render_typed(&val, arg_ty.as_ref());

        let line = match self.dbg_output_mode {
            DbgOutputMode::Terminal => match self.current_task_id {
                Some(tid) => format!(
                    "[task:{} {}:{}] {} = {}\n",
                    tid, file, span.line, expr_text, value_str
                ),
                None => format!("[{}:{}] {} = {}\n", file, span.line, expr_text, value_str),
            },
            DbgOutputMode::Json => {
                let task_id = match self.current_task_id {
                    Some(tid) => tid.to_string(),
                    None => "null".to_string(),
                };
                format!(
                    "{{\"kind\":\"dbg\",\"task_id\":{},\"file\":{},\"line\":{},\"expr\":{},\"type\":{},\"value\":{}}}\n",
                    task_id,
                    dbg_json_escape(&file),
                    span.line,
                    dbg_json_escape(&expr_text),
                    dbg_json_escape(&type_text),
                    dbg_json_escape(&value_str),
                )
            }
        };

        if let Some(ref mut cap) = self.captured_dbg {
            cap.push(line);
        } else {
            // Single atomic write — POSIX guarantees writes up to
            // PIPE_BUF bytes (4096 on Linux) are atomic at the
            // syscall level, so sibling-task lines never tear.
            use std::io::Write;
            let stderr = std::io::stderr();
            let mut handle = stderr.lock();
            let _ = handle.write_all(line.as_bytes());
        }

        val
    }

    pub(crate) fn eval_builtin_assert(&mut self, args: &[CallArg], span: &Span) -> Value {
        self.track_effect("panics");
        let cond = match args.first() {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert called with no arguments", span),
        };
        if self.pending_cf.is_some() {
            return cond;
        }
        if matches!(cond, Value::Bool(true)) {
            return Value::Unit;
        }
        // Optional 2-arg `assert(cond, "msg")` failure message. A string
        // LITERAL is used verbatim; a dynamic message falls back to the bare
        // "assertion failed" — kept symmetric with codegen's `compile_assert`
        // so the two backends report the same text (B-2026-07-18-26).
        let msg = match args.get(1).map(|a| &a.value.kind) {
            Some(ExprKind::StringLit(s)) => s.as_str(),
            _ => "assertion failed",
        };
        self.record_runtime_error(msg, span)
    }

    pub(crate) fn eval_builtin_assert_eq(&mut self, args: &[CallArg], span: &Span) -> Value {
        self.track_effect("panics");
        let left = match args.first() {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert_eq requires two arguments", span),
        };
        let right = match args.get(1) {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert_eq requires two arguments", span),
        };
        if left == right {
            return Value::Unit;
        }
        let lstr = left.debug_fmt();
        let rstr = right.debug_fmt();
        self.record_runtime_assertion("assertion failed: left != right", lstr, rstr, span)
    }

    pub(crate) fn eval_builtin_assert_ne(&mut self, args: &[CallArg], span: &Span) -> Value {
        self.track_effect("panics");
        let left = match args.first() {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert_ne requires two arguments", span),
        };
        let right = match args.get(1) {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert_ne requires two arguments", span),
        };
        if left != right {
            return Value::Unit;
        }
        let lstr = left.debug_fmt();
        let rstr = right.debug_fmt();
        self.record_runtime_assertion("assertion failed: left == right", lstr, rstr, span)
    }

    /// `std.time::sleep_ms(ms: i64)` — the tree-walk interpreter has no
    /// async reactor, so the faithful semantics of a `suspends` sleep is
    /// a real wall-clock pause: block this thread for `ms` milliseconds.
    /// The codegen path (`emit_state_machine_invocation_for_park_on_timer`)
    /// instead parks the task on the reactor's timer wheel so siblings in a
    /// `par {}` overlap; the interpreter is sequential, so a thread sleep
    /// matches its execution model. Negative / missing arg → no-op.
    ///
    /// wasm32 (the browser playground runs this interpreter client-side):
    /// `std::thread::sleep` panics (`sys/unsupported`) and the synchronous
    /// tree-walk cannot block the browser main thread anyway, so the pause
    /// is a no-op there — the arg is still evaluated for its effects.
    pub(crate) fn eval_builtin_sleep_ms(&mut self, args: &[CallArg], span: &Span) -> Value {
        self.track_effect("suspends");
        let ms = match args.first() {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("sleep_ms requires one argument", span),
        };
        if let Value::Int(ms) = ms {
            if ms > 0 {
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::sleep(std::time::Duration::from_millis(ms as u64));
            }
        }
        Value::Unit
    }
}
