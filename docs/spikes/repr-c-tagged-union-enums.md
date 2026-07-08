# Spike: `#[repr(C)]` enums across the C ABI

**Status:** ✅ **Slice 1 SHIPPED (2026-07-08)** — all-unit `#[repr(C)]` enums cross transparently as `int64_t`. Slice 2 (data-carrying tagged unions) remains deferred. Verified end-to-end: a C host round-trips `Status { Ok, NotFound, Denied }` as both param and return, correct values (`0 1 2 2 1`), on x86-64 SysV (`test_build_repr_c_enum_roundtrip_from_c_e2e`). **AArch64/Apple ABI still unverified** — the single-`{i64}`-struct == `int64_t` equivalence holds there by the same one-eightbyte-in-one-register rule, but it should be confirmed on an Apple runner before that target's producer story is called done.

**Origin:** the additive-interop producer-mode work (`spikes/additive-interop-adoption.md`, roadmap Tier 5) rejects `enum` returns/params at the export boundary with `E_EXPORT_ABI`, because a boxed enum is an opaque pointer the C side can't read. The category-specific reject diagnostic now says "return a `#[repr(C)]` struct with a tag field, or an opaque handle + accessor exports — repr(C) tagged-union enums are a planned follow-on." **This spike is that follow-on.** It is deliberately *not* bolted onto producer mode: making an `enum` cross transparently is a cross-cutting language feature (parser + codegen layout + header), so it gets its own scoping.

## The problem

A Kāra kernel wants to hand C a discriminated value — a status code (`Ok`/`NotFound`/`Denied`), a small tagged result. Today the only honest export shapes are primitives, raw pointers, `#[repr(C)]` structs, and auto-boxed `Vec`/`String` returns. An `enum` has no by-value C form, so it's rejected. The adoption ask is real: returning a `Result`-like or kind enum from a kernel is one of the most common C-FFI shapes.

## Why it's not a one-liner: the layout mismatch

Kāra lays out **every** enum (see `codegen/declarations.rs::declare_enums`) as a flat tagged struct:

```
{ i64 tag, i64 w0, i64 w1, …, i64 wN }   // N = max_payload_words across variants
```

The payload is a **unified i64-word area** sized to the widest variant, with a per-variant per-field word-offset map (`EnumLayout::field_word_offsets`). Each source field is coerced into one or more i64 words; aggregate payloads (`String`, `Vec`, nested enums) occupy several words whose interpretation is known only to codegen.

C's tagged-union idiom is entirely different:

```c
struct Msg {
    int32_t tag;
    union {
        struct { }            Ping;    // unit variant
        struct { int64_t _0; } Data;    // Data(i64)
    } payload;
};
```

The two do not agree on:
- **tag width** — Kāra `i64` (8 B) vs C `enum`/`int` (4 B);
- **payload shape** — Kāra opaque i64 words vs C a union of per-variant structs with natural field layout;
- **aggregate payloads** — a `String`/`Vec` word-run in Kāra has no by-value C form at all (it's the same reason those are auto-boxed as returns, not passed by value).

So "emit the current layout as a C struct" is a non-starter for data-carrying variants: C could read the `tag` but not a `Data(f64)` (it's a bit-cast i64 word) and certainly not a `Data(String)`.

## The split

This cleaves into two very different slices:

### Slice 1 (SHIPPED): all-unit `#[repr(C)]` enums → transparent C integer

An enum whose every variant is `Unit` (`enum Color { Red, Green, Blue }`) has **no payload** — its layout is just `{ i64 tag }`, and its value *is* the discriminant. This is the C "enum as named integer constants" case, and it's both the most common adoption shape (status/kind/mode codes) and cleanly transparent.

**ABI mapping.** Declare the exported type as `int64_t` (matching Kāra's i64 tag width), with the variant names emitted as named constants:

```c
/* Color — a #[repr(C)] all-unit Kāra enum (values are the discriminants). */
typedef int64_t Color;
enum { Color_Red = 0, Color_Green = 1, Color_Blue = 2 };
```

`typedef int64_t` (not a bare C `enum`, which is `int`/4 B and would mismatch the 8-B Kāra tag) keeps the by-value ABI honest; the anonymous `enum { … }` gives the C caller readable named constants without changing the width. (A C23 `enum Color : int64_t` would be tidier but isn't portable yet.)

**The one thing to verify, not assume:** that a Kāra all-unit enum returned/passed by value is ABI-identical to an `int64_t`. The layout is a single-field struct `{ i64 }`; on x86-64 SysV a one-eightbyte struct returns in RAX and passes in one integer register — identical to a bare `i64` — so the mapping *should* be exact, but this must be confirmed by `objdump` + a C round-trip under ASAN (the same discipline that caught the multi-register `{data,len,cap}` return mismatch in Path B), on both SysV and AArch64 (Apple), before it ships.

**Work items (Slice 1) — all done:**
1. ✅ **Parser/attributes** — `#[repr(C)]` was already parsed onto `EnumDef` (the parser attaches attributes generically; `EnumDef` carries an `attributes: Vec<Attribute>` field), so no parser change was needed. `cheader::attrs_have_repr_c` + `is_repr_c_all_unit_enum` classify it.
2. ✅ **`cheader.rs`** — `is_transparent_boundary_type` treats an all-unit `#[repr(C)]` enum as transparent; the header emits `typedef int64_t <Name>;` + an anonymous `enum { <Name>_<Variant> = tag, … }` named-constant block (emitted before struct bodies so an enum-typed struct field resolves); prototypes use the enum name (the `int64_t` alias) by value. A data-carrying `#[repr(C)]` enum stays rejected; the `abi_fix_hint` enum branch names the all-unit path explicitly.
3. ✅ **`validate_exports`** — all-unit `#[repr(C)]` enum passes as return AND param; data-carrying stays rejected (`data_carrying_repr_c_enum_still_rejected` test).
4. ✅ **Codegen** — no change needed: the round-trip confirmed the all-unit enum value ABI already matches `int64_t` (a single-field `{i64}` struct returns/passes in one integer register on SysV, identical to a bare `i64`).
5. ✅ **Tests** — `cheader` unit tests `all_unit_repr_c_enum_crosses_transparently` + `data_carrying_repr_c_enum_still_rejected`; E2E `test_build_repr_c_enum_roundtrip_from_c_e2e` (a C host round-trips `Status` as param + return, asserts `0 1 2 2 1` and the header shape).

### Slice 2 (deferred): data-carrying `#[repr(C)]` enums → real C tagged union

The full feature: `#[repr(C)] enum Msg { Ping, Data(i64), Pair(i32, i32) }` → a C `{ int32_t tag; union { … } payload; }` with each variant's payload as a natural C struct. This needs a **second enum layout** in codegen (a real union, not the i64-word area) selected on `#[repr(C)]`, plus header emission of the union, plus the by-value ABI matching (a tagged union is returned via the aggregate ABI — likely the same box-to-pointer treatment Path B uses for `{data,len,cap}`, since a multi-eightbyte struct isn't register-returned). Variants with aggregate payloads (`String`, `Vec`) stay rejected — those have no by-value C form regardless. This is a multi-week slice with real ABI surface; it should not start until Slice 1 has shipped and proven the parser/header/validate plumbing.

## Recommendation

Ship **Slice 1** (all-unit → transparent `int64_t`). It covers the common status/kind-code adoption shape, is honestly transparent, and reuses the `#[repr(C)]`-struct plumbing almost verbatim. Gate it on an objdump + ASAN round-trip confirming the single-`{i64}`-struct ABI equals `int64_t` on SysV and AArch64. Keep data-carrying enums rejected with a message that names Slice 1's boundary ("all-unit repr(C) enums cross at v1; a tagged union with payloads is a follow-on"). Revisit Slice 2 only if a concrete consumer needs it — a `#[repr(C)]` struct with a tag field + out-param already covers most data-carrying cases without new codegen.
