//! Lints the `kara` code blocks in `docs/design.md` against the grammar the
//! parser actually implements.
//!
//! `design.md` is the authoritative spec AND the corpus an LLM reads when
//! writing Kāra, so a block that does not parse is not a cosmetic nit — it
//! teaches the wrong language. The Mend loop's whole premise is blind
//! authorship against this document.
//!
//! B-2026-08-17-31: 68 declarations across 14 sections omitted the trailing
//! `;` the parser requires after a bodyless `fn` / `type` inside a `trait` or
//! `impl`, and after a top-level `type` alias — while a dozen others in the
//! same document carried it. Transcribing § Operator Traits produced a block
//! where every line was a parse error, reported as `Expected Semicolon, found
//! Fn` against the line AFTER the offending one.
//!
//! This is a targeted lint, not a "every block parses" gate: most blocks are
//! deliberate fragments (`{ ... }` bodies, `where` clauses shown alone) that
//! cannot compile standalone. It checks the one syntactic rule that has
//! actually gone wrong, which keeps it precise enough to be actionable when it
//! fires.

const DESIGN_MD: &str = include_str!("../docs/design.md");

/// Line kind for the enclosing-block stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Encl {
    Trait,
    Impl,
    Other,
}

/// A declaration that the parser requires to end in `;` but does not.
struct Offender {
    line_no: usize,
    text: String,
}

/// Byte offset -> 1-based line number in the whole document.
fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

fn starts_with_word(s: &str, word: &str) -> bool {
    s.strip_prefix(word).is_some_and(|rest| {
        rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric() && c != '_')
    })
}

/// Strip an optional `pub ` and `distinct ` prefix, so `pub distinct type X`
/// classifies the same as `type X`.
fn strip_modifiers(s: &str) -> &str {
    let s = s.strip_prefix("pub ").unwrap_or(s).trim_start();
    s.strip_prefix("distinct ").unwrap_or(s).trim_start()
}

/// The body of a `trait X { … }` written entirely on one line, if this is one.
fn single_line_trait_body(st: &str) -> Option<&str> {
    if !starts_with_word(strip_modifiers(st), "trait") {
        return None;
    }
    let open = st.find('{')?;
    let close = st.rfind('}')?;
    if close < open || !st[close + 1..].trim().is_empty() {
        return None;
    }
    Some(&st[open + 1..close])
}

fn scan(text: &str) -> Vec<Offender> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut consumed = 0usize;

    while let Some(open) = rest.find("```kara\n") {
        let body_start = open + "```kara\n".len();
        let Some(close_rel) = rest[body_start..].find("```") else {
            break;
        };
        let body = &rest[body_start..body_start + close_rel];
        let body_abs = consumed + body_start;

        let mut stack: Vec<Encl> = Vec::new();
        let mut depth: i32 = 0;
        let mut line_start = 0usize;

        let body_lines: Vec<&str> = body.split('\n').collect();
        for (idx, raw) in body_lines.iter().enumerate() {
            let raw = *raw;
            // The code half of the line: comments never carry the `;`.
            let code = raw.split("//").next().unwrap_or("").trim_end();
            let st = code.trim();
            let opens = code.matches('{').count();
            let closes = code.matches('}').count();

            let balanced = opens == 0 && closes == 0;
            let terminated = st.is_empty()
                || st.ends_with(';')
                || st.ends_with(',')
                || st.ends_with('{')
                || st.ends_with('}')
                || st.ends_with('\\');

            // A continued signature (`fn matmul[M, K, N](` opening a
            // multi-line parameter list) is not a bodyless declaration; its
            // `;`, if any, belongs on the closing line.
            let parens_balanced = code.matches('(').count() == code.matches(')').count();

            // SINGLE-LINE trait body: `trait Add { fn add(self, rhs: Self) -> Self }`.
            // The whole declaration lives between braces on one line, so the
            // balanced-brace path below never sees it — and design.md §
            // Operator Traits is written entirely in this form, which is why
            // the row called it a block where every line is a parse error.
            // Measured: that exact line fails with `Expected Semicolon, found
            // RightBrace`, and gains "All checks passed" with the `;`.
            if let Some(inner) = single_line_trait_body(st) {
                let inner = inner.trim();
                // `{}` is a marker trait and `{ ... }` is an elision, not code.
                if !inner.is_empty() && inner != "..." && !inner.ends_with(';') {
                    out.push(Offender {
                        line_no: line_of(text, body_abs + line_start),
                        text: raw.to_string(),
                    });
                }
            }

            // A signature whose NEXT non-empty line continues it — an effect
            // clause, a `where`, a wrapped return, or the body's brace — is
            // not bodyless. design.md § Effects writes
            // `pub fn new(...) -> Result[...]` / `with reads(FileSystem) …` /
            // `{`, which is a complete method and must not gain a `;`.
            let continued = body_lines[idx + 1..]
                .iter()
                .map(|l| l.split("//").next().unwrap_or("").trim())
                .find(|l| !l.is_empty())
                .is_some_and(|l| {
                    starts_with_word(l, "with")
                        || starts_with_word(l, "where")
                        // Contract clauses sit between the signature and the
                        // body (design.md § Contracts): `fn transfer(...) ->
                        // (Account, i64)` / `requires …` / `ensures(result) …`
                        // / `{ … }`. A `;` after the signature splits it from
                        // its own contract.
                        || starts_with_word(l, "requires")
                        || starts_with_word(l, "ensures")
                        || starts_with_word(l, "invariant")
                        || l.starts_with("->")
                        || l.starts_with('{')
                });

            if balanced && parens_balanced && !terminated && !continued {
                let bare = strip_modifiers(st);
                // A bodyless `fn` is a declaration form only inside a `trait`.
                // An `impl` body's methods always have bodies, so an `impl`
                // full of bodyless signatures is an API SKETCH rather than
                // code — design.md § MaybeUninit is explicitly labelled
                // "(sketch)" — and a `;` would not make it parse. `type`
                // needs the terminator in all three positions.
                let is_decl = match stack.last() {
                    Some(Encl::Trait) => {
                        starts_with_word(bare, "fn") || starts_with_word(bare, "type")
                    }
                    Some(Encl::Impl) => starts_with_word(bare, "type"),
                    _ => depth == 0 && starts_with_word(bare, "type"),
                };
                if is_decl {
                    out.push(Offender {
                        line_no: line_of(text, body_abs + line_start),
                        text: raw.to_string(),
                    });
                }
            }

            if opens > 0 {
                let mut kind = if starts_with_word(strip_modifiers(st), "trait") {
                    Encl::Trait
                } else if starts_with_word(strip_modifiers(st), "impl") {
                    Encl::Impl
                } else {
                    Encl::Other
                };
                for _ in 0..opens {
                    stack.push(kind);
                    kind = Encl::Other;
                }
            }
            for _ in 0..closes {
                stack.pop();
            }
            depth += opens as i32 - closes as i32;
            line_start += raw.len() + 1;
        }

        let advance = body_start + close_rel + 3;
        consumed += advance;
        rest = &rest[advance..];
    }
    out
}

/// Every bodyless `fn` / `type` declaration in a spec code block ends in `;`.
///
/// The parser rejects all three offending shapes — verified directly, not
/// assumed: `trait C { type Item \n fn f(...); }` and `type Alias = i64` both
/// fail with `Expected Semicolon`, and the same sources with `;` added check
/// clean and run.
#[test]
fn design_md_declarations_carry_their_terminating_semicolon() {
    let offenders = scan(DESIGN_MD);
    assert!(
        offenders.is_empty(),
        "{} declaration(s) in docs/design.md omit the `;` the parser requires \
         after a bodyless `fn` / `type`; transcribing these produces code that \
         does not parse (B-2026-08-17-31):\n{}",
        offenders.len(),
        offenders
            .iter()
            .map(|o| format!("  docs/design.md:{}: {}", o.line_no, o.text))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The scanner must actually be looking at something.
///
/// A lint whose matcher silently stops matching reads exactly like a clean
/// document, which is the one failure mode that would let this rot. Feeding it
/// the shapes it exists to catch proves the matcher still works, and feeding
/// it their fixed twins proves it is not simply flagging everything.
#[test]
fn the_scanner_catches_the_shapes_it_exists_for() {
    let bad = "```kara\n\
               trait C {\n\
                   type Item\n\
                   fn first(ref self) -> Self.Item\n\
               }\n\
               type Alias = i64\n\
               ```\n";
    assert_eq!(
        scan(bad).len(),
        3,
        "expected the two trait declarations and the alias, got: {:?}",
        scan(bad).iter().map(|o| &o.text).collect::<Vec<_>>()
    );

    let good = "```kara\n\
                trait C {\n\
                    type Item;\n\
                    fn first(ref self) -> Self.Item;\n\
                }\n\
                type Alias = i64;\n\
                ```\n";
    assert!(scan(good).is_empty(), "the fixed twin must be clean");

    // The single-line form § Operator Traits is written in.
    let one_line = "```kara\n\
                    trait Add { fn add(self, rhs: Self) -> Self }\n\
                    ```\n";
    assert_eq!(
        scan(one_line).len(),
        1,
        "the single-line trait body must be caught"
    );
    let one_line_ok = "```kara\n\
                       trait Add { fn add(self, rhs: Self) -> Self; }\n\
                       trait Eq: PartialEq {}\n\
                       trait FixedBuf[const CAP: i64] { ... }\n\
                       ```\n";
    assert!(
        scan(one_line_ok).is_empty(),
        "a terminated body, a marker trait and an elision must all pass: {:?}",
        scan(one_line_ok)
            .iter()
            .map(|o| &o.text)
            .collect::<Vec<_>>()
    );

    // A method WITH a body is not a bodyless declaration, and a `where` clause
    // continuation is not one either — neither takes a `;`. Nor does a
    // signature followed by its contract clauses.
    let fragments = "```kara\n\
                     impl C for B {\n\
                         fn first(ref self) -> i64 { return 1; }\n\
                     }\n\
                     fn sum[I: Iterator](iter: I) -> I.Item\n\
                     where I.Item: Add\n\
                     { ... }\n\
                     ```\n";
    let contract = "```kara\n\
                    impl Account {\n\
                        fn transfer(self, amount: i64) -> (Account, i64)\n\
                            requires amount > 0\n\
                            ensures(result) result.1 == amount\n\
                        {\n\
                            (self, amount)\n\
                        }\n\
                    }\n\
                    ```\n";
    assert!(
        scan(contract).is_empty(),
        "a signature followed by its contract clauses must not be flagged: {:?}",
        scan(contract).iter().map(|o| &o.text).collect::<Vec<_>>()
    );
    assert!(
        scan(fragments).is_empty(),
        "bodied methods and where-clause fragments must not be flagged: {:?}",
        scan(fragments).iter().map(|o| &o.text).collect::<Vec<_>>()
    );
}

// ── statement terminators, driven by the real parser ────────────

/// Every `kara` block in `design.md`, with the 1-based document line its body
/// starts on.
fn kara_blocks(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut consumed = 0usize;
    while let Some(open) = rest.find("```kara\n") {
        let body_start = open + "```kara\n".len();
        let Some(close_rel) = rest[body_start..].find("```") else {
            break;
        };
        let body = &rest[body_start..body_start + close_rel];
        out.push((line_of(text, consumed + body_start), body));
        let advance = body_start + close_rel + 3;
        consumed += advance;
        rest = &rest[advance..];
    }
    out
}

/// Blocks whose `Expected Semicolon` is NOT a missing statement terminator, and
/// which therefore cannot be repaired by inserting one. Each is keyed by a
/// substring of the offending line rather than a line number, so the entry
/// survives edits elsewhere in the document and names WHY it is here.
///
/// THE LIST IS EMPTY, and that is the intended resting state: every entry ever
/// added has been retired by fixing the compiler rather than by keeping the
/// exemption. A new arrival is a real regression to diagnose, not a line to
/// append to. The sibling guard test below is what forces a stale entry out —
/// it is vacuous while the list is empty and becomes load-bearing the moment
/// anything is added.
///
/// The last entry was `effect resource RequestCh: Channel[Request]`, exempt
/// because its `Expected Semicolon` was the parser stopping at `[` — a
/// declaration form design.md specifies and the parser did not accept
/// (B-2026-08-18-41). The generic provider bound now parses, which exposed a
/// SECOND defect the first had been hiding: that line is also missing its
/// terminator, and no terminator gate could see it while the parser was failing
/// earlier on the same line. Both are fixed; the exemption goes.
const NON_TERMINATOR: &[(&str, &str)] = &[];

// REMOVED, and the removal is exactly what the sibling test below exists to
// force. `matches!` was listed here because it surfaced as `Expected Semicolon,
// found Bang` -- the `!` follows an identifier, so the parser stopped at a token
// that reads like a missing terminator. `ce0ae03` then gave that shape a
// diagnostic of its own ("Kara has no macros; `matches!(...)` is Rust syntax --
// call it as `matches(...)`"), so it no longer reaches this gate and the entry
// had become a standing licence to regress that line unnoticed. The guard test
// failed on the very next full run and named it.
//
// B-2026-08-18-42 STAYS OPEN: the document still carries eight Rust macro sites.
// Only the diagnostic improved, not the corpus.

/// No `kara` block in design.md is missing a statement terminator.
///
/// B-2026-08-18-29. This is the half of B-2026-08-17-31 a LINE-LEVEL pass
/// provably cannot finish, and the reason is recorded in that row: appending
/// `;` to a `let` whose right-hand side CONTINUES on the following lines
/// corrupts it, and brackets balance on each line taken alone, so no
/// bracket-counting heuristic rescues it. Two blocks went from parsing cleanly
/// to failing when that was tried, which is the one outcome a docs edit must
/// never produce.
///
/// So this gate asks the parser instead of pattern-matching text, which makes
/// it exact: `Expected Semicolon` is reported at the token FOLLOWING the
/// omission, and only a real parse knows where that is.
///
/// It deliberately checks that one diagnostic rather than "every block parses":
/// most blocks are fragments (`{ ... }` bodies, `where` clauses shown alone)
/// that cannot compile standalone, so a blanket gate would be noise. design.md
/// is the corpus an LLM reads when writing Kāra and the Mend loop's premise is
/// blind authorship against it, which is what makes a block that does not
/// transcribe a defect rather than a cosmetic nit.
#[test]
fn design_md_statements_carry_their_terminating_semicolon() {
    let mut offenders: Vec<String> = Vec::new();
    for (start_line, body) in kara_blocks(DESIGN_MD) {
        for err in karac::parse(body).errors {
            if !err.message.contains("Expected Semicolon") {
                continue;
            }
            // The parser reports at the token AFTER the missing `;`, so the
            // omission is on the preceding non-empty code line -- report both,
            // since the fix goes on the second.
            let lines: Vec<&str> = body.split('\n').collect();
            let at = err.span.line;
            let reported = lines.get(at - 1).copied().unwrap_or("");
            let mut j = at as isize - 2;
            while j >= 0
                && lines[j as usize]
                    .split("//")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
            {
                j -= 1;
            }
            let culprit = if j >= 0 { lines[j as usize] } else { "" };
            if NON_TERMINATOR
                .iter()
                .any(|(needle, _)| reported.contains(needle) || culprit.contains(needle))
            {
                continue;
            }
            offenders.push(format!(
                "  docs/design.md:{}: {}\n      (parser stopped at docs/design.md:{}: {})",
                start_line + j.max(0) as usize,
                culprit.trim(),
                start_line + at - 1,
                reported.trim()
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "{} statement(s) in docs/design.md omit a terminating `;` (B-2026-08-18-29); \
         transcribing these produces code that does not parse:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

/// No `kara` block in design.md uses RUST MACRO SYNTAX. B-2026-08-18-42.
///
/// design.md carried eight `name!(...)` sites — six `panic!`, one `matches!`,
/// one `format_into!` — none of which Kāra can parse, because the language has
/// no macros at all. design.md is the corpus an LLM reads when writing Kāra and
/// the Mend loop's premise is blind authorship against it, so a reader
/// transcribing § Never type got a parse error from the document meant to teach
/// them.
///
/// ASKS THE PARSER rather than pattern-matching text, for the same reason the
/// terminator gate does: `vec![1, 2, 3]` IS valid Kāra (the postfix path
/// consumes it before prefix parsing), so a regex over `name!(` would have to
/// carry a hand-maintained exception list and would flag the one legitimate
/// form. The parser already draws that line exactly.
#[test]
fn design_md_kara_blocks_use_no_rust_macros() {
    let mut offenders: Vec<String> = Vec::new();
    for (start_line, body) in kara_blocks(DESIGN_MD) {
        for err in karac::parse(body).errors {
            if !err.message.contains("no macros") {
                continue;
            }
            let lines: Vec<&str> = body.split('\n').collect();
            let at = err.span.line;
            offenders.push(format!(
                "  docs/design.md:{}: {}\n      {}",
                start_line + at - 1,
                lines.get(at - 1).copied().unwrap_or("").trim(),
                err.message
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "{} Rust macro call(s) in docs/design.md's kara blocks (B-2026-08-18-42); \
         Kāra has no macros, so transcribing these produces code that does not parse:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

/// The macro gate above is only worth having if it actually fires, and the
/// shape it must NOT fire on is the one legitimate `!` form in the language.
#[test]
fn the_macro_gate_fires_on_a_macro_and_not_on_vec() {
    let macro_call = "fn main() {\n    let y = id(panic!(\"never\"));\n}\n";
    assert!(
        karac::parse(macro_call)
            .errors
            .iter()
            .any(|e| e.message.contains("no macros")),
        "the gate would not have caught an argument-position `panic!`"
    );
    let vec_literal = "fn main() {\n    let v: Vec[i32] = vec![1, 2, 3];\n}\n";
    assert!(
        !karac::parse(vec_literal)
            .errors
            .iter()
            .any(|e| e.message.contains("no macros")),
        "`vec![…]` is valid Kāra and must not be flagged as a macro"
    );
}

/// The allow-list above stays honest: an entry that no longer fires means its
/// underlying row was fixed, and the entry must go rather than silently
/// covering a future regression at the same line.
#[test]
fn non_terminator_allow_list_entries_all_still_fire() {
    for (needle, why) in NON_TERMINATOR {
        let fires = kara_blocks(DESIGN_MD).iter().any(|(_, body)| {
            body.contains(needle)
                && karac::parse(body)
                    .errors
                    .iter()
                    .any(|e| e.message.contains("Expected Semicolon"))
        });
        assert!(
            fires,
            "allow-list entry no longer fires and should be removed: {needle:?} ({why})"
        );
    }
}

// ── spec claims checked against the compiler ────────────────────

/// B-2026-08-17-35 — two design.md sections described a language other than the
/// one the compiler implements, in opposite directions. Both are now pinned to
/// the compiler rather than to prose, because prose is what drifted.
///
/// (1) § `#[derive(Display)]` wrote its data-carrying example as
/// `Circle(radius: f64)` — a tuple variant with NAMED fields, a third spelling
/// that exists nowhere in the language (`error[parse]: Expected RightParen,
/// found Colon`). Transcribing the section that teaches the derive produced a
/// block where the enum itself would not parse.
///
/// (2) § Display then claimed the opposite of what ships: "`#[derive(Display)]`
/// on an enum is restricted to all-unit-variant enums (a data variant requires
/// a manual `impl Display`)". Measured on both backends, a data variant derives
/// and renders `Circle(2.5)` — the interpreter's own test for it carries a
/// comment saying that restriction "was stale", which is how long the sentence
/// had been wrong.
///
/// The example's output comment was wrong too, in a third direction:
/// `Shape.Rect(3.0, 4.0)` renders `Rect(3, 4)`, because a whole-valued `f64`
/// prints without a trailing `.0` in both backends.
#[test]
fn design_md_derive_display_data_variant_example_compiles() {
    let block = kara_blocks(DESIGN_MD)
        .into_iter()
        .find(|(_, b)| b.contains("enum Shape") && b.contains("derive(Display)"))
        .map(|(_, b)| b)
        .expect(
            "§ derive(Display)'s data-carrying `Shape` example has moved or been \
             renamed — re-anchor this test rather than deleting it",
        );
    let parsed = karac::parse(block);
    assert!(
        parsed.errors.is_empty(),
        "the derive(Display) data-variant example must be transcribable; it is \
         the section teaching the feature, and it previously used a \
         tuple-variant-with-named-fields form the language does not have:\n{:?}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
    // The rendered output is asserted end-to-end by
    // `test_enum_display_payload_variants_to_string` (interpreter) and its
    // codegen sibling; what this pins is that the SPEC's spelling is one the
    // parser accepts.
}

/// B-2026-08-17-35 leg (2) — § Subscript Trait opened "User-defined types
/// support `[]` indexing by implementing two standard traits" with no v1
/// caveat, while the resolver rejects a user impl outright. The neighbouring
/// § Operator Traits DOES carry the caveat, so the two sections disagreed with
/// each other about the same v1 boundary.
///
/// This ties the prose to the compiler in both directions. If v1 ever admits
/// user-defined operator impls, the rejection below stops firing and this test
/// fails — which is the point: the caveat would then be the stale claim, and
/// the failure is the reminder to remove it.
#[test]
fn design_md_subscript_trait_v1_caveat_matches_the_resolver() {
    let src = "struct Grid { cells: Vec[i64] }\n\
               impl Index[i64] for Grid {\n\
                   type Output = i64;\n\
                   fn index(ref self, idx: i64) -> ref Self.Output { self.cells[idx] }\n\
               }\n\
               fn main() { println(1); }\n";
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let rejected = resolved
        .errors
        .iter()
        .any(|e| e.message.contains("operator traits are stdlib-only"));
    assert!(
        rejected,
        "v1 no longer rejects a user-defined `impl Index`; § Subscript Trait's \
         v1 caveat has become the stale claim and must be removed"
    );
    let section = DESIGN_MD
        .split("### Subscript Trait")
        .nth(1)
        .expect("§ Subscript Trait heading has moved — re-anchor this test");
    let intro = &section[..section.len().min(1200)];
    assert!(
        intro.contains("stdlib types only") || intro.contains("stdlib-only"),
        "§ Subscript Trait must state the v1 boundary the resolver enforces, as \
         its sibling § Operator Traits does; without it a reader transcribes an \
         impl the compiler refuses"
    );
}

/// B-2026-08-26-1 — § Operator Traits' `vec1 + vec2` redirect must name only
/// methods that exist AND do what the sentence says they do.
///
/// The paragraph used to offer "`vec.concat(other)` or `vec.extend(other)`".
/// `Vec.concat()` is a real method, which is what made this hard to notice: it
/// is the ZERO-ARGUMENT `Vec[String]` join (`join` with an empty separator),
/// so a reader with a `Vec[i64]` who followed the spec got
/// `Vec.concat() requires String elements`, and one with a `Vec[String]` got
/// `Vec.concat() expects 0 argument(s), found 1`. Neither says "that is not
/// what this method is for".
///
/// This is a PROSE guard rather than a code-block one because the claim lives
/// in a sentence, not in a fenced example — `scripts/design-conformance.py`
/// checks the blocks and would never have seen it. The compiler side is
/// already pinned by `tests/cli.rs::test_operators_page_quotes_live_diagnostics`
/// ("vecredirect" asserts the live diagnostic says `use \`a.extend(b)\``), so
/// between the two, spec and implementation cannot drift apart again without a
/// test failing.
#[test]
fn operator_traits_vec_redirect_names_only_extend() {
    let section = DESIGN_MD
        .split("**Notably absent:**")
        .nth(1)
        .expect("§ Operator Traits must still carry the `Notably absent` paragraph");
    // Bound the window to the paragraph plus its follow-up note, so a later
    // unrelated `vec.concat` elsewhere in the document cannot fail this.
    let window: String = section.chars().take(2200).collect();

    assert!(
        !window.contains("vec.concat(other)"),
        "§ Operator Traits offers `vec.concat(other)` as a redirect for \
         `vec1 + vec2` again. `Vec.concat()` is the zero-argument `Vec[String]` \
         join, not a two-Vec concatenation — a reader who follows it gets \
         `Vec.concat() requires String elements`. Name `extend`, which is the \
         one that does this. (B-2026-08-26-1)"
    );
    assert!(
        window.contains("a.extend(b)"),
        "the paragraph must still name the method that actually appends one \
         Vec's elements to another — the whole point of having no \
         `impl Add for Vec[T]` is that the diagnostic names a real method"
    );
    // `append` was the other plausible name a reader might reach for; the
    // paragraph says outright that it does not exist, so keep that.
    assert!(
        window.contains("`append` does not exist"),
        "the paragraph should keep saying `append` does not exist — it is the \
         name a Rust reader tries first"
    );
}

/// B-2026-08-26-2 — § `Hash` and `Hasher`'s account of the MISSING
/// cryptographic hash must stay pointed at where that work actually lives.
///
/// The paragraph's job is to stop a reader substituting `siphash24` (a keyed
/// PRF, not collision-resistant) for a cryptographic hash. Saying only "there
/// is none" does half that job and invites the other failure: someone deciding
/// the gap is theirs to fill in this namespace. `std.crypto` is committed —
/// `deferred.md § std.crypto` fixes BLAKE3 as the general-hashing algorithm and
/// roadmap.md schedules the module at Phase 11+ (P1) behind FFI stabilisation —
/// so the absence is a schedule, and the text has to say which.
///
/// A prose guard, like the `vec.concat` one above: the claim lives in a
/// sentence rather than a fenced block, so `scripts/design-conformance.py`
/// cannot see it.
#[test]
fn hash_section_points_the_crypto_gap_at_its_tracker() {
    let section = DESIGN_MD
        .split("**Stability policy.**")
        .nth(1)
        .expect("§ `Hash` and `Hasher` must still carry the stability policy");
    let window: String = section.chars().take(4000).collect();

    assert!(
        window.contains("deferred.md § std.crypto"),
        "the stability paragraph must name where the cryptographic-hash work \
         lives, or its \"there is none\" reads as an unclaimed gap rather than \
         a schedule (B-2026-08-26-2)"
    );
    assert!(
        window.contains("BLAKE3"),
        "the paragraph should name the committed algorithm — a reader deciding \
         whether to wait or reach for an external library needs to know what \
         is coming"
    );
    assert!(
        window.contains("not a collision-resistant hash"),
        "the substitution warning is the paragraph's whole reason to exist and \
         must survive edits to the surrounding text"
    );
}

/// Every method named in design.md's **`String`** method table must actually
/// exist on `String`. B-2026-08-26-11.
///
/// That row: the table listed `push_char` — `fn push_char(mut ref self, c: char)`
/// — which the compiler has never had, while the method that DOES append a
/// char, `push`, had no row at all. Both directions were broken, which is what
/// separated it from a rename: a reader following the spec got a typecheck
/// error, and a reader looking for how to append a char found no documented
/// way to do it.
///
/// A method TABLE is the surface a user is expected to program against, and
/// this document is also the corpus an LLM reads when writing Kāra blind —
/// the same premise the rest of this file's lints rest on. A table row naming
/// a method that does not exist teaches an API that isn't there.
///
/// EXISTENCE, NOT SIGNATURE, is what this checks, and the distinction is what
/// makes it cheap enough to be worth having. Calling a documented method with
/// deliberately wrong arguments produces either "no method 'x' on type
/// 'String'" (it does not exist — the bug) or some argument/type complaint
/// (it exists, and this lint is satisfied). So the probe needs no per-row
/// signature parsing: it only has to tell those two apart. Static
/// constructors are tried in `String.x()` form as well, since they are not
/// callable on an instance.
#[test]
fn design_md_string_table_names_only_methods_that_exist() {
    let table = string_method_table(DESIGN_MD)
        .expect("could not locate the **`String`** method table in design.md");
    // A floor, so a table that moves or a parser that stops matching cannot
    // make this lint silently pass over an empty set.
    assert!(
        table.len() >= 15,
        "expected the String table to yield many method names, got {}: {table:?}",
        table.len()
    );
    let missing: Vec<&String> = table
        .iter()
        .filter(|name| !string_method_exists(name))
        .collect();
    assert!(
        missing.is_empty(),
        "design.md's `String` method table names {} method(s) the compiler does \
         not have: {missing:?}. A table row is the surface users program \
         against — either implement the method or correct the row (B-2026-08-26-11 \
         was `push_char`, which never existed; the real spelling is `push`).",
        missing.len()
    );
}

/// Extract the first-column method names from design.md's **`String`** table.
/// Stops at the first blank line after the table starts, so it cannot run on
/// into the sections that follow. Operator rows (`+`) and anything that is not
/// a bare identifier are skipped — they are not method-name lookups.
fn string_method_table(text: &str) -> Option<Vec<String>> {
    let start = text.find("**`String`**")?;
    let mut names = Vec::new();
    let mut seen_header = false;
    for line in text[start..].lines().skip(1) {
        let t = line.trim();
        if t.is_empty() {
            if seen_header {
                break;
            }
            continue;
        }
        if !t.starts_with('|') {
            if seen_header {
                break;
            }
            continue;
        }
        seen_header = true;
        let cell = t.trim_matches('|').split('|').next().unwrap_or("").trim();
        let name = cell.trim_matches('`').trim();
        if name.is_empty()
            || name == "Method"
            || name.starts_with("---")
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        names.push(name.to_string());
    }
    Some(names)
}

/// `true` when `String` has a method (or associated function) of this name.
///
/// Probed by CALLING it and inspecting the diagnostic. A name the compiler
/// does not know reports "no method 'x'" / "no associated function 'x'"; a
/// name it does know reports an argument complaint instead ("'push' expects a
/// Char argument, found 'i64'"). Telling those two apart is all this lint
/// needs, so it never has to parse the signature column.
///
/// Several arities are tried because resolution is ARITY-KEYED: measured,
/// `String.with_capacity()` reports "no associated function 'with_capacity'"
/// even though `String.with_capacity(8i64)` typechecks. A single zero-arg
/// probe would therefore report every arity-taking method as missing. Both
/// call forms are tried too, since the table mixes instance methods with
/// constructors. Existence is claimed as soon as ANY probe avoids the
/// does-not-exist diagnostic.
fn string_method_exists(name: &str) -> bool {
    let says_absent = |src: &str| -> bool {
        let parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            // A probe that does not parse proves nothing either way; do not
            // let it count as evidence of absence.
            return false;
        }
        let resolved = karac::resolve(&parsed.program);
        let res = karac::typecheck(&parsed.program, &resolved);
        res.errors.iter().any(|e| {
            let m = &e.message;
            (m.contains(&format!("no method '{name}'"))
                || m.contains(&format!("no associated function '{name}'")))
                && m.contains("String")
        })
    };
    // `0i64` is a deliberately wrong argument type for most of these; a type
    // complaint is a PASS, because only an existing method can complain about
    // its arguments.
    let arg_lists = ["", "0i64", "0i64, 0i64"];
    for args in arg_lists {
        let instance = format!("fn probe() {{ let mut s: String = \"\"; s.{name}({args}); }}");
        if !says_absent(&instance) {
            return true;
        }
        let statik = format!("fn probe() {{ let _ = String.{name}({args}); }}");
        if !says_absent(&statik) {
            return true;
        }
    }
    false
}
