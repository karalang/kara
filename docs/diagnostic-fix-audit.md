# Diagnostic fix-invariant audit

**Date:** 2026-07-06 · **Method:** 4 parallel read-only audits, one per phase family, against a single bucketing rubric. · **Spine:** the code→kind map in `src/cli.rs` (~lines 2790–3300).

## The invariant this measures

The flagship design lever for Kāra — "the compiler auto-corrects what LLMs get wrong" — becomes a *design principle* rather than a feature only if it is a compiler-wide invariant:

> **Every diagnostic either carries a machine-applicable fix, or carries a machine-readable reason it can't (`needs-human-decision`) with the repair options enumerated.**

Under that rule you may not add a bare error message: an error is incomplete until it emits either an auto-edit or a structured set of alternatives. This document measures how far the compiler is from that rule today, and names the cheapest path toward it.

## Bucketing rubric

Each diagnostic variant is assigned exactly one bucket:

- **A — carries a fix today.** An emission site attaches a machine-applicable edit that appears in the JSON diagnostic. Split into **A✓** (also applied by `karac fix`) and **A⚠** (emitted to JSON but silently dropped by the `karac fix` applier).
- **B — could carry a fix.** No fix today, but the repair is a *single unambiguous mechanical edit fully determined by information the compiler already has* (insert a missing token, rename to a unique candidate, delete a stray attribute, add `mut`, change `self`→`ref self`, add the exact effect the checker inferred).
- **C — needs a human.** The repair requires a decision the compiler cannot make (choose among several candidate types/methods, restructure logic, pick intent). Under the invariant these carry structured `alternatives`, not an auto-edit.

## Verified applier paths

`karac fix` (`cmd_fix`, `src/cli.rs` ~9631) collects, and therefore applies:

| Source | Field | Applied? |
|---|---|---|
| Parser | `fix_edits` (span-keyed) | **yes** (~9642) |
| Resolver | `.replacement` | **yes** (~9654) — did-you-mean family now populated (`76be2de` for E0100/E0104/import; `830831f` for E_MODULE_BINDING_NAMING const-rename, B-2026-07-06-3) |
| Effect | `.replacement` | **yes** (~9661) — E0412 flows |
| Ownership | `.replacement` | **yes** (~9668) — N0507 flows |
| Ownership | `error_fix_diffs` / `fix_diff` | **yes** (~9680, `0f21b4b`) — flattened into the edit set alongside `.replacement`; was dropped, fixed under B-2026-07-06-4 |
| Typechecker | `fix_it` | **yes** (~9680) |

## The number

Across **~133 distinct diagnostic variants**:

| Bucket | Count | Share |
|---|---|---|
| **A — carries a fix today** | **8** | **~6%** |
| **B — could carry a fix (mechanical, info already in hand)** | **71** | **~53%** |
| **C — needs a human (→ structured alternatives)** | **54** | **~41%** |

> **Snapshot date 2026-07-06.** Since this count, the resolver did-you-mean family (E0100 / E0104 / import / E_MODULE_BINDING_NAMING / E0107 label rename) and the ownership `fix_diff` migration have been wired to real edits (B-2026-07-06-3 `830831f`, B-2026-07-07-3 `911db54`, B-2026-07-06-4 `0f21b4b`) — roughly +6 A / −6 B against these totals. The per-family table below is kept current; this headline row is the original audit snapshot.

- The flagship is **~6% real today**.
- The **auto-fix ceiling is A+B ≈ 60%** — reachable with mechanical wiring, no new analysis.
- The remaining **~41% (C)** is the *structured-alternatives* design, not auto-edits.

Per family (A / B / C):

| Phase | A | B | C | Note |
|---|---|---|---|---|
| Parse | 1 | 0 | 0 | E0001 comma fix |
| Resolver | 6\* | 12 | 14 | \*did-you-mean family now machine-applicable: E0100 / E0104 / unknown-module / unknown-item (`76be2de`) + E_MODULE_BINDING_NAMING const-rename (`830831f`, B-2026-07-06-3) + E0107 `continue`-label rename (`911db54`, B-2026-07-07-3) |
| Typechecker | 3 | 42 | 19 | `fix_it` wired end-to-end |
| Effect | 1 | 7 | 10 | E0412 is the lone A |
| Ownership | 3\* | 4 | 11 | \*3 wired (N0507 + `ConcurrentShared`/`PlainStruct` `fix_diff`, `0f21b4b`) |

(One dead variant — ownership `N0503 RcFallbackNote`, defined but never emitted — is excluded from live counts.)

## Two "free" wins — fixes the compiler already computes but never applies

These are wiring gaps, not new features — the same latent-gap pattern already closed once for typecheck `fix_it`s. Tracked as ledger entries.

### 1. Resolver did-you-mean → `.replacement`  (`B-2026-07-06-3`) — **FIXED**

The resolver computes the exact correct name (`suggest_const_name`, fuzzy matches) and its error struct **has** a `.replacement` field. The E0100/E0104/import did-you-mean sites were wired in `76be2de`; the remaining `E_MODULE_BINDING_NAMING` const-rename was wired in `830831f` (adding a `name_span` to `ModuleBinding` so the edit anchors to the identifier alone). `karac fix` now applies `let myConfig` → `let MY_CONFIG`. E0107 UndefinedLabel was completed in `911db54` (B-2026-07-07-3) by adding a `label_span` to `ExprKind::Continue` — `karac fix` renames `continue otuer` → `continue outer`. (A misspelled `break` label never reaches E0107: it parses as a value expression and surfaces as E0100, already machine-applicable.) The whole E01xx/E0107 did-you-mean family is now machine-applicable.

### 2. Ownership `fix_diff` applier  (`B-2026-07-06-4`) — **FIXED (`0f21b4b`)**

`ConcurrentSharedStruct` / `ConcurrentPlainStruct` compute a full multi-edit migration (`par struct` keyword + per-field `Mutex[T]` wraps), emit it to JSON as a `fix_diff` array — and `cmd_fix` collected only `.replacement`, so `karac fix` silently ignored it. **Fixed:** `cmd_fix` now flattens `pipeline.ownership.error_fix_diffs` values into the edit set; the existing descending-offset sort + overlap dedup applies the envelope safely. Two end-to-end `tests/cli.rs` regressions drive real `karac fix` over the plain-struct (4 edits) and shared-struct (7 edits) cases. Flips these 2 A⚠→A✓.

## The B→A ladder (cheapest first)

1. **Attribute-position family (~12 resolver Bs) — one shared helper.** Every "`#[...]` on the wrong target" error (`non_exhaustive`, `track_caller`, `deprecated`, `profile`, codegen-hint, `default`, malformed-args) has the identical repair: delete the attribute span. One `delete-span replacement` helper closes a dozen at once.
2. **Typechecker one-token edits (biggest pool).** Small `fix_it`s at existing emission sites, in the pattern already used in `patterns.rs`: E0220 `i128`→`i64`, E0252 `let`→`let mut`, E0258 `mut self`→`ref self`, E0270 insert `MemoryOrdering::Relaxed`, E0202 arg-count, E0203 insert-missing-field, E0218 insert `mut`, E0219 remove `mut`, E0241 add `..`.
3. **Effect signature edits — highest strategic value (flagship axis).** E0400/E0401 add-or-remove *the exact effect the checker already inferred* (the inferred set is in hand at the emission site); E0413 add `with panics`.

## The C bucket is a design; E0412 is its template

The ~41% that "needs a human" should not be bare prose. E0412 shows the pattern: it offers two valid repairs, emits the *safe* one (`ref self`) as the auto-fix, and names the *alternative* (declare `writes`) in the message. Generalize: for C diagnostics the compiler usually knows the 2–3 options even when it can't pick (E0200: cast / convert / change-decl; E0508: clone-inside / don't-return / make-owned). The invariant then reads: **A/B → single safe auto-edit; C → structured `alternatives:[...]` the agent chooses from.**

---

## Appendix — full per-diagnostic tables

Bucket key: **A✓** applied · **A⚠** emitted-not-applied · **B** mechanical, unbuilt · **C** needs-human. `B*` = mechanical only when a unique candidate exists, else C.

### Parse

| Code | Kind | Meaning | Bucket | Fix / proposal |
|---|---|---|---|---|
| E0001 | (catch-all) | stray comma in effect clause | A✓ | `fix_edits` delete-comma (`parser/items_effects.rs:189`) |

### Resolver (`ResolveErrorKind`)

| Code | Variant | Meaning | Bucket | Fix / proposal / why-human |
|---|---|---|---|---|
| E0100 | UndefinedName | name not found | B | rename to computed unique candidate (`suggest_const_name`/fuzzy) — data exists, not emitted |
| E0101 | DuplicateDefinition | two defs same name | C | ambiguous which to remove |
| E0102 | ReservedIdentifier | name is reserved | C | replacement name is the author's choice |
| E0103 | PrivateAccess | item not accessible | C | add `pub` vs restructure — human |
| E0104 | UndefinedType | type name not found | B* | rename iff single fuzzy match |
| E0105 | UndefinedVariant | variant not found | C | correct variant unknown |
| E0106 | UndefinedField | field not found | C | rename/restructure/remove — ambiguous |
| E0107 | UndefinedLabel | loop label not found | A | rename iff single nearby label (`911db54`) |
| E0108 | OperatorTraitImplRestricted | operator-trait impl on non-stdlib type | C | move code — human |
| E0109 | IntoTraitImplNotAllowed | impl Into instead of From | C | trait rename needs intent |
| E0110 | ImplLevelEffectVarNotAllowed | effect var on impl generics | C | structural move to method |
| E0222 | PrivateItemAccess | item private cross-module | C | make pub vs restructure |
| E0224 | UnknownModule | module path not found | C | correct path unknown |
| E0225 | UnknownItemInModule | item not in module | C | correct item unknown |
| E0228 | ReservedEffectResource | resource uses reserved name | C | replacement name is author's choice |
| E0237 | CompilerBuiltinReserved | `#[compiler_builtin]` in user code | B | delete attribute |
| E0238 | ContinueOnBlockLabel | `continue` on labeled block | C | convert block→loop — structural |
| E0239 | NonExhaustiveInvalidTarget | `#[non_exhaustive]` wrong target | B | delete attribute |
| E0240 | TrackCallerInvalidTarget | `#[track_caller]` wrong target | B | delete attribute |
| E0241 | DeprecatedOnImpl | `#[deprecated]` on impl | B | delete attribute |
| E0242 | DeprecatedOnField | `#[deprecated]` on field | B | delete attribute |
| E0243 | UnknownAttribute | unknown bare attribute | B | delete attribute |
| E0244 | ProfileInvalidTarget | `#[profile]` wrong target | B | delete attribute |
| E0245 | UnknownProfile | invalid profile name | B* | replace iff single valid profile |
| E0800 | GpuInvalidTarget | `#[gpu]` wrong target | B | delete attribute |
| — | CodegenHintInvalidTarget | codegen hint wrong target | B | delete attribute |
| — | CodegenHintOnExternDecl | codegen hint on extern | B | delete attribute |
| — | QueryResolutionConflict | two attributes conflict | C | which to remove — human |
| — | UnionNonExhaustiveForbidden | `#[non_exhaustive]` on union | B | delete attribute |
| — | DefaultAttributeInvalidPosition | `#[default]` wrong position | B | delete attribute |
| — | DefaultAttributeWithoutDerive | `#[default]` w/o derive | B | add `#[derive(Default)]` to enum |
| — | MalformedAttributeArgs | `#[default(...)]` has args | B | delete the args |

### Typechecker (`TypeErrorKind`)

| Code | Variant | Meaning | Bucket | Fix / proposal / why-human |
|---|---|---|---|---|
| E0200 | TypeMismatch | value type ≠ expected | C | correct value/conversion unknown |
| E0201 | UndefinedField | field absent on type | C | correct field unknown |
| E0202 | WrongNumberOfArgs | wrong arg count | B | add placeholder args / remove extras |
| E0203 | MissingField | struct literal missing field | B | insert `field: <placeholder>` |
| E0204 | ExtraField | struct literal extra field | B | delete the field |
| E0205 | NonExhaustiveMatch | match missing arm(s) | A✓ | fix_it inserts witness `=> todo()` (`patterns.rs:1061`) |
| E0206 | NotCallable | value not callable | C | unknowable |
| E0207 | NotAStruct | non-struct in struct literal | C | unknowable |
| E0208 | InvalidBinaryOp | binop invalid for types | C | different op/cast/method — unknowable |
| E0209 | InvalidUnaryOp | unary op invalid | C | unknowable |
| E0210 | InvalidCast | invalid cast | B | insert compiler-computed valid target |
| E0211 | ConditionNotBool | condition not bool | B | wrap `.bool()` / replace |
| E0212 | BranchTypeMismatch | if branches differ | C | intended branch type unknown |
| E0213 | ReturnTypeMismatch | return type mismatch | C | correct value/decl unknown |
| E0214 | InvalidTupleIndex | tuple index OOB | B | delete invalid index |
| E0215 | LabelMismatch | label mismatch | C | intended label unknown |
| E0216 | NonContiguousLabels | labels not contiguous | C | intended set unknown |
| E0217 | InvalidPipePlaceholder | pipe placeholder error | B | remove/reposition placeholder |
| E0218 | MissingMutMarker | fresh-binding arg needs `mut` | B | insert `mut` |
| E0219 | InvalidMutMarker | `mut` marker invalid | B | remove `mut` |
| E0220 | UnsupportedNumericSuffix | `i128`/`u128` unsupported | B | `i128`→`i64`, `u128`→`u64` |
| E0221 | PrivateTypeInPublicSignature | private type in pub sig | C | make pub / change sig / privatize fn |
| E0222 | RefutablePattern | refutable in irrefutable slot | C | needs `if let`/`match` restructure |
| E0229 | MissingSupertrait | impl missing supertrait | B | add the known `impl Supertrait for T` |
| E0232 | TraitBoundNotSatisfied | type violates bound | C | candidate type unknown |
| E0233 | AmbiguousAssocFn | ambiguous assoc fn | C | UFCS disambiguation — human |
| E0234 | CannotInferAssocFn | no expected-type context | C | annotation/UFCS — human |
| E0235 | OnceFnIntoFnSlot | OnceFn into Fn slot | C | restructure — human |
| E0236 | NoMethodFound | method not found | B* | rename iff typo hint present, else C |
| W0237 | UnreachableArm | arm covered by earlier | B | delete the dead arm |
| W0238 | RefinementDomainTooWide | domain > 1024 | C | wildcard vs enum — human |
| E0238 | CannotInferTypeParam | uninferred return param | C | annotation/usage — human |
| E0239 | AmbiguousMethod | multiple impl candidates | C | UFCS — human |
| E0240 | ConflictingImpl | overlapping impls | C | refactor/merge — human |
| E0241 | NonExhaustiveCrossPackageLiteral | literal missing `..` | B | insert `, ..` |
| E0242 | NonExhaustiveCrossPackageMatch | match missing wildcard | A✓ | fix_it inserts `_ => panic(...)` (`patterns.rs:1008`) |
| E0243 | NonExhaustiveCrossPackagePattern | pattern missing `..` | A✓ | fix_it inserts `..` (`patterns.rs:629,1537`) |
| W0244 | UnknownLint | lint not in registry | B | remove unknown lint name |
| W0245/E0245 | Deprecated | deprecated symbol | B | replace w/ suggested alt or delete |
| W0246 | MissingNonExhaustive | stdlib Error enum needs attr | B | add `#[non_exhaustive]` |
| E0247 | ForbiddenLintAllow | `-F` forbid violated | B | remove the `#[allow(NAME)]` |
| E0248 | ExpectOnUnfulfilled | circular expect | B | remove the `#[expect(...)]` |
| W0249/E0249 | UnfulfilledLintExpectation | expect never fired | B | remove the `#[expect(NAME)]` |
| E0250 | ModuleBindingEffectfulInit | non-const module init | C | LazyLock vs const refactor — human |
| E0251 | ModuleBindingHeapType | module binding is `String` | B | `String`→`StringSlice` |
| E0252 | ReassignToImmutableModuleBinding | reassign immutable module `let` | B | `let`→`let mut` |
| E0253 | ScopeLocalEscape | ScopeLocal escapes | C | restructure — human |
| E0254 | CrossTaskUnsafeCapture | unsafe cross-task capture | C | restructure — human |
| E0255/W0255 | UnstableApi | use of `#[unstable]` | B | remove the unstable use |
| E0256 | InvalidRefinementPredicate | disallowed refinement construct | C | simplify predicate — human |
| E0257 | ParFieldNotConcurrent | par-struct field not Atomic/Mutex | B | wrap field `Atomic[T]`/`Mutex[T]` |
| E0258 | ParMutSelfReceiver | par-struct method `mut self` | B | `mut self`→`ref self` |
| E0260 | LockTargetNotMutex | `lock` target not Mutex | C | correct binding unknown |
| E0261 | AtBindingDoubleConsume | `@` double-consume | C | pattern restructure — human |
| E0262 | TypeAliasBoundNotSatisfied | alias arg fails bound | C | candidate type unknown |
| E0263 | RangePatternBoundNotConst | range bound not const | B | move bound to module `const` |
| E0264 | PanickingAllocRejected | panicking alloc under fallible profile | B | replace w/ `try_*` companion |
| E0265 | DeriveCloneAllocates | derive(Clone) allocates | B | manual `impl Clone` w/ `try_clone` or allow-lint |
| E0266 | MainReturnType | invalid `main` return | B | change to `()`/`Result`/`ExitCode` |
| E0267 | MainErrNotDisplay | main err lacks Display | B | add `impl Display for E` |
| E0268 | StringNotIndexable | `s[i]` on String | B | `.char_at(i)`/`.bytes()[i]` |
| E0269 | SharedFieldNotMut | reassign non-mut shared field | B | add `mut` to field decl |
| E0270 | AtomicMissingOrdering | atomic op missing ordering | B | insert `MemoryOrdering::Relaxed` |
| E0801 | GpuNotSafe | gpu fn uses non-gpu-safe type | C | which types to swap — human |

### Effect (`EffectErrorKind`)

| Code | Variant | Meaning | Bucket | Fix / proposal / why-human |
|---|---|---|---|---|
| E0400 | MissingEffectDeclaration | pub fn performs undeclared effect | B | add the inferred effect to `with` |
| E0401 | OverDeclaredEffect | declares unperformed effect | B | remove the over-declared effect |
| E0402 | CircularEffectGroup | effect group self-references | C | break cycle — restructure |
| E0403 | UndefinedEffectGroup | references undefined group | B | remove ref / define group |
| E0404 | EffectSubtypeViolation | arg effects exceed slot | B | widen slot `with` to inferred set |
| E0405 | ProfileViolation | extern effect forbidden by profile | B | remove forbidden effect |
| E0406 | EffectVariableConflict | `with E` slots conflict | C | unify/split — restructure |
| E0407 | ProfileIncompatibleEffect | profile fn has forbidden effect | C | policy decision |
| E0408 | ModuleBindingWriteInPar | module `let mut` written in `par` | C | Atomic/Mutex/thread_local — design |
| E0409 | PubFnSyntheticResource | unnameable synthetic resource | C | wrap/opt-in — policy |
| E0410 | ForbiddenEffectInContract | side effect in contract predicate | C | remove side effect — semantics |
| E0411 | TargetGateViolation | needs HOST resource target lacks | C | gate/bind/reroute — design |
| E0412 | ResourceReceiverContradiction | reads-only method w/ owning `self` | A✓ | replace `self`→`ref self` (`inference.rs:189`) — safe repair; alt `writes(R)` in message |
| E0413 | ExternCUnwindRequiresPanics | may-panic extern lacks decl | B | add `with panics` |
| E0802 | GpuEffectViolation | gpu graph has forbidden effect | C | architectural |
| L0001 | FfiLintHint | advisory FFI-effect hint (note) | B | add suggested effect (advisory) |

### Ownership (`OwnershipErrorKind`)

| Code | Variant | Meaning | Bucket | Fix / proposal / why-human |
|---|---|---|---|---|
| E0500 | UseAfterMove | value used after move | C | `.clone()` is mechanical but may mask intent — human |
| E0501 | OwnershipCycle | cycle in type graph | C | ref/Box/shared/restructure — design |
| E0502 | NoRcViolation | `@no_rc` needs RC fallback | C | restructure ownership — human |
| N0503 | RcFallbackNote | (dead — never emitted) | — | — |
| E0504 | CaptureModeViolation | capture mode violated by body | C | redesign capture/body — human |
| E0505 | UseOfUninitialized | read before assignment | B | insert initialization before first read |
| E0506 | ReassignToImmutable | reassign non-mut `let` | B | `let`→`let mut` |
| N0507 | UnusedMutCaptureNote | `mut ref` capture never mutates | A✓ | `.replacement` `mut ref`→`ref` (applied) |
| E0508 | RefCaptureEscapesScope | borrow capture escapes via return | C | clone-inside / don't-return / make-owned — human |
| E0509 | BorrowReturnNotSourcePinned | `-> ref T` not pinned to ref param | C | unsupported return form — human |
| — | SliceFromTemporaryEscapes | slice from temp escapes stmt | B | bind temp to local, then slice |
| — | SliceBorrowConflict | conflicting slice borrows | C | redesign lifetimes — human |
| — | CrossBorrowConflict | slice + ref conflict on source | C | drop one borrow / redesign — human |
| — | ClosureCaptureBorrowConflict | consume conflicts w/ capture borrow | C | restructure captures — human |
| — | RcBudgetExceeded | RC bindings over module budget | C | restructure / raise budget — human |
| — | ConcurrentSharedStruct | shared struct in >1 par branch | A⚠ | `fix_diff` per-field `Mutex[T]` wraps — **emitted, dropped by `karac fix`** |
| — | ConcurrentPlainStruct | plain struct in >1 par branch | A⚠ | `fix_diff` `par struct` + `Mutex[T]` wraps — **emitted, dropped by `karac fix`** |
| — | RcFallbackAllocatesUnderFallibleProfile | RC fallback would panic under fallible profile | C | restructure / try_new — human |
| — | ExclusiveBorrowAliasedArgs | exclusive borrow aliased in call | B | `split_at_mut` / distinct bindings |
