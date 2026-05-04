# v61 — design.md Rust-reference sweep

Scope: a pass over every `Rust` mention in `docs/design.md` to make the spec self-contained — Kāra's design should stand on its own definitions, not lean on reader familiarity with Rust as a prerequisite.

Spawned out of v59 F2 (which did the minimal fix on line 5990 in self-contained style). v59 F2 set the precedent; v61 applies it across the whole doc.

## Lens

A canonical spec should describe what the language *is*, not what it *inherits*. If a Kāra concept needs "Rust does it this way" to be understandable, the definition isn't self-contained yet — that's a defect, not an aid. design.md is read by:

1. **Reviewers (LLM and human)** working through it linearly. Every Rust shibboleth is a friction point for non-Rust reviewers; a self-contained definition serves everyone.
2. **Implementers** of the compiler, who need precise rules — not "see Rust's behavior" handwaves.
3. **Future readers** (5+ years out), when the obvious-reference-language may have shifted.

The current saved feedback memory `feedback_llm_written_human_reviewed.md` already lands here for syntax decisions; v61 extends the same lens to spec prose.

**Where comparisons stay:** when the *contrast* is the rationale (e.g., "no user-defined `Deref` because user-defined deref forces unbounded chain walking"), the comparison can be rewritten generically — "languages with user-defined deref walk an unbounded chain..." — without naming Rust. When prior-art citation is the point (e.g., the Maranget exhaustiveness algorithm is shared across Rust/OCaml/Scala), citing the multi-language family is fine; that's not Rust-anchoring, it's establishing prior-art breadth.

The single explicit-lineage exception: the **Inspirations** table near the end of design.md (line 9696) lists Rust as one of several explicit influences. That row stays — it's where lineage *belongs*. Stripping Rust from the spec body lets the inspirations row do its job: name the influences once, then write the spec on its own terms.

---

## False positives in the grep

`grep -i "rust"` matches `trust` as a substring, producing ~18 false positives (lines 3064, 4374, 4375, 4612, 5635, 8657, 8844, 9103, 9116, 9120, 9128, 9149, 9185 and a few more). These are unrelated to the sweep. Real `Rust` mentions: ~58.

---

## Disposition categories

**A. STRIP** — drop the Rust mention; meaning preserved without rewriting. Casual "Rust-style" / "Matches Rust" / "Same as Rust." anchors. *Lowest-cost edits.*

**B. STRIP-FROM-MULTI-LANG** — line cites Rust alongside other languages (e.g., "Matches Rust / C / Java / Go"). Drop just the Rust token; keep the multi-language anchor.

**C. REPHRASE COMPARISON-AS-RATIONALE** — the comparison is doing real argumentative work. Rewrite generically ("languages with X do Y") so the rationale survives without naming Rust.

**D. KEEP — load-bearing precedent reference.** Concrete API names (`OnceLock`, `set_hook`), tool comparisons (`rustfmt`, `rust-analyzer`), or architecture parallels (`core`/`alloc`/`std` split) where the named-reference *is* the value. These are pointers, not crutches.

**E. KEEP — Inspirations table row.** Single occurrence (line 9696). The lineage row is the one place naming Rust is the right thing.

---

## Summary table

| Disposition | Count | Effort       |
|-------------|-------|--------------|
| A. STRIP                              | ~20   | mechanical |
| B. STRIP-FROM-MULTI-LANG              | ~11   | mechanical |
| C. REPHRASE COMPARISON-AS-RATIONALE   | ~12   | judgment   |
| D. KEEP — load-bearing precedent      | ~14   | none       |
| E. KEEP — Inspirations row            | 1     | none       |

---

## Per-line dispositions

### A. STRIP (drop the Rust token; meaning preserved)

| Line | Current snippet | Action |
|------|-----------------|--------|
| 247  | "Enums — algebraic data types with exhaustive pattern matching (Rust-style)" | Drop "(Rust-style)" |
| 249  | "methods with Rust-style syntax, UFCS bridge..." | Drop "Rust-style " |
| 1502 | "**Literal suffixes.** Rust-style suffixes force the type..." | Drop "Rust-style " |
| 2056 | "This matches Rust's model and ensures no intermediate allocations..." | Drop "Rust's model and " (keep "ensures...") |
| 2405 | "...trait `m` candidates at the same level are not collected. Same rule as Rust." | Drop final sentence |
| 3420 | "**`b'A'` has type `u8`.** A single byte. Matches Rust exactly." | Drop "Matches Rust exactly." |
| 4088 | "This is the same split Rust uses, and for the same reason..." | Reword: "The split exists because composing the hash of a struct should not require..." |
| 4125 | "This matches Rust's split and keeps per-hash state..." | Drop "matches Rust's split and " |
| 4273 | "Use a wrapper type with `#[repr(align(N))]` as the field type — the same pattern as Rust." | Drop "— the same pattern as Rust" |
| 4455 | "These are the same semantics as Rust's `std::sync::mpsc`." | Drop entire sentence |
| 5109 | "This mirrors how `Send`/`Sync` bounds work for trait objects in Rust..." | Reword: "This is the standard treatment for trait-object effect contracts under separate compilation..." |
| 5722 | "Rust-style enums with exhaustive pattern matching:" | Drop "Rust-style " |
| 5896 | "This matches user expectation and Rust's behavior." | Drop "and Rust's behavior" |
| 5898 | "Adding a variant ... is a breaking change for any match that does not have a wildcard arm — the same source-stability trade-off Rust makes" | Drop "— the same source-stability trade-off Rust makes" |
| 5902 | "Range patterns ... work with integer and `char` types (not floats). Same decision as Rust." | Drop "Same decision as Rust." |
| 5928 | "Distinct types do not introduce a new constructor space ... Matches Rust's newtype handling." | Drop final sentence |
| 5938 | "(c) the workaround is trivial — write an unguarded `_` arm. Matches Rust." | Drop "Matches Rust." |
| 6031 | "This is the same dataflow model Rust uses, and it is necessary..." | Drop "the same dataflow model Rust uses, and " |
| 6107 | "This produces the same effect as Rust's `ref` binding in match arms." | Reword: "This binds the field as a borrow without consuming it." |
| 6109 | "This is the same as Rust's 2021 edition match ergonomics." | Drop sentence (rule that follows is self-contained) |
| 6305 | "This matches Rust's closure-capture-site dataflow and is necessary..." | Drop "matches Rust's closure-capture-site dataflow and " |
| 8276 | "**Monomorphization.** Rust's well-known pain point." | Reword: "**Monomorphization.** A known compile-time-cost pain point." |

### B. STRIP-FROM-MULTI-LANG (drop just Rust; keep other anchors)

| Line | Current snippet | Action |
|------|-----------------|--------|
| 234  | "Rust's warning-only approach and Python's socially-enforced PEP 8 both produce persistent real-world friction" | Drop Rust clause; keep Python anchor (or rewrite both as generic precedents) |
| 564  | "languages where default = private-to-module (Rust, Java, C#)" | Drop "Rust, " — keep Java, C# |
| 1108 | "the equivalent of Rust's `vec![val; n]` or Python's `[val] * n`" | Drop "Rust's `vec![val; n]` or " — keep Python comparison |
| 1519 | "Matches Rust / C / Java / Go / Swift" | Drop "Rust / " |
| 1522 | "Matches Rust / Java / C-for-signed" | Drop "Rust / " |
| 1523 | "C's undefined-behavior door and Rust-release's silent mask both do" | Drop "and Rust-release's silent mask" |
| 1525 | "Identical names to Rust because the idiom is industry-standard (Swift, Zig, and Rust all share `wrapping_*`)" | Drop "Identical names to Rust because " and "and Rust"; rewrite as "the idiom is industry-standard (Swift, Zig share `wrapping_*` / `saturating_*`)" |
| 1573 | "safer-than-Java-worse-than-Rust middle-ground" | Reword: "the implicit-widening middle-ground" |
| 4110 | "This matches Rust, Python, and modern Java" | Drop "Rust, " |
| 6765 | "consistent with Rust, Swift, Go" | Drop "Rust, " |
| 8345 | "Effects are a language differentiator that Python and Rust REPLs cannot surface" | Drop "and Rust" |

### C. REPHRASE COMPARISON-AS-RATIONALE (the comparison is the argument)

These need careful rewrites — the Rust comparison is doing real argumentative work. Plan: replace with generic phrasing ("languages with X do Y") that preserves the rationale.

| Line | Current snippet | Why it's load-bearing |
|------|-----------------|------------------------|
| 14   | "...avoids the common misreading that Kāra is 'Rust with auto-inference.'" | Deliberate disclaimer. Could keep — or reword as "...avoids the common misreading that Kāra is just an existing systems language with type inference bolted on." |
| 265  | "Bidirectional typing is what Dotty, Swift, and parts of Rust use for the same feature combination." | Multi-lang citation establishing prior art. Keep, OR drop "and parts of Rust" |
| 2381 | "Kāra commits to a four-step algorithm adapted from Rust with two intentional simplifications: no user-defined `Deref`, and no Rust-style 'bring a trait into scope to use its methods' layer." | The algorithm is concretely "the standard four-step receiver-candidate / candidate-set / autoref / coercion lookup." Rewrite without sourcing it to Rust. |
| 2385 | "**No user-defined `Deref`.** Rust's method resolution walks an unbounded deref chain because any type can `impl Deref<Target = U>`..." | Rewrite as: "Languages with user-defined `Deref` (or equivalent re-routing trait) must walk an unbounded chain..." |
| 2387 | "**Trait methods participate in lookup iff the trait is visible.** Rust requires `use Trait` to bring a trait's *methods* into scope..." | Rewrite as: "Some method-lookup designs require an explicit `use Trait` import to bring trait methods into scope even when the trait name is already visible. Kāra does not add this second layer." |
| 2463 | "**Why not 'trait methods require explicit `use`'?** Rust requires `use Trait` because its imports are lexical..." | Same rephrase as 2387 — generic "designs that require explicit method-bringing imports..." |
| 2465 | "...the soundness hazards that plagued early Rust specialization proposals." | Reword: "...the soundness hazards documented in early specialization proposals." |
| 4329 | "...a guarantee that languages without a static effect system (including Rust) cannot make without external tooling." | Drop "(including Rust)" — the general statement stands |
| 5231 | "Kāra's orphan rule matches Rust's." | Rewrite to specify the rule directly: "Kāra's orphan rule: an impl is permitted only if the trait or the receiver type is local to the package." |
| 5865 | "the same decidable algorithm that Rust, OCaml, and Scala use" | Multi-lang citation. Keep — establishes the algorithm has industry pedigree. |
| 9264 | "Anchors cleanly to the `par struct` definition site — simpler than Rust's `Send`/`Sync` because the intent is declared at the keyword level." | Rewrite: "...simpler than the alternative of separate marker traits propagated structurally, because the intent is declared at the keyword level." |

### D. KEEP — load-bearing precedent reference

These are concrete API/tool/architecture references where the *named reference* is the value. Removing them would force vague paraphrase.

| Line | Reference | Why keep |
|------|-----------|----------|
| 812  | `Cargo.toml` / `rust-toolchain.toml` separation | Concrete file-level analogy for MSRV vs toolchain pin split |
| 891  | "equivalent of Rust's `OnceLock`/`LazyLock`" | Specific stdlib primitive name; precise prior-art pointer |
| 1026 | Table column "Rust equivalent" | The comparison table is *intentionally* a Rust-equivalence table for migrating users |
| 1124 | Table column "Rust equivalent" | Same table |
| 1635 | "(same as Rust's `char`)" | Spec-precise reference: "`char` = Unicode scalar value" is unambiguous via the Rust pointer |
| 7020 | "(Rust's `std`, Go's runtime, Swift's libswiftCore)" | Multi-language citation establishing the runtime-as-library pattern |
| 7024 | "≤10% gap to hand-written Rust on compute-bound benchmarks" | Concrete benchmark target — Rust is the reference compiler for "hand-written native baseline" |
| 7028 | "mirroring Rust's `std` being written in Rust and Go's runtime being written in Go" | Self-hosting precedent — multi-language |
| 8070 | "rust-analyzer class" | Names a known LSP-architecture class precisely |
| 8210 | "per Rust's `std::panic::set_hook` precedent" | API-precedent reference for an overridable hook |
| 8224 | "Native call frames (non-Kāra C/Rust callees through FFI)" | C/Rust = concrete FFI source languages |
| 8252 | "same scope as `gofmt` or `rustfmt`" | Tool comparison; both are widely known |
| 8855 | "mirrors Rust's `core`/`alloc`/`std` split" | Architecture parallel; concrete tier names |

### E. KEEP — Inspirations row

| Line | Snippet | Why keep |
|------|---------|----------|
| 9696 | `\| **Rust** \| Ownership, enums, pattern matching, traits, \`Result[T, E]\` \|` | This *is* the place to name Rust. Stripping the spec body lets this row do its job. |

---

## Execution plan when v61 runs

1. **Confirm category dispositions in one pass.** Walk through A / B / C / D / E here, agree on bulk dispositions. Per-line questions only when judgment is needed (Category C lines 14 and 265 may want individual decisions).
2. **Apply A and B mechanically** (~31 small edits, no rewriting required).
3. **Apply C with case-by-case rewrites** (~11 lines requiring real rewrites).
4. **D and E are no-ops** by design.
5. **Final grep verification:** `grep -i "rust" docs/design.md` should show only D and E remaining (~14 + 1 lines).

Estimated time when running v61: 1.5–2 hours focused work (down from 2–4 hours unscoped, because the categorization is pre-computed).

---

## Out of scope for v61

- README.md, syntax.md, glossary.md, roadmap.md, deferred.md, brainstorming/* — only design.md is in scope here. If the lens proves valuable, a follow-up sweep can apply the same categorization to other canonical docs.
- "Inspirations" table itself: not touched. If the inspirations row's content needs updating (e.g., adding more languages, renaming "Rust" to a more specific reference), that's a separate decision.
- Code examples that happen to match Rust syntax (`Vec`, `Result`, `match`, etc.). The lens is about *prose anchoring*, not surface syntax that happens to resemble Rust.
