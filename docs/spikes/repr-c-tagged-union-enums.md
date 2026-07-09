# Spike: `#[repr(C)]` enums across the C ABI

**Status:** ✅ **Slices 1 + 2a SHIPPED (2026-07-08/09).** Slice 1: all-unit `#[repr(C)]` enums cross transparently as `int64_t`. Slice 2a: `#[repr(C)]` enums with unit + single-scalar variants cross as a heap-boxed pointer to a faithful C tagged union `{ int64_t tag; union {…} payload; }`. Both verified end-to-end from a C host: Slice 1 round-trips `Status { Ok, NotFound, Denied }` as param + return (`0 1 2 2 1`, `test_build_repr_c_enum_roundtrip_from_c_e2e`); Slice 2a round-trips `Msg { Ping, Data(i64), Ratio(f64) }` (`0 1 4242 2.5`, `test_build_repr_c_enum_tagged_union_from_c_e2e`) and is **ASAN/LeakSanitizer-clean** (box malloc + box-only destructor balance). **Slice 2b/2c (multi-scalar / aggregate-payload variants) remain deferred** — still rejected. **AArch64/Apple ABI unverified** for both slices (holds by the same one-eightbyte-per-register rule; confirm on an Apple runner).

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
3. ✅ **`validate_exports`** — all-unit `#[repr(C)]` enum passes as return AND param; a non-boxable data-carrying enum stays rejected (`non_boxable_data_carrying_repr_c_enum_still_rejected` test).
4. ✅ **Codegen** — no change needed: the round-trip confirmed the all-unit enum value ABI already matches `int64_t` (a single-field `{i64}` struct returns/passes in one integer register on SysV, identical to a bare `i64`).
5. ✅ **Tests** — `cheader` unit tests `all_unit_repr_c_enum_crosses_transparently` + `data_carrying_repr_c_enum_still_rejected`; E2E `test_build_repr_c_enum_roundtrip_from_c_e2e` (a C host round-trips `Status` as param + return, asserts `0 1 2 2 1` and the header shape).

### Slice 2a (SHIPPED): scalar-payload `#[repr(C)]` enums → boxed faithful C tagged union

`#[repr(C)] enum Msg { Ping, Data(i64), Ratio(f64) }` — every variant unit OR a single scalar (integer / float / bool / raw pointer) — now crosses as a **heap-boxed pointer** to a faithful C tagged union. The insight that made this tractable without a second enum layout: Kāra already stores a scalar payload in one i64 word via `coerce_to_i64` (integers **zero-extended**, floats **bit-cast**, pointers `ptrtoint`), so a C `union` member typed per variant, overlaying that word, reads it faithfully. The emitted C exactly matches Kāra's `{ i64 tag, i64 w0 }`:

```c
enum { Msg_Ping = 0, Msg_Data = 1, Msg_Ratio = 2 };
typedef struct { int64_t tag; union { int64_t Data; double Ratio; } payload; } Msg;
Msg* make(int64_t kind);
void karac_free_make(Msg* handle);
```

**What shipped:**
- **Boxing reuses Path B** — `boxed_enum_export_names` (a codegen set, since `boxed_return_of` can't see enum defs) marks the return; the three boxing sites (`declare_function` → `ptr`, `current_fn_boxes_return`, the tail/explicit-return box) fire; `box_return_value` was generalized to size the box from the value's own struct type (equivalent to `vec_struct_type` for the Vec path).
- **A distinct box-only destructor** — the load-bearing correctness point: the Vec-box destructor's `emit_free_vec_buffer_if_owned` would read an enum payload word as a `data` pointer and free garbage. `emit_one_export_destructor` takes an `is_plain_box` flag; the enum box (scalar payloads own no heap) just frees the box.
- **cheader** — `repr_c_enum_box_variants` classifies the boxable shape; `emit_c_header` emits the tagged union + `<Name>*` return + `karac_free_<name>`; `validate_exports` accepts it as a return (rejects as a param — params never box); `export_symbols` lists the destructor (Windows `/EXPORT`).
- **Verified** — `test_build_repr_c_enum_tagged_union_from_c_e2e` (`0 1 4242 2.5`), ASAN/LeakSanitizer-clean, `memory_sanitizer` 561 pass (no regression to the Vec boxing path).

### Slice 2b / 2c (deferred): multi-scalar and aggregate-payload variants

- **2b — multi-field scalar variants** (`Pair(i32, i32)`): occupy >1 word. Boxable in principle (a C struct member `{ int32_t _0; int32_t _1; }` at the word offsets), but the field-width/word-packing needs its own ABI verification. Rejected today.
- **2c — aggregate payloads** (`Data(String)`, `Node(Vec[i64])`): no by-value C form at all — the word holds a `{ptr,len,cap}` sub-aggregate. Stays rejected regardless (same reason those aren't by-value Path-B params).

## Recommendation (updated)

Slices 1 + 2a are shipped and cover the overwhelmingly common cases — status/kind codes and `Option`/`Result`-over-scalars. **Revisit 2b only if a concrete consumer needs a multi-scalar-payload variant across C**; a `#[repr(C)]` struct with a tag field already covers it without new codegen. 2c is a permanent reject (aggregate-by-value has no C form). Confirm both shipped slices' ABI on an Apple/AArch64 runner (the 5-target CI matrix would do this).
