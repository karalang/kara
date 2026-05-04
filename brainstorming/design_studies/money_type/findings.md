# Brick #1 — Money (domain type with behavior)

**Program.** A `Money { amount, currency }` type with `add`, same-currency comparison, display formatting, and typed error for currency mismatch. Pure data + pure methods — no I/O, no injection, no effect resources.

**What this brick stress-tests.** The v44/v45 D1 question (bundled vs. separated `impl`) directly — Money has fields, methods, and trait conformance in one type. Injection machinery is deliberately absent so the comparison is about the trait/impl shape itself.

---

## Per-axis scoring

| Axis | Java | Python | Rust | Kāra (current D1a) |
|---|---|---|---|---|
| Robustness | Medium | Low | High | High |
| Locality | High | High | Medium | Medium |
| Composability | Medium | Medium | High | High |
| Ceremony | Low–Med (~62 lines) | Very low (~40) | Medium (~80) | Medium (~55) |

### Robustness
- **Java / Python** lose. Currency mismatch is `IllegalArgumentException` / `ValueError` — runtime-only, not in the signature. A caller has no type-level notice that `add` can fail.
- **Rust / Kāra** win. `Result<Money, MismatchedCurrency>` / `Result[Money, MoneyError]` puts the failure mode in the signature. The compiler forces the caller to pattern-match.
- Deliberate choice in all four: **don't** derive a total `Ord`. Lex-comparing `(amount, currency)` would silently treat USD < EUR < GBP, which is meaningless. Only Rust and Kāra can express this cleanly as a Result-returning helper; Java and Python can only throw.

### Locality
- **Java / Python** win. The whole type — fields, methods, trait conformance — is in one block. A reader opens the file and sees everything the type does.
- **Rust / Kāra** take a hit here. `struct Money { ... }` tells you zero about what traits Money satisfies. You have to scroll (or search) to find `impl Display for Money`. For this one-type file the cost is trivial. For a module with 5+ trait impls scattered across files, it compounds.
- **Mitigation in Rust/Kāra:** `#[derive(...)]` *is* bundled at the struct — `PartialEq`, `Eq`, `Hash`, `Debug`, etc. are visible at the definition site. The locality cost only applies to hand-written `impl Trait for Type` blocks.

### Composability
- **Rust / Kāra** win. Moving `impl Display for Money` to another file is mechanical — nothing else changes, no inheritance chain to update, no bundled class body to re-layout. Retroactive impls (`impl Display for SomeoneElsesType`) work.
- **Java** medium. Class-bundled conformance can't be added retroactively; you'd need a wrapper or a separate utility class. Class hierarchies interact with trait conformance in subtle ways.
- **Python** medium. Decorators stack but can conflict; dynamic dispatch only.

### Ceremony
- **Python** lowest — `@dataclass(frozen=True)` + `@total_ordering` do most of the work.
- **Java** low if you accept boilerplate equals/hashCode; could be ~20 lines as a `record` (Java 16+).
- **Rust** highest — 4 separate `impl` blocks + a standalone error struct + its own `Display` + `Error` impls. Every piece is justified by a robustness or composability win, but it adds up.
- **Kāra** comparable to Rust, slightly shorter because `MoneyError` is one enum variant rather than a struct + error trait impl. Still separated-style, still multi-block.

---

## What Kāra already gets right

1. **Typed errors.** `MoneyError.MismatchedCurrency(Currency, Currency)` beats `IllegalArgumentException` on robustness — the caller can't ignore it.
2. **Derive bundles the easy cases.** `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]` is always visible at the struct. The D1 tension only applies to hand-written `impl Trait for Type`.
3. **Mechanical refactor.** Separated impl blocks move between files without touching the struct. Retroactive impls are natural.

## Where Kāra has friction

1. **Conformance not visible at struct site.** A reader of `struct Money { ... }` doesn't know Money implements Display without searching. Bounded cost for small files; compounds as types grow trait conformances.
2. **Two visually-similar block kinds.** `impl Money { ... }` (inherent) and `impl Display for Money { ... }` (trait) look alike but mean different things. No one coming from Java/Kotlin naturally reads this pattern.

Neither is catastrophic. Both are real.

---

## Provisional design implication

For this brick alone, **current D1a is adequate, not excellent**. The friction (conformance-not-at-struct-site, two impl flavors) is real but bounded; the wins (typed errors, mechanical refactor, retroactive impls) clearly justify the ceremony.

**Not enough signal to decide D1.** Domain types with 1–2 trait impls aren't where the separated form stresses a reader. Move on to bricks where:

- The type has 5+ trait conformances (stresses locality more)
- Conformance is retroactive (only separated form handles it)
- Multiple types in a graph (`Order` → `LineItem` → `Money`) live across files (stresses module organization)

**Recommendation:** continue to brick #2 (`http_api_call`) before revisiting D1. If subsequent bricks don't shift the scoring, close v44 at **D1a + status quo**. If brick #3 (parallel fanout) surfaces a composability issue, revisit.

---

## Notes for later bricks

- **Tests on pure-data types are trivial.** `money_test.kara` exercises `#[test]` with no `#[with_provider]` — no injection machinery is forced. This confirms the injection triangle is genuinely elective, not structural.
- **Java's `record` + `sealed` pattern** (post-16) closes much of the ceremony gap with Rust/Kāra for this brick. Worth noting because it weakens "bundled-only languages are forced to be verbose" as a Kāra-side argument.
- **Python's decorator approach** is worth watching as an LLM-friendliness data point. `@total_ordering` is a tiny amount of text that implies a lot; a similar decorator in Kāra (`#[auto_ord(by = amount)]`?) could cut ceremony without changing the separated-impl fundamentals.
