//! `karac fmt` over every program in `corpus/`: the output parses, formats to
//! itself, and means what the input meant. The printer reprints the AST, so
//! each construct it spelled differently from the parser (a char escape, a
//! float, an f-string brace, `*const`, a contract after `where`, a block-like
//! operand at the start of a statement, ...) came out as a file that no
//! longer parsed, or parsed as something else; about 3% of the corpus did
//! (B-2026-10-30-29). The corpus is the sample of real Kāra, so a construct
//! the printer gets wrong next shows up here first.

use std::path::{Path, PathBuf};

fn corpus_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            corpus_sources(&p, out);
        } else if p.file_name().is_some_and(|n| n == "source.kara") {
            out.push(p);
        }
    }
}

fn corpus(sub: &str) -> Vec<(PathBuf, String)> {
    let mut paths = Vec::new();
    corpus_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join(sub), &mut paths);
    paths
        .into_iter()
        .map(|p| {
            let s = std::fs::read_to_string(&p).unwrap();
            (p, s)
        })
        .collect()
}

/// `f` over `items` on every core, each worker with a deep stack: the front
/// end recurses on nesting.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let n = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = items.len().div_ceil(n).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|c| {
                let f = &f;
                std::thread::Builder::new()
                    .stack_size(64 << 20)
                    .spawn_scoped(s, move || c.iter().map(f).collect::<Vec<_>>())
                    .unwrap()
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    })
}

/// Diagnostics as phase and message with every number masked, sorted:
/// formatting moves code between lines, and a message can name a position
/// (`moved at line 5:13`).
fn diagnostics(source: &str) -> Vec<String> {
    let mut d: Vec<String> = karac::check_source(source)
        .into_iter()
        .map(|d| {
            let mut m = format!("{}: ", d.phase);
            for c in d.message.chars() {
                if !c.is_ascii_digit() {
                    m.push(c);
                } else if !m.ends_with('#') {
                    m.push('#');
                }
            }
            m
        })
        .collect();
    d.sort();
    d
}

#[test]
fn every_corpus_program_formats_to_a_fixed_point() {
    let files = corpus("corpus");
    let results = par_map(&files, |(path, src)| {
        let once = karac::format_source(src)?;
        let failure = match karac::format_source(&once) {
            None => "the formatted program does not parse",
            Some(twice) if twice != once => "a second format changes it",
            Some(_) => return Some(None),
        };
        Some(Some(format!("{}: {failure}", path.display())))
    });
    let formatted = results.iter().flatten().count();
    let failures: Vec<_> = results.into_iter().flatten().flatten().collect();
    assert!(
        formatted > 15_000,
        "only {formatted} corpus programs parsed"
    );
    assert!(
        failures.is_empty(),
        "{} of {formatted}:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_corpus_program_checks_the_same_after_formatting() {
    let files = corpus("corpus");
    let failures: Vec<String> = par_map(&files, |(path, src)| {
        let once = karac::format_source(src)?;
        let (before, after) = (diagnostics(src), diagnostics(&once));
        (before != after).then(|| format!("{}: {before:?} became {after:?}", path.display()))
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(
        failures.is_empty(),
        "{}:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn formatting_a_core_pin_or_drop_program_keeps_its_output() {
    // The core-semantics pins exercise every rule of the core language, and
    // the drop matrix every value shape in every position: each one prints
    // the same output, in the same order, after formatting.
    let mut files = corpus("corpus/core");
    files.extend(corpus("corpus/drop-matrix"));
    assert!(files.len() > 300, "{} programs", files.len());
    let failures: Vec<String> = par_map(&files, |(path, src)| {
        let once = karac::format_source(src)?;
        let (before, after) = (diagnostics(src), diagnostics(&once));
        if before != after {
            return Some(format!(
                "{}: diagnostics {before:?} became {after:?}",
                path.display()
            ));
        }
        if !before.is_empty() {
            return None;
        }
        let (a, b) = (karac::run_playground(src), karac::run_playground(&once));
        (a.stdout != b.stdout).then(|| {
            format!(
                "{}: output {:?} became {:?}",
                path.display(),
                a.stdout,
                b.stdout
            )
        })
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn fmt_writes_back_what_the_parser_keeps_beside_the_tree() {
    // A check that passes cannot see these: a dropped `#[serde(rename)]`
    // renames a JSON key, a dropped hasher changes a map's order, a dropped
    // `#[allow]` or `#![..]` only moves a warning, and `u128::MAX` printed
    // as its signed carrier `-1` is a different value.
    let src = "#![rc_budget(max: 4)]

#[derive(Serialize)]
struct Cfg {
    #[serde(rename: \"server_name\")]
    name: String,
}

enum Mode {
    #[default]
    Off,
    On,
}

type Small = i64 where self < 10;

fn main() {
    let mut m: Map[i64, i64, FxBuildHasher] = Map.new();
    let n = Map[i64, i64, FxBuildHasher].new();
    #[allow(unused_variable)]
    let x = 1;
    let big = 340282366920938463463374607431768211455u128;
    let s = u32.size_of();
    let v = Vector[i64, 4](0, 3, -1, 255);
    println(f\"{ { s } }\");
}
";
    let once = karac::format_source(src).unwrap();
    for want in [
        "#![rc_budget(max: 4)]\n",
        "    #[serde(rename: \"server_name\")]\n    name: String,\n",
        "    #[default]\n    Off,\n",
        "type Small = i64 where self < 10;\n",
        "let mut m: Map[i64, i64, FxBuildHasher] = Map.new();\n",
        "let n = Map[i64, i64, FxBuildHasher].new();\n",
        "    #[allow(unused_variable)]\n    let x = 1;\n",
        "340282366920938463463374607431768211455u128",
        "let s = u32.size_of();\n",
        "Vector[i64, 4](0, 3, -1, 255)",
        "println(f\"{ {\n",
    ] {
        assert!(once.contains(want), "missing {want:?} in:\n{once}");
    }
    assert_eq!(karac::format_source(&once).as_deref(), Some(once.as_str()));
}

#[test]
fn fmt_keeps_a_literals_escapes() {
    // A doubled brace, a newline char and a quote char came out as `{y}`,
    // a raw line break between quotes and `'''` (B-2026-10-30-29).
    let src = "fn main() {
    let a = f\"x{1} {{y}} \\u{4}\";
    let b = '\\n';
    let c = '\\'';
    let d = \"tab\\there\\0\";
    let e = 1.5e300;
    let f = 3.0;
    println(f\"{a}{b}{c}{d}{e}{f}\");
}
";
    let once = karac::format_source(src).unwrap();
    assert_eq!(once, src);
}
