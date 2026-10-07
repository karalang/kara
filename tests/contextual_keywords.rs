//! Contextual keywords (design review 2026-10-07 § 3). The effect verbs and the
//! other soft words are keywords only where they start their construct, and
//! ordinary names everywhere else: fields, bindings, functions, methods.

use karac::parse;
use karac::token::Token;

/// The 24 words that stopped being reserved.
const SOFT_WORDS: &[&str] = &[
    "union",
    "marker",
    "mod",
    "use",
    "weak",
    "lock",
    "resource",
    "verb",
    "reads",
    "writes",
    "sends",
    "receives",
    "allocates",
    "panics",
    "blocks",
    "suspends",
    "transparent",
    "stable",
    "seq",
    "par",
    "layout",
    "group",
    "alias",
    "independent",
];

/// Parses `source`, asserting no errors; returns the program's debug dump so
/// a test can check which construct a word became.
fn parse_ok(source: &str) -> String {
    let result = parse(source);
    assert!(
        result.errors.is_empty(),
        "expected no parse errors for:\n{source}\ngot: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    format!("{:?}", result.program)
}

fn assert_has(dump: &str, needle: &str, source: &str) {
    assert!(
        dump.contains(needle),
        "`{source}` should parse to a {needle}.."
    );
}

#[test]
fn every_soft_word_lexes_as_an_identifier() {
    for w in SOFT_WORDS {
        let toks = karac::tokenize(w);
        assert!(
            matches!(&toks[0].token, Token::Identifier { name, raw: false } if name == w),
            "`{w}` should lex as an identifier, got {:?}",
            toks[0].token
        );
    }
}

#[test]
fn every_soft_word_is_a_binding_a_parameter_and_a_function_name() {
    for w in SOFT_WORDS {
        parse_ok(&format!(
            "fn main() {{ let {w} = 1; let mut y = {w} + 1; y = y + {w}; }}"
        ));
        parse_ok(&format!("fn f({w}: i64) -> i64 {{ {w} * 2 }}"));
        parse_ok(&format!(
            "fn {w}(x: i64) -> i64 {{ x }}\nfn main() {{ let _ = {w}(3); }}"
        ));
    }
}

#[test]
fn every_soft_word_is_a_field_and_a_method_name() {
    for w in SOFT_WORDS {
        parse_ok(&format!(
            "struct S {{ {w}: i64 }}\n\
             impl S {{ fn {w}(ref self) -> i64 {{ self.{w} }} }}\n\
             fn main() {{ let s = S {{ {w}: 1 }}; let _ = s.{w}(); let _ = s.{w}; }}"
        ));
    }
}

/// The motivating case: an I/O counter with fields named after effect verbs.
#[test]
fn effect_verbs_name_fields_and_still_declare_effects() {
    let src = "\
effect resource Disk;

struct IoStats {
    reads: i64,
    writes: i64,
    blocks: i64,
}

fn total(s: ref IoStats) -> i64 {
    s.reads + s.writes + s.blocks
}

pub fn load(path: String) -> i64 with reads(Disk) blocks {
    let reads = 3;
    reads
}

fn main() {
    let s = IoStats { reads: 1, writes: 2, blocks: 3 };
    println(f\"{total(s)}\");
}
";
    parse_ok(src);
    // Through every phase, not just the parser: the resolver and checkers
    // must see `reads` and `writes` as ordinary field names.
    let dir = std::env::temp_dir().join(format!("karac-ckw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("io.kara");
    std::fs::write(&path, src).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "karac run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "6\n");
}

/// Where the word starts its construct it is still the keyword.
#[test]
fn soft_words_still_start_their_constructs() {
    for (src, construct) in [
        ("union Bits { i: i64, f: f64 }", "UnionDef("),
        ("marker trait Tag;", "MarkerTrait("),
        ("par struct Counter { mut n: Atomic[i64] }", "is_par: true"),
        ("par enum Msg { Ping, Data(i64) }", "is_par: true"),
        ("shared struct Node { parent: Option[weak Node] }", "Weak("),
        ("fn main() { let a = 1; par { let x = a; } }", "Par("),
        ("fn main() { seq { let z = 3; } }", "Seq("),
        (
            "fn f() with writes(Db) reads(Cache) allocates panics { }",
            "Writes",
        ),
    ] {
        assert_has(&parse_ok(src), construct, src);
    }
}

/// `r#` on a former keyword is now redundant but stays legal, so code that
/// escaped these words keeps compiling.
#[test]
fn raw_escape_of_a_soft_word_is_still_accepted() {
    parse_ok("fn main() { let r#union = 1; let r#reads = r#union + 1; }");
}
