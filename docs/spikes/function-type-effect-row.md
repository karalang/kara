# An effect row on `Type::Function` (B-2026-08-23-11)

**Status:** design resolved, **implementation deliberately not scheduled**.
The question the row asked — should the typechecker's function type carry
effects, and how, given that it cannot know them — has an answer, and the
answer changes what the work *is*: this is a **prerequisite for the package
manager**, not a standalone precision improvement. Until `[dependencies]`
does something, the whole-program net already covers every path measured
here. Read "The scheduling answer" for the one-paragraph version.

## The question

design.md § First-Class Functions spells a function value's type as

```
let f = save;    // f: Fn(User) -> () with writes(UserDB)
```

`Type::Function { params, return_type }` has no effect field, so that
spelling is not representable and effects do not propagate through the
TYPE — they propagate along the effect checker's name-keyed call-graph
edges. B-2026-08-23-11 proposed adding the field. Its size estimate was 131
`Type::Function` sites plus 86 `Type::OnceFunction` across 27 files.

That estimate measures the cheap part.

## Finding 1 — the blocker dissolves, and the row was asking the wrong thing

The obvious objection is a phase cycle. `effectcheck` runs AFTER
`typecheck` **and consumes its output** (`effectcheck_with_typecheck_data`
takes `method_callee_types` and `call_type_subs` to resolve method
callees), so a typechecker that needed inferred effects would need an edge
back — and inference for private functions is the entire point of the
effect system. Declared effects are syntactic and available; inferred ones
are not.

That objection assumes the row must hold an effect SET. It does not. The
typechecker does not need to know a function's effects — it needs to carry
a **variable** and let constraints accumulate, exactly as it already does
for type variables. Solving happens later, in the pass that has the call
graph.

This is not speculative: **the language already has effect variables and
the effect checker already solves them.**

```
fn map[T, U, with E](list: Vec[T], f: Fn(T) -> U with E) -> Vec[U] with E
```

`with E` is an effect parameter in the generic list (design.md § Effect
Polymorphism). `build_fn_effect_var_positions` (`src/effectchecker/
bounds.rs`) indexes each function's effect variables to the parameter
positions they appear in, and `compute_call_var_bindings`
(`src/effectchecker.rs`) binds each variable at a call site by unioning the
effects of the arguments at those positions.

So the row is `Concrete(EffectSet) | Var(name)`, written by the typechecker
straight from syntax, solved by the effect checker with machinery it
already has. **No cycle.** The three-state `Known | Unknown` shape
considered earlier is the same idea in weaker form; `Var` is better,
because a variable can be *unified* while "unknown" can only be tolerated.

## Finding 2 — nothing today depends on it

Every path a function value can take was measured against current `main`.
All four demand the declaration, none leaks:

| shape | verdict |
|---|---|
| `let f = save; f(7)` | demands `writes(UserDB)` |
| effect var at a param position, value via a `let` | demands |
| `Vec[Fn(..) with E]` container param | demands |
| across a `pub` boundary (`pub fn get() -> Fn(..)`) | demands |

B-2026-08-23-7's rule — a mention of a free function in value position
contributes its effects to the enclosing function — is what closes all of
them, and it needs no type row. The five slot positions (B-2026-08-23-12,
B-2026-08-24-1) and the container element slots (B-2026-08-24-11) close the
complementary half: whether a declared slot is a *lie*. Between them there
is no measured hole.

## Finding 3 — the reason there is no hole is that v1 has no separate compilation

design.md § 6297: *"Public function boundaries act as inference firewalls —
the compiler uses **declared** effects, not inferred ones, when crossing
public boundaries."*

design.md § 1115: `[dependencies]` and every other manifest section beyond
`[package].{name, edition, version, authors}` is *"ignored, not rejected"*
in v1; the package-manager work lands later.

Put together: the firewall is real in the design, but today every build is
one package and the effect checker sees every body, so the mention rule
reaches across the boundary anyway. **The moment packages land, it cannot.**
A downstream consumer of

```
pub fn get() -> (Fn(i64) -> i64 with writes(UserDB)) with writes(UserDB)
```

has only the signature. The `with` inside the parentheses is exactly the
row this spike is about — and today it is parsed, checked against the
returned value (B-2026-08-24-1), and then **discarded at lowering**:
`lower_type_expr_inner` (`src/typechecker/lowering.rs`) matches
`TypeKind::FnType { params, return_type, is_once, .. }` and drops
`effect_spec` on the floor. One `..` is the whole gap.

## The scheduling answer

**The row is a prerequisite for the package manager, and should be
scheduled with it, not before it.** Doing it now buys diagnostics polish
(hover and `karac explain` could show a value's effects) and some precision
at the margins; doing it *after* packages ship would mean shipping a
soundness hole and fixing it under compatibility pressure. Doing it *with*
packages is when its cost is justified by something that cannot be
delivered another way.

## What the work actually is

Not 217 construction sites. Those are mechanical — every one of them names
a fresh field, and most match sites already use `..`. The work is:

1. **`EffectRow` on `Type::Function` / `Type::OnceFunction`** —
   `Concrete(EffectSet) | Var(Symbol)`. Populated in
   `lower_type_expr_inner` from the `effect_spec` it currently drops.
2. **Unification** — `types_compatible` / `is_subtype` gain an effect leg.
   `is_subtype`'s doc comment already reserves the spot: *"Effect-set
   variance … is deferred until phase-3 lands effect variables on
   `Type::Function` — the type lacks an effect-set field today."*
   Variance is contravariant in params, covariant in the row.
3. **The solver, which is the real cost.**
   `build_fn_effect_var_positions` keys a variable to top-level parameter
   INDICES. That shape cannot express a variable in a return type, inside a
   container, or bound through a `let` — the three things the row exists to
   enable. Replacing index-keyed positional binding with structural
   unification over the row is the actual project; the field is its
   precondition.
4. **Display, codegen, interpreter** — the row is analysis-only and must
   not reach codegen's layout decisions. `Type::Function`'s LLVM lowering
   ignores it; the containment rule in CLAUDE.md applies unchanged.

## Not to be redone

- **Do not** try to populate the row with inferred effect SETS at
  typecheck time. That is the cycle; carry a variable instead.
- **Do not** re-derive the size estimate from `grep -c Type::Function`.
  51 of the 218 hits construct; the rest match and mostly use `..`.
- **Do not** treat the mention rule as a stopgap to be removed once the row
  lands. It is the whole-program half of the story and stays sound
  regardless; the row is the cross-package half.

## Related rows

- B-2026-08-23-7 — the mention rule (the soundness net measured above).
- B-2026-08-23-11 — this row; narrowed to representability in `522e4d2`.
- B-2026-08-23-12, B-2026-08-24-1, B-2026-08-24-11 — the six declared-slot
  positions, which check whether a written row is honest.
- B-2026-08-24-8 — `panics` is transparent for slot subtyping; whatever the
  row's comparison ends up being must inherit that rule.
