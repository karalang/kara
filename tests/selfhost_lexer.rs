//! Differential oracle for the self-hosted Kāra lexer (phase-12 self-hosting).
//!
//! Lexes a shared corpus with BOTH the Rust `karac::tokenize` (the bootstrap
//! seed + spec) and the Kāra lexer in `selfhost/src/main.kara` (built via
//! `karac build` — AOT, because the interpreter can't run self-mutating
//! methods, self-hosting blocker #2), and asserts the two token streams render
//! identically. This is the bootstrap oracle: as the port grows, any
//! divergence from the Rust lexer fails here.
//!
//! Covers the port's slice-A token set: all delimiters, punctuation, single-
//! and multi-char operators (maximal-munch forms like `<<=` / `..=` / `?.`),
//! the full keyword table, identifiers, decimal integers, whitespace, and EOF.
//! Deferred to later slices (and kept OUT of the corpus): comments, string /
//! char / byte / interpolated / c-string literals, non-decimal and suffixed /
//! float numbers, raw identifiers (`r#x`), non-ASCII, and the reserved-word /
//! reserved-prefix error forms. Inputs are single-line (so the reported line
//! is always one) until both the port and the corpus grow newlines. Both
//! lexers emit a trailing EOF, so the full streams (including EOF) are
//! compared.
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

use karac::token::{SpannedToken, Token};
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
        Token::EOF => "EOF",
        other => panic!(
            "corpus input produced a token the slice-A lexer does not model \
             ({other:?}); keep the corpus within delimiters / punctuation / \
             operators / keywords / ident / decimal-int / ws"
        ),
    };
    body_with(s, body)
}

/// Prefix the span coordinates onto a rendered token body.
fn body_with(s: &karac::token::Span, body: &str) -> String {
    format!("{} {} {} {} {}", s.offset, s.length, s.line, s.column, body)
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
        assert!(
            !input.contains(['"', '\\', '\n']),
            "corpus input must be plain ASCII (no quote/backslash/newline): {input:?}"
        );
        prog.push_str(&format!("    lex_and_print(\"{input}\");\n"));
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
    let mut rust_lines: Vec<String> = Vec::new();
    for input in CORPUS {
        rust_lines.extend(karac::tokenize(input).iter().map(render_rust));
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
