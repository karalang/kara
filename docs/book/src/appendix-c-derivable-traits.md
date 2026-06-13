# Appendix C: Derivable Traits

`#[derive]` is a compiler built-in that generates trait implementations mechanically from a type's fields or variants. You list the traits you want derived in one attribute:

```kara
#[derive(PartialEq, Eq, Hash, Display)]
struct GridCell {
    row: i64,
    col: i64,
}
```

(Note the field types are integers. `Eq` and `Hash` require *every* field to be `Eq`/`Hash`, and `f64` is neither — `NaN != NaN` breaks reflexivity — so a struct with `f64` fields can derive `PartialEq`/`PartialOrd`/`Display` but **not** `Eq`/`Ord`/`Hash`. See the per-trait requirements below.)

The compiler resolves derive dependencies automatically regardless of the order you list them. Writing `#[derive(Hash)]` when `PartialEq` and `Eq` are not yet derived causes the compiler to derive them first, in the correct order.

---

## Equality and ordering

### `PartialEq`

Generates `==` and `!=` by comparing fields pairwise in declaration order. For enums, first checks that the variants match, then compares fields.

No dependencies.

### `Eq`

A marker trait indicating that `==` is a total equivalence relation (reflexive, symmetric, transitive, and always defined). Adds no method body — it is a promise to the type system.

**Requires:** `PartialEq`

### `PartialOrd`

Generates `<`, `<=`, `>`, `>=` with lexicographic field-order comparison. Returns `Option[Ordering]` because `NaN != NaN` for floats; for types without `NaN`, every comparison returns `Some(...)`.

**Requires:** `PartialEq`

### `Ord`

Total ordering: every pair of values is comparable. Generates a complete lexicographic comparison in declaration order.

**Requires:** `PartialOrd + Eq`

---

## Hashing

### `Hash`

Generates a `hash` method that feeds each field into a `Hasher`. Used by `Map` and `Set` as key types.

**Requires:** `Eq` (reflects the consistency contract: `a == b` must imply `hash(a) == hash(b)`)

---

## Display and debugging

### `Display`

Generates a human-readable string representation.

- **Structs:** emits `TypeName { field: value, ... }` for `pub` fields. Use `#[derive(Display(all_fields))]` to include private fields.
- **Enums:** emits the variant name. Use `#[derive(Display(snake_case))]` to emit in `snake_case` instead of the declared `PascalCase`. For variants with data, appends the fields in parentheses.

Use `Display` for values shown to end users. Implement it manually to override the generated representation.

No dependencies.

### `Debug`

Generates a developer-oriented representation. Used by `{expr:?}` in interpolated strings and by the test runner when printing unexpected values.

- **Structs:** always includes all fields, regardless of visibility.
- **Enums:** includes variant name and all fields.

No dependencies.

---

## Default values

### `Default`

Generates a `T.default()` method that returns a "zero-like" value for the type. The derived implementation calls `.default()` on each field in declaration order and constructs the struct. For enums, the first declared variant is used, with each of its fields defaulted.

```kara
#[derive(Default)]
struct Config {
    timeout_ms: i64,   // defaults to 0
    retries: i64,      // defaults to 0
    verbose: bool,     // defaults to false
}

let cfg = Config.default();
```

**Requires:** every field must also implement `Default`.

---

## Copying

### `Clone`

Generates a `.clone()` method that produces a deep copy of the value. For reference-semantics (`shared`) types, cloning produces a new RC handle, not a new heap allocation.

No dependencies.

### `Copy`

Marks a type as trivially copyable (bitwise copy semantics). Assignment and passing to functions copy the value silently instead of moving it. All primitive types are `Copy`.

**Requires:** every field of the type must also be `Copy`.

**Auto-derives `Clone`:** `#[derive(Copy)]` automatically adds `Clone` if not already present.

---

## Arithmetic on distinct types

### `Arithmetic`

Available on `distinct` (newtype) types only. Generates `+`, `-`, `*`, `/`, `%` by forwarding to the underlying type's operations and wrapping the result back in the newtype. Without this derive, arithmetic between two values of the same distinct type is a type error (intentional: distinct types are supposed to be incompatible units).

```kara
#[derive(Arithmetic)]
distinct type Metres = f64;   // now Metres + Metres → Metres
```

**Only valid on `distinct` types.**

---

## Serialization (post-v1)

### `Serialize`

Generates a `serialize` method that visits each field in declaration order via a `Serializer`. Format backends (`Json`, `Toml`, `MessagePack`, etc.) implement `Serializer` — the derived code is format-agnostic.

### `Deserialize`

Generates a `deserialize` static method that reconstructs the type field-by-field from a `Deserializer`.

Field-level attributes control serialization behavior:

```kara
#[derive(Serialize, Deserialize)]
struct Config {
    host: String,
    #[serde(rename = "port_number")]
    port: u16,
    #[serde(skip)]
    internal_flag: bool,
}
```

Supported field attributes: `rename`, `skip`, `skip_serializing`, `skip_deserializing`, `default`.

**Note:** `Serialize` and `Deserialize` are post-v1. The derive syntax and field attributes are reserved now.

---

## Dependency summary

| Trait | Auto-derives | Requires |
|-------|-------------|---------|
| `PartialEq` | — | — |
| `Eq` | — | `PartialEq` |
| `PartialOrd` | — | `PartialEq` |
| `Ord` | — | `PartialOrd + Eq` |
| `Hash` | — | `Eq` |
| `Display` | — | — |
| `Debug` | — | — |
| `Clone` | — | — |
| `Copy` | `Clone` | every field is `Copy` |
| `Default` | — | every field is `Default` |
| `Arithmetic` | — | type must be `distinct` |
| `Serialize` | — | — |
| `Deserialize` | — | — |
