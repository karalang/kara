# Appendix D: Attributes

Attributes are metadata attached to declarations. Two syntactic forms are supported:

```kara,ignore
#[attribute_name]               // marker
#[attribute_name(arg, ...)]     // with arguments: positional, or `key: value`
```

Attributes appear immediately before the item they annotate. An attribute
path has one segment (`#[derive]`, `#[must_use]`), except for the compiler's
own `karac::` and `diagnostic::` namespaces. A bare name the compiler does not
know is `error[E_UNKNOWN_ATTRIBUTE]`, and so is a path in any other namespace
(`#[karafmt::skip]`): tool namespaces come later. The authority is
design.md § Attributes, whose list is generated from the compiler's registry.

---

## Derive

### `#[derive(Trait, ...)]`

Generates trait implementations for the annotated `struct` or `enum`. See [Appendix C](appendix-c-derivable-traits.md) for the full list of derivable traits and their dependencies.

```kara
#[derive(PartialEq, Eq, Hash, Display, Clone)]
struct UserId { value: u64 }
```

### `#[default]`

Marks the variant `#[derive(Default)]` uses on an enum. Exactly one unit variant carries it.

---

## Lint control

### `#[allow(lint_name)]`

Suppress a specific lint within the annotated item. The lint fires nowhere inside the item.

### `#[warn(lint_name)]`

Ensure a lint is at warning level even if it would otherwise be suppressed.

### `#[deny(lint_name)]`

Promote a lint to a hard error within the annotated item.

### `#[expect(lint_name)]`

Suppress a lint and check that it fired: if it fires nowhere in the item,
`unfulfilled_lint_expectation` warns at the attribute.

Written as an inner attribute among a file's first lines, `#![allow(lint_name)]`
sets the level for the whole file.

**Some lint names** (`karac lint --list` prints the full registry, with each
lint's default level):

| Lint name | Default | What it checks |
|-----------|---------|----------------|
| `undocumented_unsafe` | warn | An `unsafe { }` block without a `// Safety:` comment, or an `unsafe extern` block without a `# Safety` doc comment |
| `ffi_float_eq` | warn | Comparing an `extern "C"` float return with `==` or `!=` |
| `redundant_suffix` | warn | A literal suffix that matches the default type (`42i64`) |
| `acronym_case` | warn | A Type name that writes an acronym in capitals (`HTTPClient`) |
| `prelude_shadow` | warn | A declaration that takes a prelude type's name |
| `must_use` | warn | A discarded `Result`, or a value of a `#[must_use]` type |
| `deprecated` | warn | A use of a `#[deprecated]` item, or of a form scheduled for removal |
| `mutual_recursion_note` | warn | A mutual-recursion group whose effects are computed together |
| `redundant_ref_param` | allow | A redundant `ref` on a parameter (a style lint: off until turned on) |

---

## Memory layout

### `#[repr(C)]`

On a `struct`: lay out fields in C ABI order (declaration order, with C padding rules). Required for types passed through `extern "C"` boundaries.

### `#[repr(u8)]`, `#[repr(i32)]`, ...

On an enum: fix the discriminant's integer type.

### `#[repr(transparent)]`

On a single-field wrapper: the same layout as its field.

`#[repr(packed)]` and `#[repr(align(N))]` are not in v1: the compiler refuses
them (`E_REPR_DEFERRED`). They belong to the systems track, with linker
control (`#[unsafe(no_mangle)]`, `#[unsafe(link_section("..."))]`, `#[used]`),
`#[thread_local]` and per-function `#[profile(...)]` checks (deferred.md).

---

## FFI

### `#[kara_name("identifier")]`

On an `extern` item: rebinds a non-conforming foreign name to a valid Kāra identifier. The Kāra-visible name must follow the identifier case-class rules; the foreign name may be arbitrary ASCII.

```kara
/// # Safety
/// `GLXFBConfig` is an opaque handle owned by the X server; it is only passed
/// back to the GLX functions that produced it.
unsafe extern "C" {
    #[kara_name("GlxFbConfig")]
    type GLXFBConfig;
}
```

---

## Targets

### `#[target(...)]`

Gates an item to the named targets (`#[target(native)]`). It is the 0.8
spelling; the general `#[cfg(...)]` is not in 0.8, and the compiler refuses it
and names `#[target]`. See [Server-Side Rendering](ch17-ssr.md) for where it
belongs: on entry points, not inside components.

---

## Functions

### `#[no_effect(VERB, ...)]`

On a function: asserts the named effects are **absent** from its transitive effect set. The arguments are effect verbs in the same grammar a `with` clause uses. It is what makes a default-permitted effect such as `allocates(Heap)` or `panics` checkable on one function.

A bare verb forbids every occurrence of it; a verb with a resource narrows it to that resource, so `#[no_effect(allocates(Heap))]` still permits `allocates(Arena)`. A forbidden effect in the declared or inferred set is `error[E_NO_EFFECT_VIOLATED]`. The attribute is `fn`-only (`error[E_NO_EFFECT_INVALID_TARGET]` elsewhere), and an empty list is rejected rather than treated as a vacuous guarantee.

```kara
#[no_effect(allocates(Heap), panics)]
fn mix(a: f32, b: f32) -> f32 {
    a * 0.5 + b * 0.5
}
```

### `#[must_use]`  /  `#[must_use("reason")]`

On a **type**: every binding site where a value of this type would be silently dropped produces a warning. Use for types that must be explicitly handled (e.g., a connection that must be closed).

On a **function**: the return value must not be silently discarded. `Result` return values are implicitly `#[must_use]`.

```kara,ignore
#[must_use("connections must be explicitly disconnected")]
struct Connection { ... }
```

### `#[track_caller]`

A panic inside the function reports its caller's location, as `unwrap()` does.

### `#[panic_handler]`

Marks the program's one custom panic handler ([Chapter 20](ch20-debugging-crashes.md#running-code-on-a-panic)).

### `#[deprecated]`, `#[unstable]`, `#[non_exhaustive]`

`#[deprecated(since: "1.2.0", note: "...")]` warns at every use of an item
scheduled for removal; `#[unstable]` makes a use require
`#[allow(unstable_api)]`; `#[non_exhaustive]` lets a public type gain variants
or fields, so other packages need a wildcard arm or `..`.

---

## Testing

Tests are `test "name" { ... }` blocks ([Chapter 16](ch16-testing.md)), not
attributed functions: `#[test]` on a `fn` is
`error[E_TEST_ATTR_INVALID_TARGET]`. Three attributes go on a test block:

### `#[with_provider(Resource, provider)]`

Runs the test body inside `with_provider[Resource](provider, || body)`, with the
provider built afresh for the case. Several stack, outermost first.

### `#[test(requires: [Resource, ...])]`

Marks a test that needs a live external resource. When the resource is
unavailable, the test is skipped (or fails with `reason: "unsatisfied_requires"`
when `karac test --all` is used).

### `#[test(timeout_seconds: N)]`

Bounds the case's run time; without it the limit is `kara.toml`'s
`[test] timeout_seconds`, else 30 seconds.

---

## The `diagnostic::` namespace

Trait authors can shape the error a failed bound produces:

```kara,ignore
#[diagnostic::on_unimplemented(
    message: "the type `{Self}` cannot be used as a map key",
    label: "`{Self}` is not Hash",
    note: "derive it with `#[derive(Hash)]` or write an impl"
)]
trait Hash { ... }
```

`#[diagnostic::do_not_recommend]` on an `impl` keeps it out of the
"the trait is implemented by ..." suggestions. Both are advisory: an unknown
name in this namespace is accepted silently, and a malformed one warns
(`malformed_diagnostic_attribute`) rather than failing the build.
