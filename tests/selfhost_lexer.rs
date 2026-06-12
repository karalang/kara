//! Differential oracle for the self-hosted Kāra lexer (phase-12 self-hosting).
//!
//! Lexes a shared corpus with BOTH the Rust `karac::tokenize` (the bootstrap
//! seed + spec) and the Kāra lexer in `selfhost/src/main.kara` (built via
//! `karac build` — AOT, because the interpreter can't run self-mutating
//! methods, self-hosting blocker #2), and asserts the two token streams render
//! identically. This is the bootstrap oracle: as the port grows, any
//! divergence from the Rust lexer fails here.
//!
//! Covers the port's slice-A+B+C+D token set: all delimiters, punctuation,
//! single- and multi-char operators (maximal-munch forms like `<<=` / `..=` /
//! `?.`), the full keyword table, identifiers, numbers (decimal, hex/bin/octal,
//! float, `_` separators, int/float suffixes), whitespace, line and (nesting)
//! block comments (skipped), `///` / `//!` doc-comment tokens, string /
//! multi-string / char / byte literals (with `\n \t \r \\ \" \'` escapes,
//! rendered through a shared `escape_for_render`), f-string interpolation
//! (`f"…{e}…"` → text/expr parts with absolute expr positions) and c-strings
//! (`c"…"` → byte sequence + source length, incl. `\xHH`), and EOF. Deferred to
//! later slices (and kept OUT of the corpus): `\u{…}` / `\0` escapes, raw
//! identifiers (`r#x`), non-ASCII, and the reserved-word / reserved-prefix
//! error forms. Inputs are single-line (so the
//! reported line is always one) until both the port and the corpus grow
//! newlines. Both lexers emit a trailing EOF, so the full streams (including
//! EOF) are compared.
//!
//! The corpus is lexed back-to-back with NO printed separator between inputs:
//! a bare string-literal `println` was observed to interleave out of order
//! with the lexer's (computed-String) token output, so the oracle stays on the
//! single computed-output path and concatenates every input's token stream in
//! corpus order. Each input's EOF carries its own byte-offset, so the streams
//! stay self-aligning across the join.
//!
//! The Kāra lexer is built with auto-parallelization ON (the default). It used
//! to require `KARAC_AUTO_PAR=0` to dodge self-hosting blocker #8 (the analyzer
//! parallelized the sequentially-dependent scan loops and raced); #8 is fixed
//! (the auto-par dependency analyzer now tracks `self` reads/writes), so the
//! oracle exercises the real default build path.
//!
//! Gated on `--features llvm` and soft-skips when the runtime archive isn't
//! present (the same vacuous-pass contract as the codegen E2E suite). A
//! genuine COMPILE failure of the lexer panics — that's a port regression to
//! catch, not an environment gap.

#![cfg(feature = "llvm")]

use karac::token::{InterpolationPart, SpannedToken, Token};
use std::path::PathBuf;

/// Inputs exercising the slice-A token set. Plain ASCII, no quotes /
/// backslashes / newlines (so they embed verbatim as Kāra string literals),
/// and deliberately free of comments, string/char prefixes, and non-decimal
/// numbers (those produce tokens later slices model). Operator lines space the
/// forms apart so each is its own maximal-munch token.
const CORPUS: &[&str] = &[
    // Original skeleton inputs (regression).
    "(ab, 12)",
    "foo(1, 2, 3)",
    "  spaced   out  ",
    "(())",
    "a1b2 999 snake_case_id",
    "",
    "x",
    "1000000",
    ",,,",
    "trailing   ",
    // Realistic code shapes.
    "fn add(a: i64, b: i64) -> i64 { a + b }",
    "let mut x = 1 + 2 * 3 - 4 / 5 % 6",
    "if a == b and c != d or not e { return x }",
    "for item in items { yield item }",
    "match v { _ => x }",
    "struct Foo { bar: Baz }",
    "pub fn f() where T: Bound",
    "arr[idx] = val",
    "data |> transform |> collect",
    "obj?.field ?? fallback",
    "Self::method path::sep",
    "true false self Self",
    // Operator munch coverage.
    "x <= y >= z < w > v",
    "a && b || !c",
    "p & q | r ^ s << t >> u ~ n",
    "x += 1 ; y -= 2 ; z *= 3 ; w /= 4 ; v %= 5",
    "a &= b ; c |= d ; e ^= f ; g <<= h ; i >>= j",
    "lo 0 .. 10 ..= 20 ... 30",
    "p -> q => r # s @ t",
    // Keyword coverage (every keyword the lexer table emits).
    "fn struct union enum trait marker impl mod use import const type distinct",
    "pub private if else match while for in loop return break continue",
    "defer errdefer try asm global_asm let mut and or not",
    "own ref weak lock move effect resource verb reads writes sends receives",
    "allocates panics blocks suspends with transparent stable seq par yield",
    "as where dyn requires ensures invariant unsafe extern shared layout group",
    "true false alias independent self Self",
    // Slice B: comments. Line/block comments skip (no token); `///` / `//!`
    // tokenize as DocComment / ModuleDocComment (body = rest of line, one
    // optional leading space stripped). Single-line only until the newline slice.
    "a // line comment here",
    "1 + 2 // trailing comment",
    "/// doc comment text",
    "///x",
    "//! module doc comment",
    "x /* block */ y",
    "a /* /* nested */ */ b",
    "let x = 1 /* inline */ + 2",
    "fn f() { /* body */ }",
    "p / q /= r",
    // Slice C: number forms — radix prefixes, floats, `_` separators, suffixes.
    "0xff 0x10 0xFF 0xdead",
    "0b1010 0b0 0b1111_0000",
    "0o777 0o17 0o0",
    "3.14 0.5 100.0 0.0",
    "1.5e3 2e10 1.0e-5 1.25e2",
    "1_000_000 0xff_ff 1_2_3",
    "5i32 10u8 100i64 255u8 7u32 9i8",
    "1.5f64 2.0f32 3.14f64",
    "let n = 42 + 0xa * 2",
    "0 1 12 999 1000000",
    "5f64",
    // Slice D: string / multi-string / char / byte literals (+ simple escapes).
    // Raw Rust strings so the entry IS the verbatim lexer input (incl. `"`/`\`).
    r#""hello""#,
    r#""a b c" 42"#,
    r#"x = "val" + "ue""#,
    r#""with \"quote\" in it""#,
    r#""tab\there" "ret\rurn""#,
    r#""line\nbreak""#,
    r#""back\\slash""#,
    r#""""#,
    r#""""triple quoted""""#,
    r#""""has "" inner quotes""""#,
    r#"'a' 'Z' '1' ' '"#,
    r#"'\n' '\t' '\r' '\\' '\''"#,
    r#"b'A' b'z' b'0' b'~'"#,
    r#"b'\n' b'\t' b'\\' b'\'' b'"'"#,
    r#"let s = "name: " + x"#,
    // Slice D-cont: f-string interpolation + c-strings.
    r#"f"hello {name}!""#,
    r#"f"{a + b} and {c}""#,
    r#"f"no holes""#,
    r#"f"""#,
    r#"f"nested {outer{inner}} done""#,
    r#"f"tab\there {x} end""#,
    r#"f"x={x[0]} y={obj.field}""#,
    r#"c"hello""#,
    r#"c"a\tb\n""#,
    r#"c"\x41\x42\x7e""#,
    r#"c"with \"quote\"""#,
    r#"let p = f"{a}" + c"x""#,
    // Slice E: raw idents, reserved string prefixes / `#`-guarded strings,
    // reserved future keywords, the `expr_<year>` fragment-specifier namespace,
    // and single-codepoint non-ASCII recovery. Error tokens render as bare
    // `ERROR`, so these assert SPAN parity (offset/length/line/column).
    // Raw identifiers `r#NAME` — payload is bare NAME, span covers `r#NAME`.
    "r#match r#type r#fn",
    "r#x + r#y",
    "let r#struct = 1",
    // Structural markers are not reservable → Error.
    "r#self r#mut r#ref",
    // Reserved single-letter string prefixes (`x"…"`, `_"…"`, `r"…"`); `f`/`c`
    // are the only recognized ones (covered in slice D-cont).
    "x\"abc\"",
    "_\"y\"",
    "r\"raw\"",
    "z\"esc \\\" end\"",
    "a + b\"\" + c",
    // Reserved `#`-guarded strings (Rust-style raw strings); `#[attr]` stays Pound.
    "#\"raw\"#",
    "##\"x\"##",
    "#\"unterminated",
    "#[derive]",
    "a #\"s\"# b",
    // Reserved future keywords (numeric types + reserved-for-future words).
    "f16 bf16",
    "gen async await comptime pure box",
    "become do final override priv typeof virtual",
    // Reserved `expr_<year>` fragment-specifier namespace vs ordinary idents.
    "expr_2026 expr_2050 expr_2099 expr_2020",
    "expr_2019 expr_2100 expr_abcd expr_99 express",
    "let expr_2030 = 1",
    // Single-codepoint non-ASCII recovery (each codepoint isolated by ASCII).
    "€",
    "π",
    "x € y",
    "1 + π",
    "λ + 1",
];

/// Render one Rust `SpannedToken` in the Kāra lexer's canonical one-line
/// format: `offset length line column KIND payload` (see `render` in
/// `selfhost/src/main.kara`). The KIND/lexeme strings here must match the
/// Kāra `render` arms byte-for-byte — that equality is the whole oracle.
fn render_rust(t: &SpannedToken) -> String {
    let s = &t.span;
    let body = match &t.token {
        // Keywords → `KW <lexeme>`.
        Token::Fn => "KW fn",
        Token::Struct => "KW struct",
        Token::Union => "KW union",
        Token::Enum => "KW enum",
        Token::Trait => "KW trait",
        Token::Marker => "KW marker",
        Token::Impl => "KW impl",
        Token::Mod => "KW mod",
        Token::Use => "KW use",
        Token::Import => "KW import",
        Token::Const => "KW const",
        Token::Type => "KW type",
        Token::Distinct => "KW distinct",
        Token::Pub => "KW pub",
        Token::Private => "KW private",
        Token::If => "KW if",
        Token::Else => "KW else",
        Token::Match => "KW match",
        Token::While => "KW while",
        Token::For => "KW for",
        Token::In => "KW in",
        Token::Loop => "KW loop",
        Token::Return => "KW return",
        Token::Break => "KW break",
        Token::Continue => "KW continue",
        Token::Defer => "KW defer",
        Token::ErrDefer => "KW errdefer",
        Token::Try => "KW try",
        Token::Asm => "KW asm",
        Token::GlobalAsm => "KW global_asm",
        Token::Let => "KW let",
        Token::Mut => "KW mut",
        Token::And => "KW and",
        Token::Or => "KW or",
        Token::Not => "KW not",
        Token::Own => "KW own",
        Token::Ref => "KW ref",
        Token::Weak => "KW weak",
        Token::Lock => "KW lock",
        Token::Move => "KW move",
        Token::Effect => "KW effect",
        Token::Resource => "KW resource",
        Token::Verb => "KW verb",
        Token::Reads => "KW reads",
        Token::Writes => "KW writes",
        Token::Sends => "KW sends",
        Token::Receives => "KW receives",
        Token::Allocates => "KW allocates",
        Token::Panics => "KW panics",
        Token::Blocks => "KW blocks",
        Token::Suspends => "KW suspends",
        Token::With => "KW with",
        Token::Transparent => "KW transparent",
        Token::Stable => "KW stable",
        Token::Seq => "KW seq",
        Token::Par => "KW par",
        Token::Yield => "KW yield",
        Token::As => "KW as",
        Token::Where => "KW where",
        Token::Dyn => "KW dyn",
        Token::Requires => "KW requires",
        Token::Ensures => "KW ensures",
        Token::Invariant => "KW invariant",
        Token::Unsafe => "KW unsafe",
        Token::Extern => "KW extern",
        Token::Shared => "KW shared",
        Token::Layout => "KW layout",
        Token::Group => "KW group",
        Token::True => "KW true",
        Token::False => "KW false",
        Token::Alias => "KW alias",
        Token::Independent => "KW independent",
        Token::SelfValue => "KW self",
        Token::SelfType => "KW Self",
        Token::Underscore => "OP _",
        // Delimiters / punctuation / operators → `OP <lexeme>`.
        Token::LeftParen => "OP (",
        Token::RightParen => "OP )",
        Token::LeftBrace => "OP {",
        Token::RightBrace => "OP }",
        Token::LeftBracket => "OP [",
        Token::RightBracket => "OP ]",
        Token::Colon => "OP :",
        Token::ColonColon => "OP ::",
        Token::Comma => "OP ,",
        Token::Semicolon => "OP ;",
        Token::Dot => "OP .",
        Token::DotDot => "OP ..",
        Token::DotDotEq => "OP ..=",
        Token::DotDotDot => "OP ...",
        Token::QuestionDot => "OP ?.",
        Token::QuestionQuestion => "OP ??",
        Token::Arrow => "OP ->",
        Token::FatArrow => "OP =>",
        Token::Question => "OP ?",
        Token::Pound => "OP #",
        Token::At => "OP @",
        Token::Plus => "OP +",
        Token::Minus => "OP -",
        Token::Star => "OP *",
        Token::Slash => "OP /",
        Token::Percent => "OP %",
        Token::EqualEqual => "OP ==",
        Token::BangEqual => "OP !=",
        Token::LessThan => "OP <",
        Token::LessThanOrEqual => "OP <=",
        Token::GreaterThan => "OP >",
        Token::GreaterThanOrEqual => "OP >=",
        Token::AmpAmp => "OP &&",
        Token::PipePipe => "OP ||",
        Token::Bang => "OP !",
        Token::Amp => "OP &",
        Token::Pipe => "OP |",
        Token::PipeArrow => "OP |>",
        Token::Caret => "OP ^",
        Token::Tilde => "OP ~",
        Token::LessLess => "OP <<",
        Token::GreaterGreater => "OP >>",
        Token::Equal => "OP =",
        Token::PlusEqual => "OP +=",
        Token::MinusEqual => "OP -=",
        Token::StarEqual => "OP *=",
        Token::SlashEqual => "OP /=",
        Token::PercentEqual => "OP %=",
        Token::AmpEqual => "OP &=",
        Token::PipeEqual => "OP |=",
        Token::CaretEqual => "OP ^=",
        Token::LessLessEqual => "OP <<=",
        Token::GreaterGreaterEqual => "OP >>=",
        // Literals / special.
        Token::Identifier { name, .. } => return body_with(s, &format!("IDENT {name}")),
        Token::Integer(v, _) => return body_with(s, &format!("INT {v}")),
        // Display for f64 matches Kāra's f64.to_string (both use Rust's
        // formatter): 100.0→"100", 1.0e-5→"0.00001", etc. The suffix is
        // ignored on both sides (the port consumes but does not yet store it).
        Token::Float(v, _) => return body_with(s, &format!("FLOAT {v}")),
        // String / char values go through escape_for_render (shared with the
        // Kāra `render`) so control chars don't break the line-based compare.
        Token::StringLiteral(v) => return body_with(s, &format!("STR {}", escape_for_render(v))),
        Token::MultiStringLiteral(v) => {
            return body_with(s, &format!("MSTR {}", escape_for_render(v)))
        }
        Token::InterpolatedStringLiteral(parts) => {
            let mut b = "FSTR".to_string();
            for p in parts {
                match p {
                    InterpolationPart::Text(t) => {
                        b.push_str(" T:");
                        b.push_str(&escape_for_render(t));
                    }
                    InterpolationPart::Expr {
                        raw,
                        offset,
                        line,
                        column,
                    } => {
                        b.push_str(&format!(
                            " E:{offset}:{line}:{column}:{}",
                            escape_for_render(raw)
                        ));
                    }
                }
            }
            return body_with(s, &b);
        }
        Token::CStringLiteral { bytes, source_len } => {
            let mut b = format!("CSTR {source_len}");
            for byte in bytes {
                b.push_str(&format!(" {byte}"));
            }
            return body_with(s, &b);
        }
        Token::CharLiteral(c) => {
            return body_with(s, &format!("CHAR {}", escape_for_render(&c.to_string())))
        }
        Token::ByteLiteral(b) => return body_with(s, &format!("BYTE {b}")),
        Token::DocComment(t) => return body_with(s, &format!("DOC {t}")),
        Token::ModuleDocComment(t) => return body_with(s, &format!("MODDOC {t}")),
        // Error tokens (slice E: raw-ident structural markers, reserved string
        // prefixes / `#`-guarded strings, reserved future keywords, reserved
        // fragment-specifier idents, non-ASCII recovery). The Kāra `render`
        // discards the message and emits a bare `ERROR`, so only the SPAN is
        // compared — each error path must consume the identical byte extent.
        Token::Error(_) => "ERROR",
        Token::EOF => "EOF",
        // The match is now exhaustive over every Token the seed lexer emits — the
        // port models the full token set (slices A–E). A new seed variant fails
        // to compile here until rendered, which is a stronger guarantee than the
        // former runtime catch-all panic.
    };
    body_with(s, body)
}

/// Prefix the span coordinates onto a rendered token body.
fn body_with(s: &karac::token::Span, body: &str) -> String {
    format!("{} {} {} {} {}", s.offset, s.length, s.line, s.column, body)
}

/// Escape a string/char value to a single-line, unambiguous form. MUST stay
/// identical to `escape_for_render` in `selfhost/src/main.kara`.
fn escape_for_render(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\n' => {
                out.push('\\');
                out.push('n');
            }
            '\t' => {
                out.push('\\');
                out.push('t');
            }
            '\r' => {
                out.push('\\');
                out.push('r');
            }
            '\\' => {
                out.push('\\');
                out.push('\\');
            }
            _ => out.push(c),
        }
    }
    out
}

#[test]
fn selfhost_lexer_matches_rust_lexer() {
    // 1. Build the generated program: the lexer library (everything in
    //    `selfhost/src/main.kara` except its driver `fn main`) + a `main` that
    //    lexes each corpus input and prints its token render, separated by SEP.
    let lib_src = {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("selfhost/src/main.kara");
        let full = std::fs::read_to_string(&p).expect("read selfhost lexer source");
        let cut = full
            .rfind("\nfn main(")
            .expect("selfhost lexer source has a driver `fn main`");
        full[..cut].to_string()
    };

    let mut prog = lib_src;
    prog.push_str(
        "\n\
         fn lex_and_print(src: String) {\n\
         \x20   let toks = lex_all(src);\n\
         \x20   for t in toks { println(render(t)); }\n\
         }\n\
         fn main() {\n",
    );
    for input in CORPUS {
        // Inputs may now contain `"` and `\` (string/char literal tests); they
        // are escaped here so the embedded Kāra string literal reconstructs the
        // exact input. Newlines still can't be embedded single-line, so the
        // corpus stays newline-free until the newline slice.
        assert!(
            !input.contains('\n'),
            "corpus input must be single-line (no newline): {input:?}"
        );
        let escaped = input.replace('\\', "\\\\").replace('"', "\\\"");
        prog.push_str(&format!("    lex_and_print(\"{escaped}\");\n"));
    }
    prog.push_str("}\n");

    // 2. Build via `karac build` (AOT — the interpreter mishandles the
    //    lexer's self-mutating methods).
    let tmp = std::env::temp_dir().join(format!("karac-selfhost-lexer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("lex.kara"), &prog).unwrap();

    let build = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&tmp)
        .args(["build", "lex.kara"])
        .env_remove("KARAC_RUNTIME")
        .output()
        .expect("spawn karac build");
    let berr = String::from_utf8_lossy(&build.stderr);
    let bin = tmp.join("lex");

    if !bin.exists() {
        // A real COMPILE failure (typecheck / codegen / parse / verifier) is a
        // port regression — fail loudly with the generated source. Anything
        // else (no-llvm gate, or a link failure from a missing runtime archive)
        // soft-skips like the rest of the E2E suite.
        let compile_err = berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted lexer FAILED TO COMPILE (port regression):\n{berr}\n\
             --- generated source ---\n{prog}"
        );
        eprintln!(
            "skip: selfhost_lexer_matches_rust_lexer — lexer did not link \
             (no llvm feature / missing runtime archive); stderr:\n{berr}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }

    // 3. Run the Kāra lexer.
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run kara lexer binary");
    assert!(
        run.status.success(),
        "kara lexer binary exited nonzero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let kout = String::from_utf8_lossy(&run.stdout);
    let kara_lines: Vec<String> = kout
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    // 4. Expected = the Rust lexer's render of every input, concatenated in
    //    corpus order (each input including its trailing EOF).
    // `trim_end` on both sides (kara_lines is already trimmed above): a doc-
    // comment body is the verbatim rest of the line, so a trailing space in the
    // body would otherwise produce an asymmetric trailing space here. The corpus
    // keeps doc bodies free of trailing whitespace, so this only guards against
    // an accidental asymmetry — it does not mask a real token-text divergence.
    let mut rust_lines: Vec<String> = Vec::new();
    for input in CORPUS {
        rust_lines.extend(
            karac::tokenize(input)
                .iter()
                .map(|t| render_rust(t).trim_end().to_string()),
        );
    }

    // Pinpoint the first divergence for a legible failure (the full vectors
    // are ~50 lines and assert_eq would dump both).
    if let Some((i, (k, r))) = kara_lines
        .iter()
        .zip(rust_lines.iter())
        .enumerate()
        .find(|(_, (k, r))| k != r)
    {
        panic!(
            "self-hosted lexer diverged from the Rust lexer at token line {i}:\n  \
             Kāra: {k:?}\n  Rust: {r:?}\n--- full Kāra output ---\n{kout}"
        );
    }
    assert_eq!(
        kara_lines.len(),
        rust_lines.len(),
        "token-count mismatch (Kāra {} vs Rust {}) — one lexer emitted extra/fewer tokens\n\
         --- full Kāra output ---\n{kout}",
        kara_lines.len(),
        rust_lines.len()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
