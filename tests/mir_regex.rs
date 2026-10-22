//! `std.regex` on the v2 backends (B-2026-10-14-63): the methods in
//! runtime/stdlib/source/regex.kara over the `RegexEngine` natives, the
//! `regex` crate on both backends. Each program runs on the MIR interpreter
//! and, compiled, against the opt-in regex archive; both must print the
//! expected output, which is what the crate gives.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("karac-mir-regex-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The program's output on the compiled backend at `opt`; `None` when the
/// runtime archive is absent.
fn build_and_run(src: &str, opt: u8) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::codegen::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("utf-8"))
}

/// `src` prints `want` on the interpreter and, compiled, at -O0 and -O2.
fn agrees(src: &str, want: &str) {
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "on the interpreter");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "compiled at -O{opt}");
        }
    }
}

/// Every method of the surface: `compile` (and its error message),
/// `is_match`, `find`, `find_all`, `replace` and `replace_all` with group
/// references, `captures` and `captures_all` with numbered and named
/// groups, `split`, `escape`, and byte offsets around a two-byte character.
#[test]
fn regex_surface_agrees_on_both_backends() {
    agrees(SURFACE, WANT_SURFACE);
}

/// Empty matches follow the crate's iterators: one that ends where the
/// previous match ended is skipped, and an empty pattern matches between
/// characters, not inside one.
#[test]
fn regex_empty_matches_follow_the_crate() {
    agrees(EMPTY, WANT_EMPTY);
}

/// The compiled program has no leak, double free or use-after-free under
/// the ASan lane.
#[test]
fn regex_surface_is_clean_under_asan() {
    let dir = Scratch::new();
    let src = dir.path().join("surface.kara");
    std::fs::write(&src, SURFACE).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&src)
        .output()
        .expect("karac");
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("not found") || err.contains("fsanitize") {
        eprintln!("skipping: no ASan toolchain or runtime archive: {err}");
        return;
    }
    assert!(!err.contains("Sanitizer"), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT_SURFACE, "{err}");
}

/// `karac check` reads the v2 surface for a program that names `Regex`,
/// so `captures` and `Captures.name` check.
#[test]
fn regex_v2_surface_checks() {
    let dir = Scratch::new();
    let src = dir.path().join("check.kara");
    std::fs::write(&src, SURFACE).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&src)
        .output()
        .expect("karac");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A program that declares its own `Regex` keeps it: the library is not
/// appended.
#[test]
fn a_program_with_its_own_regex_keeps_it() {
    agrees(
        "struct Regex { p: String }\n\
         impl Regex {\n    fn compile(p: Str) -> Regex { Regex { p: p.to_string() } }\n    \
         fn captures(ref self) -> i64 { self.p.len() }\n}\n\
         fn main() {\n    println(f\"{Regex.compile(\"abc\").captures()}\");\n}\n",
        "3\n",
    );
}

const SURFACE: &str = r###"import std.regex.{Regex};

fn main() {
    match Regex.compile("err(or)?:\\s+(\\w+)") {
        Ok(re) => {
            println(f"{re.is_match("x error:  disk")}");
            match re.find("x error:  disk") {
                Some(m) => println(f"{m.start} {m.end} {m.text}"),
                None => println("none"),
            }
            println(re.replace_all("err: a, error: b", "E"));
            println(re.replace("err: a, error: b", "[$2]"));
            for m in re.find_all("err: a, error: b, nope") {
                println(f"all {m.start}..{m.end} {m.text}");
            }
            match re.captures("x error:  disk") {
                Some(c) => {
                    println(f"len {c.len()}");
                    match c.get(1) { Some(m) => println(f"g1 {m.text}"), None => println("g1 none") }
                    match c.get(2) { Some(m) => println(f"g2 {m.text} {m.start}"), None => println("g2 none") }
                    match c.get(9) { Some(_) => println("g9 some"), None => println("g9 none") }
                }
                None => println("no caps"),
            }
        }
        Err(e) => println(f"bad {e.message}"),
    }
    match Regex.compile("(?P<y>\\d{4})-(?P<m>\\d\\d)") {
        Ok(re) => {
            for c in re.captures_all("2024-01 and 1999-12") {
                match (c.name("y"), c.name("m"), c.name("zz")) {
                    (Some(y), Some(m), None) => println(f"{y.text}/{m.text}"),
                    _ => println("?"),
                }
            }
            println(re.replace_all("2024-01", "${m}.${y} $$"));
        }
        Err(e) => println(f"bad {e.message}"),
    }
    match Regex.compile("a*") {
        Ok(re) => {
            for m in re.find_all("baaab") { println(f"e {m.start}..{m.end}"); }
            for p in re.split("baaab") { println(f"p [{p}]"); }
        }
        Err(_) => println("bad"),
    }
    match Regex.compile(",\\s*") {
        Ok(re) => { for p in re.split("a, b,c,") { println(f"s [{p}]"); } }
        Err(_) => println("bad"),
    }
    match Regex.compile("(unclosed") {
        Ok(_) => println("ok?"),
        Err(e) => println(e.message),
    }
    println(Regex.escape("a.b*c"));
    match Regex.compile("é|x") {
        Ok(re) => { for m in re.find_all("aéx") { println(f"u {m.start}..{m.end} {m.text}"); } }
        Err(_) => println("bad"),
    }
}
"###;

const WANT_SURFACE: &str = r###"true
2 14 error:  disk
E, E
[a], error: b
all 0..6 err: a
all 8..16 error: b
len 3
g1 or
g2 disk 10
g9 none
2024/01
1999/12
01.2024 $
e 0..0
e 1..4
e 5..5
p []
p [b]
p [b]
p []
s [a]
s [b]
s [c]
s []
regex parse error:
    (unclosed
    ^
error: unclosed group
a\.b\*c
u 1..3 é
u 3..4 x
"###;

const EMPTY: &str = r###"import std.regex.{Regex};

fn main() {
    match Regex.compile("a*") {
        Ok(re) => { for m in re.find_all("baaab") { println(f"e {m.start}..{m.end}"); } }
        Err(_) => println("bad"),
    }
    match Regex.compile("") {
        Ok(re) => { for m in re.find_all("aé") { println(f"z {m.start}..{m.end}"); } println(re.replace_all("aé", "-")); }
        Err(_) => println("bad"),
    }
}
"###;

const WANT_EMPTY: &str = r###"e 0..0
e 1..4
e 5..5
z 0..0
z 1..1
z 3..3
-a-é-
"###;

/// B-2026-10-21-52: library pruning keeps only the methods a program names,
/// and check used to probe for `captures` to tell the v2 surface from the
/// legacy one, so a program that never wrote that word got the legacy
/// four methods: `replace` and `split` were "no method", and `is_match` of
/// a `Str` was a type error.
#[test]
fn regex_v2_surface_checks_without_captures() {
    let dir = Scratch::new();
    let src = dir.path().join("check.kara");
    std::fs::write(
        &src,
        "import std.regex.{Regex};\n\nfn main() {\n    let s: Str = \"a1b22\";\n    match Regex.compile(\"[0-9]+\") {\n        \
         Ok(re) => {\n            println(re.replace(s, \"#\"));\n            \
         println(f\"{re.split(s).len()} {re.is_match(s)}\");\n        }\n        \
         Err(e) => println(e.message),\n    }\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&src)
        .output()
        .expect("karac");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
