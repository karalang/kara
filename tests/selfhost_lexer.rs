//! Differential oracle for the self-hosted Kāra lexer (phase-12 self-hosting).
//!
//! Lexes a shared corpus with BOTH the Rust `karac::tokenize` (the bootstrap
//! seed + spec) and the Kāra lexer in `selfhost/src/main.kara` (built via
//! `karac build` — AOT, because the interpreter can't run self-mutating
//! methods, self-hosting blocker #2), and asserts the two token streams render
//! identically. This is the bootstrap oracle: as the port grows, any
//! divergence from the Rust lexer fails here.
//!
//! Restricted to the skeleton's current token set: left/right paren, comma,
//! identifiers, integers, whitespace, EOF. Inputs are single-line (so the
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

use karac::token::{SpannedToken, Token};
use std::path::PathBuf;

/// Inputs exercising only the skeleton's token set. Plain ASCII, no quotes /
/// backslashes / newlines (so they embed verbatim as Kāra string literals).
const CORPUS: &[&str] = &[
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
];

/// Render one Rust `SpannedToken` in the Kāra lexer's canonical one-line
/// format: `offset length line column KIND payload` (see `render` in
/// `selfhost/src/main.kara`).
fn render_rust(t: &SpannedToken) -> String {
    let s = &t.span;
    let body = match &t.token {
        Token::LeftParen => "OP (".to_string(),
        Token::RightParen => "OP )".to_string(),
        Token::Comma => "OP ,".to_string(),
        Token::Identifier { name, .. } => format!("IDENT {name}"),
        Token::Integer(v, _) => format!("INT {v}"),
        Token::EOF => "EOF".to_string(),
        other => panic!(
            "corpus input produced a token the lexer skeleton does not model \
             ({other:?}); keep the corpus within `( ) , ident int ws`"
        ),
    };
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
