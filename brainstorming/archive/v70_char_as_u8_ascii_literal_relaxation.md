# v70 — Char→u8 ASCII-literal cast relaxation: design re-examination

**Status:** Open brainstorming, 2026-05-21. The `'x' as u8` rejection (shipped per `phase-8-stdlib-floor.md` line 230, error code `E_CHAR_AS_NARROW_INT`, fix-it `c as u32 as uN`) has surfaced as a recurring ergonomic friction in the kata corpus: every byte-stream kata pays the documented escape-hatch tax plus a multi-line comment explaining why. The question this v-item frames: is the friction strong enough to flip a shipped, structurally consistent design call, or is the friction acceptable as the price of a coherent rule family?

This is not a deferral and not a tracker entry. The rejection is correct as shipped; this entry exists so the *next* discussion of the question — whether triggered by a new kata, a refactor-noise complaint, or an adjacent design call landing — starts from a built framing rather than from scratch.

---

## Current state

The rule and its motivation, recorded at `phase-8-stdlib-floor.md` line 230 (slice 1 of v60 item 49, `[x]` shipped):

> `char as u32` and `char as i32` produce the Unicode scalar value (range `0..=0x10FFFF`, fits in 21 bits); codegen emits `zext` from the `i32`-internal char representation. Wide-target casts accepted as of slice 1; narrow-target casts (`char as u8/u16/i8/i16`) rejected via `E_CHAR_AS_NARROW_INT`.

The rejection is one of a four-error family designed together (line 222):

| Code | Pattern | Fix-it |
|---|---|---|
| `E_CHAR_AS_NARROW_INT` | `char as u8/u16/i8/i16` | `c as u32 as uN` (explicit truncation) OR `c.encode_utf8(buf)` (proper UTF-8) |
| `E_INT_AS_CHAR` | `iN/uN as char` | `char.try_from(n)` |
| `E_INT_AS_BOOL` | `iN/uN as bool` | `n != 0` |
| `E_FLOAT_AS_BOOL` | `f32/f64 as bool` | (meaningless — no fix-it) |

The family's design intent is "narrow-target casts that may lose information require explicit acknowledgment of the loss." For `char as u8`, the loss case is real: a non-ASCII codepoint silently truncates to its low byte, which is almost always a bug (UTF-8 multi-byte encoding vs codepoint-low-byte confusion).

The fix-it `c as u32 as uN` is a no-op intermediate: the `u32` cast can't lose information (codepoint fits in 21 bits), and the `u32 → u8` cast does the truncation the user is now explicitly requesting. The intermediate step is the acknowledgment.

---

## Kata-corpus evidence

Three katas pay the double-cast tax today, each ~2 LOC of cast plus a 5-line comment block explaining why:

- `kara-katas/leetcode/1-100/8-string-to-integer-atoi/atoi.kara` (the original explanatory comment block, ~10 lines)
- `kara-katas/leetcode/1-100/65-valid-number/valid.kara` (5 char constants, no comment — relies on prior-art familiarity)
- `kara-katas/leetcode/1-100/91-decode-ways/decode_ways.kara` (1 char constant + 4-line comment cross-referencing design.md)

The recurring shape:

```kara
let zero: u8 = '0' as u32 as u8;
let nine: u8 = '9' as u32 as u8;
// [4–8 lines explaining why the chain is required, often cross-referencing
//  design.md or a prior kata]
```

The cast targets are always ASCII (`'0'..'9'`, `'+'`, `'-'`, `'.'`, `'e'`, `'E'`, `' '` — all single-byte codepoints). The compiler knows this at compile time when the source is a literal — the codepoint is right there in the AST — but the cast-resolution rule treats `char as u8` as uniformly suspicious regardless of source.

---

## Options

Three relaxation shapes, with successively wider blast radius.

### Option A: Relax for char-literal sources only

Compile-time check at cast resolution: when the source is a `char` literal (`'x' as u8`), evaluate the codepoint. If ≤ 0xFF, accept the cast and emit `zext`. If > 0xFF, reject with a new `E_CHAR_LITERAL_NARROW_OVERFLOW` and a fix-it pointing at `c.encode_utf8(buf)`.

**Pro:**
- Eliminates the kata friction entirely. Every existing double-cast simplifies to `'x' as u8`.
- The variable case (`c as u8` where `c: char`) still rejects, preserving the family's "loss requires acknowledgment" intent for the case where the compiler genuinely can't see the value.
- Locally checked at the cast site — typechecker's cast-resolution step already has the source AST in hand.
- No new effects, no new error infrastructure beyond one new code.

**Con:**
- Creates a syntactic asymmetry: `'x' as u8` works, `let c = 'x'; c as u8` doesn't. Refactor noise: extract a constant → suddenly need the double-cast back.
- Sets a precedent: arguably `'x' as i8` (signed narrow) needs the same relaxation for ASCII chars with codepoint ≤ 127. Once accepted for `i8`, the question for `u16` / `i16` (narrow-Unicode) follows. Either the relaxation stops at one cast-pair (arbitrary) or it propagates through the family (Option B).

### Option B: Relax for char-literal sources across the narrow-int family

Same as A, but extend to `i8`, `u16`, `i16` when the literal's codepoint fits the target's representable range. Eliminates the family-asymmetry concern internal to Option A.

**Pro:**
- Internally consistent — one rule, four cast pairs, same compile-time-check structure.
- Refactor symmetry between `'x' as u8` and `'x' as i8` for ASCII chars.

**Con:**
- Wider diagnostic surface — `E_CHAR_LITERAL_NARROW_OVERFLOW` needs target-specific range messages (e.g., `'\u{0080}' as i8`: "127 max", `'\u{0100}' as u8`: "255 max").
- `'A' as i8` and `'A' as u16` are unmotivated by current workloads; the relaxation lands surface that may stay unused.
- Doesn't address the literal-vs-variable asymmetry from Option A; only closes the within-family asymmetry.

### Option C: Const-eval-driven relaxation, no special-case for literals

Generalize: any `char` cast whose source value is provable at compile time and fits the narrow target is accepted. Source can be a literal (`'x'`), a `const` binding (`const ZERO: char = '0';`), a const-eval expression (`if cfg!(...) { '0' } else { 'a' }` once such expressions are const), or any future expansion of the const-eval surface.

**Pro:**
- Most general — covers `const ZERO: char = '0'; ZERO as u8` and similar refactor-friendly patterns. The literal-vs-variable asymmetry of Options A/B disappears for *const* variables.
- Aligns with the existing const-eval machinery (const-generic args, array sizes — already in the typechecker).
- The diagnostic naturally explains itself: "this value is `'\u{0100}'` (256), outside `u8` range" rather than "char-to-narrow casts require explicit truncation."

**Con:**
- Const-eval availability at cast-resolution time is a sequencing question. The cast-resolution step runs before some const-eval can complete (e.g., const fn results that depend on other items). May require restructuring the type-check ordering.
- The runtime-variable case (`let c = some_io.read_char(); c as u8`) still rejects. The asymmetry moves from "literal vs let" to "const-eval-able vs runtime" — narrower but still present.
- Largest implementation surface; needs a clear rejection diagnostic for sources that *look* compile-time-evaluable but aren't (yet) reachable by the const-eval pass.

---

## Decision criteria

The question isn't "which option" — it's "is the friction strong enough to flip the shipped call." Suggested triggers for promoting this v-item to a tracker entry:

1. **Friction threshold by kata count.** N≥5 katas pay the double-cast tax. (Current: N=3.)
2. **Refactor-noise complaint.** Someone hits the `let c = 'x'; c as u8` asymmetry and writes a real bug or readability regression report. (Current: no instances.)
3. **Adjacent design call lands.** If the const-eval machinery used for const-generics extends naturally to cast sources, Option C becomes nearly free. The promotion criterion then shifts from "is the friction worth it" to "is the surface coherent without it."
4. **Public-facing surface friction.** A blog post / talk demo where `'x' as u32 as u8` reads as language-design noise rather than as the deliberate safety mechanism it is. (Current: no instances.)

If/when any trigger fires, the canonical action is: promote one of the three options to a `[ ]` entry under `phase-8-stdlib-floor.md` § Cast-pair rules (line 220 region), and move this file to retain the framing as prior-art reference.

---

## What this is *not*

- **Not a deferral.** No commitment to land any of A / B / C. The rejection is shipped and structurally consistent; this file frames the question, not the answer.
- **Not breaking.** Any relaxation here is purely additive — every program that compiles today (with the double-cast workaround) keeps compiling after relaxation. Sequencing is independent of v1.
- **Not a tripwire elsewhere.** Per the project's brainstorming/canonical-doc model, the workload itself is the tripwire — every future byte-stream kata that needs the double-cast is an automatic re-encounter with this question. No `MEMORY.md` cross-reference, no scheduled review, no calendar.
- **Not a critique of the original decision.** v60 item 49 designed the four-error family as a coherent unit; the rejection of `char as u8` follows from that family logic, not from an oversight. The question this v-item raises is whether the literal-source case is a coherent carve-out *within* that family, not whether the family is wrong.
