# Appendix D: Attributes

Attributes are metadata attached to declarations. Two syntactic forms are supported:

```kara,ignore
#[attribute_name]               // marker
#[attribute_name(arg, ...)]     // with arguments
@attribute_name                 // shorthand marker (selected attributes only)
```

Attributes appear immediately before the item they annotate.

---

## Derive

### `#[derive(Trait, ...)]`

Generates trait implementations for the annotated `struct` or `enum`. See [Appendix C](appendix-c-derivable-traits.md) for the full list of derivable traits and their dependencies.

```kara
#[derive(PartialEq, Eq, Hash, Display, Clone)]
struct UserId { value: u64 }
```

---

## Lint control

### `#[allow(lint_name)]`

Suppress a specific lint within the annotated item. The lint fires nowhere inside the item.

### `#[warn(lint_name)]`

Ensure a lint is at warning level even if it would otherwise be suppressed.

### `#[deny(lint_name)]`

Promote a lint to a hard error within the annotated item.

**Available lint names:**

| Lint name | Default | What it checks |
|-----------|---------|----------------|
| `undocumented_unsafe` | warning | Every `unsafe { }` block must be preceded by a `// Safety:` comment |
| `ffi_float_eq` / `ffi_float_eq` | warning | Comparing an `extern "C"` float return with `==` or `!=` |
| `redundant_suffix` | warning | Literal suffix that matches the default type (e.g., `42i64`) |
| `mutual_recursion_note` | note | Note when the SCC pass detects a mutual-recursion group |
| `module_mut_binding` | warning (`lib` profile) | `let mut` at module scope |
| `layout_unassigned_fields` | warning | Fields not assigned to a `group` in a `layout` block |
| `repr_c_layout_ignored` | warning | `layout` block on a private struct (has no FFI effect) |
| `rc_fallback` | note | Compiler chose RC tier to satisfy ownership analysis |

---

## Safety

### `#[noblock]`  /  `@noblock`

On an `extern "C"` or `extern "C-unwind"` function: removes `blocks` from the default effect set. Use this for pure-CPU foreign functions (math routines, `strlen`, etc.) that are known not to block.

```kara,ignore
@noblock
extern "C" fn sqrt(x: f64) -> f64;
```

---

## Linker control

### `#[unsafe(no_mangle)]`

Use the Kāra identifier as the exported symbol name without any name mangling. Required when a foreign caller (C, linker script, debugger) must reference the symbol by its exact Kāra name. Does not imply `extern "C"` — the calling convention is independent.

The `#[unsafe(...)]` wrap is mandatory: disabling name mangling can collide with foreign symbols, an obligation the compiler cannot verify. Bare `#[no_mangle]` is rejected at parse time.

```kara,ignore
#[unsafe(no_mangle)]
pub fn kara_entry() { ... }
```

### `#[used]`

Prevent dead-code elimination for the annotated symbol even if no Kāra code references it. Use for linker-section entries, interrupt vectors, or other symbols that are referenced only from outside the compiler's visibility (linker scripts, hardware, debuggers). Stays plain (no `#[unsafe(...)]` wrap) — `#[used]` only suppresses DCE, no soundness obligation.

```kara,ignore
#[unsafe(link_section(".vectors"))]
#[used]
let interrupt_table: [fn(); 16] = [...];
```

### `#[unsafe(link_section("name"))]`

Place the annotated symbol in a named linker section. Required for embedded targets that map specific sections to specific memory regions (flash, DTCM RAM, etc.).

The `#[unsafe(...)]` wrap is mandatory: section placement carries layout and aliasing obligations the compiler cannot verify. Bare `#[link_section(...)]` is rejected at parse time.

```kara,ignore
#[unsafe(link_section(".dtcmram"))]
let fast_buffer: [u8; 1024] = [0; 1024];
```

---

## FFI

### `#[kara_name = "identifier"]`

On an `extern` item: rebinds a non-conforming foreign name to a valid Kāra identifier. The Kāra-visible name must follow the identifier case-class rules; the foreign name may be arbitrary ASCII.

```kara,ignore
#[kara_name = "GlxFbConfig"]
extern type GLXFBConfig;
```

---

## Module-level bindings

### `#[thread_local]`

On a module-level `let mut` binding: gives each OS thread (and each task under the runtime) its own independent copy. The binding's initializer must still be a compile-time constant.

```kara,ignore
#[thread_local]
let mut request_count: i64 = 0;
```

---

## Memory layout

### `#[repr(C)]`

On a `struct`: lay out fields in C ABI order (declaration order, with C padding rules). Required for types passed through `extern "C"` boundaries.

### `#[repr(packed)]`

On a `struct`: remove all padding. Fields may be unaligned — use `unsafe` for pointer access to packed fields.

### `#[repr(align(N))]`

On a `struct` or as a wrapper type: require at least `N`-byte alignment.

---

## Functions

### `#[profile(P1, P2, ...)]`

On a function: asserts that its transitive effect set is compatible with the *intersection* of the listed profiles' constraints — the function must satisfy the strictest constraint from any listed profile. The v1 profile names are `default` (forbids nothing), `embedded` (forbids `allocates(Heap)`), and `kernel` (forbids `allocates(*)`, `panics`, `blocks`, `suspends`). A forbidden effect in the function's declared or inferred set is `error[E_PROFILE_INCOMPATIBLE_EFFECT]`; an unknown profile name is `error[E_UNKNOWN_PROFILE]`.

```kara
#[profile(embedded, kernel)]
fn scale(x: i64, factor: i64) -> i64 {
    x * factor
}
```

### `#[must_use]`  /  `#[must_use = "reason"]`

On a **type**: every binding site where a value of this type would be silently dropped produces a warning. Use for types that must be explicitly handled (e.g., a connection that must be closed).

On a **function**: the return value must not be silently discarded. `Result` return values are implicitly `#[must_use]`.

```kara,ignore
#[must_use = "connections must be explicitly disconnected"]
struct Connection { ... }
```

---

## Testing

### `#[test]`

Mark a `test_`-prefixed function as a test case.

### `#[test(requires = [resource, ...])]`

Mark a test that needs a live external resource. When the resource is unavailable, the test is skipped (or fails with `reason: "unsatisfied_requires"` when `karac test --all` is used).

### `#[with_provider(resource_path, constructor_fn)]`

Supply an in-memory provider for a test. The provider scope wraps the entire test body. Multiple `#[with_provider]` attributes are allowed; source order is outer-to-inner.

---

## Tool-namespaced attributes

Multi-segment attribute paths of the form `#[TOOL::NAME(...)]` are reserved for external tools — formatters, linters, doc generators, IDE plugins, custom analyzers. The compiler accepts them syntactically, stores them on the AST, and otherwise ignores them; semantic interpretation is each tool's responsibility. The full design lives at *design.md § Tool-Namespaced Attributes*; this appendix entry catalogs the v1-reserved names and the read surface.

```kara,ignore
#[karafmt::skip]
fn manually_aligned_table() { 0 }

#[karalint::allow(complexity)]
fn complicated_inner_loop(data: ref Slice[Frame]) -> Frame {
    // ...
}

#[acmecorp_security::audit_required(level: "strict")]
pub fn login(username: String, password: String) -> Result[Session, AuthError] { /* ... */ }
```

The discriminator is structural: a *bare-name* path (`#[derive]`, `#[no_mangle]`) must match a known compiler attribute or it is `error[E_UNKNOWN_ATTRIBUTE]`; a *multi-segment* path is either a compiler-reserved namespace (`#[diagnostic::*]` — validated per *Appendix D § Diagnostic*) or a tool namespace (silently accepted). There is no per-project tool registration at v1; the open-namespace rule applies.

### v1-reserved first-party tool namespaces

The Kāra organisation reserves three tool namespaces at v1 for the canonical first-party tools that will ship post-v1. User code may write attributes against them today — they parse and store like any other tool namespace — but their semantics are defined when the corresponding tool ships, and the names will not be reused by any other tool. The reservation is a name-claim, not an implementation commitment.

#### `#[karafmt::*]`  *(post-v1, reserved)*

The canonical formatter. Initial members:

- `karafmt::skip` — on any item: suppresses formatting for that item.

Until `karafmt` ships, `#[karafmt::skip]` is functionally a no-op.

#### `#[karalint::*]`  *(post-v1, reserved)*

The canonical lint pack ride-along — separate from the compiler-built-in lints from *Appendix D § Lint control*. Initial members:

- `karalint::allow(NAME)` / `karalint::warn(NAME)` / `karalint::deny(NAME)` / `karalint::expect(NAME)` — same shape as the compiler's built-in lint attributes but scoped to lints that live in the external `karalint` package.

#### `#[karadoc::*]`  *(post-v1, reserved)*

The canonical doc generator. Initial members:

- `karadoc::hidden` — on any item: omits the item from generated docs.

### Third-party tool namespaces

Any other multi-segment path is also accepted. By convention, third-party tools use a namespace matching their package or organisation name (e.g., `acmecorp_security::audit_required`, `mytool::config(level: 9)`) to avoid collision with the reserved names above. The compiler does not enforce this convention; conflict-resolution authority is social — first registered, first served, with the v1-reserved names taking absolute precedence.

### Reading tool attributes from outside the compiler

Tools consume tool-namespaced attributes via one of three paths:

- **`karac query attributes [--tool=PREFIX]`** — emits a JSON list of every multi-segment attribute on every item, optionally filtered by first-segment prefix. `--tool=karafmt` returns every `#[karafmt::*]`. Without `--tool`, returns every multi-segment attribute (including `#[diagnostic::*]`).
- **Language Server Protocol** (post-v1) — the IDE-facing surface exposes the same data through workspace-symbol and document-symbol responses.
- **Direct AST access** — tools written in Kāra and using the compiler-as-library API read the same `Attribute { path, args, span }` structures the typechecker stores.

