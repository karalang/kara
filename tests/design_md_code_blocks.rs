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
