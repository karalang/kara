//! `corpus/diagnostics/<code>/`: for each diagnostic code the corpus emits,
//! the smallest program that produces it (B-2026-11-16-303), which is the
//! example behind `karac explain <code>` (B-2026-11-16-296).
//!
//! Each directory is named for its code in lower case. The test checks that
//! `karac check --output=json` still reports that code for the program, and
//! that `karac explain` gives the code a description of its own rather than
//! the "no class page yet" stub. The corpus runner runs the same entries on
//! every backend.

use std::process::{Command, Stdio};

fn karac(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn diagnostics_corpus_programs_produce_their_code_and_are_explained() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/diagnostics");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut bad = Vec::new();
    for dir in &dirs {
        let code = dir.file_name().unwrap().to_str().unwrap().to_uppercase();
        // The f16 lint judges the target's CPU, and an Apple Silicon host
        // runs f16 natively (nothing to report), so its two pins are checked
        // for a target whose baseline is the same on every host. `check`
        // reads no CPU override: `KARAC_TARGET_CPU` is `build`'s.
        let json = if matches!(code.as_str(), "E0311" | "W0311") {
            karac(
                dir,
                &[
                    "check",
                    "--targets=wasm_wasi",
                    "--output=json",
                    "source.kara",
                ],
            )
        } else {
            karac(dir, &["check", "--output=json", "source.kara"])
        };
        // A numeric code may be the `parent_code` of a more specific one.
        if !json.contains(&format!("\"code\":\"{code}\""))
            && !json.contains(&format!("\"parent_code\":\"{code}\""))
        {
            bad.push(format!(
                "{code}: `karac check` no longer reports it:\n{json}"
            ));
        }
        let explained = karac(dir, &["explain", &code]);
        if explained.contains("no class page yet") || !explained.contains(&code) {
            bad.push(format!(
                "{code}: `karac explain` has no description:\n{explained}"
            ));
        }
    }
    // A void run (the directory moved, nothing matched) must not pass.
    assert!(
        dirs.len() >= 81,
        "only {} entries under {root:?}",
        dirs.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

/// B-2026-11-16-297: a code for a later-track or removed construct is not
/// described by the class it is filed under.
#[test]
fn diagnostics_corpus_later_track_codes_get_the_scope_paragraph() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for code in [
        "E_ARCHIVED",
        "E_DEFERRED_M4A",
        "E_TRY_BLOCK_NOT_IMPLEMENTED_YET",
        "E_TAIT_NOT_IMPLEMENTED_YET",
        "E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET",
        "E_DEFERRED_EFFECT_GROUP",
        "E_DEFERRED_LAYOUT_BLOCK",
        "E0520",
    ] {
        let out = karac(dir, &["explain", code]);
        assert!(
            out.contains("not in the language this compiler implements"),
            "{code}: {out}"
        );
        assert!(
            !out.contains("Operand, argument, or return-value type")
                && !out.contains("Method-call dispatch found no matching impl"),
            "{code} still prints an unrelated class paragraph: {out}"
        );
        let json = karac(dir, &["explain", "--format=json", code]);
        assert!(
            json.contains("not in the language this compiler implements"),
            "{code}: {json}"
        );
    }
}

/// B-2026-11-16-297: a symbolic code with no prose of its own says that its
/// class paragraph is the class's, not the code's.
#[test]
fn diagnostics_corpus_symbolic_code_frames_its_class_paragraph() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = karac(dir, &["explain", "E_INDEX_MOVE_NON_COPY"]);
    assert!(
        out.contains("filed under the TYPE_MISMATCH class, which gathers many"),
        "{out}"
    );
    let e0216 = karac(dir, &["explain", "E0216"]);
    assert!(e0216.contains("labeled arguments"), "{e0216}");
    assert!(!e0216.contains("Operand, argument"), "{e0216}");
}

/// B-2026-11-17-77: `Point.make()` where two traits `Point` implements each
/// declare `make` is ambiguous (corpus/diagnostics/e0233), but an inherent
/// `make` is found first, and a trait named with the expected type picks its
/// own impl.
#[test]
fn diagnostics_corpus_inherent_assoc_fn_is_not_ambiguous() {
    let dir = tempfile_dir("assoc_inherent");
    std::fs::write(
        dir.join("main.kara"),
        "trait Origin { fn make() -> Self; }
trait Unit { fn make() -> Self; }
struct Point { x: i64 }
impl Origin for Point { fn make() -> Point { Point { x: 0 } } }
impl Unit for Point { fn make() -> Point { Point { x: 1 } } }
impl Point { fn make() -> Point { Point { x: 2 } } }
fn main() {
    let a: Point = Unit.make();
    let b = Point.make();
    println(f\"{a.x} {b.x}\");
}
",
    )
    .unwrap();
    let out = karac(&dir, &["check", "--output=json", "main.kara"]);
    assert!(!out.contains("\"code\":\"E0233\""), "{out}");
    assert!(!out.contains("\"severity\":\"error\""), "{out}");
}

fn tempfile_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac_diag_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Review-thread ruling 2026-11-17 on B-2026-11-16-303: codes with no
/// reachable trigger are retired (the number stays reserved and `explain`
/// names what reports the case now), and codes for deferred features print
/// the scope paragraph with a pointer to where the spec says so.
#[test]
fn diagnostics_corpus_retired_and_deferred_codes_say_so() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for (code, now) in [
        ("E0103", "E0111"),
        ("E0105", "E0236"),
        ("E0220", "E0002"),
        ("E0274", "E0275"),
        ("E0277", "?."),
        ("E0401", "may declare effects"),
        ("E0417", "may panic"),
        ("E0508", "E0515"),
        ("E_ENUM_NESTED_ENUM_PAYLOAD", "any depth"),
        ("E_MODULE_BINDING_NAMING", "E_MODULE_LET"),
        (
            "E_REPR_TRANSPARENT_ENUM_NOT_SINGLE_VARIANT",
            "E_REPR_TRANSPARENT_ON_ENUM",
        ),
        ("W0281", "E_MODULE_LET"),
        ("W0313", "E0313"),
        ("W0314", "E0314"),
        ("W0315", "E0289"),
        ("W0316", "E0316"),
        ("E_CONCURRENT_SHARED_STRUCT", "E_NOT_CROSS_TASK"),
        ("E_CONCURRENT_PLAIN_STRUCT", "E0418"),
        ("E0281", "E_MODULE_LET"),
        ("E0315", "E0289"),
        ("E0408", "E_MODULE_LET"),
        ("E0502", "E0500"),
        ("E_RC_BUDGET_EXCEEDED", "E0500"),
        ("N0503", "E0500"),
        ("N0507", "E0519"),
        ("E0413", "E0415"),
        (
            "E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY",
            "is not a char boundary",
        ),
    ] {
        let out = karac(dir, &["explain", code]);
        assert!(out.contains("This code is retired"), "{code}: {out}");
        assert!(out.contains(now), "{code}: {out}");
        let json = karac(dir, &["explain", "--format=json", code]);
        assert!(json.contains("This code is retired"), "{code}: {json}");
    }
    for (code, pointer) in [
        ("E0217", "no `|>`"),
        ("E0106", "E_DEFERRED_LAYOUT_BLOCK"),
        ("E0151", "E_DEFERRED_LAYOUT_BLOCK"),
        ("E0260", "m.lock()"),
        ("E0402", "Effect Groups and Composition"),
        ("E0405", "Project Profiles"),
        ("E0407", "Project Profiles"),
        ("E0409", "Module-Level Mutable State"),
        ("E_UNION_REQUIRES_REPR", "FFI Unions"),
        ("E_PROFILE_NO_PROFILES", "Project Profiles"),
        ("E_MULTIVERSION_EMPTY", "Multiversioning"),
        ("E_CODEGEN_HINT_ON_CLOSURE", "Codegen Hint Attributes"),
        ("E_COLD_INLINE_ALWAYS_CONFLICT", "not yet lowered"),
        ("E_REPR_DEFERRED", "`#[repr(packed)]`"),
        ("E_REPR_TRANSPARENT_UNION_FORBIDDEN", "FFI Unions"),
        ("E_SHAPE", "Numerical Types"),
        ("E0264", "Project Profiles"),
        ("E0265", "Project Profiles"),
    ] {
        let out = karac(dir, &["explain", code]);
        assert!(
            out.contains("not in the language this compiler implements") && out.contains(pointer),
            "{code}: {out}"
        );
    }
}

/// Gowtham's decision 2026-11-17: contracts are an experimental
/// implementation, so their codes say so and point at deferred.md, without
/// the "not in the language" scope paragraph.
#[test]
fn diagnostics_corpus_contract_codes_are_experimental() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for code in [
        "E_CONTRACT_IMPURE",
        "E_OLD_OUTSIDE_ENSURES",
        "E_OLD_RESULT",
        "E_COMPTIME_PANIC",
        "E_OFFSET_OF_UNKNOWN_FIELD",
        "E_LAZYLOCK_RUNTIME_CAPTURE",
        "E0800",
        "E_GPU_DISPATCH_ARITY",
        "E_REFINEMENT_PREDICATE_VIOLATION",
        "E_GAT_EFFECT_PARAM",
        "E_TARGET_FEATURE_EMPTY",
    ] {
        let out = karac(dir, &["explain", code]);
        assert!(
            out.contains("experimental feature") && out.contains("docs/deferred.md § "),
            "{code}: {out}"
        );
        assert!(
            !out.contains("not in the language this compiler implements"),
            "{code}: {out}"
        );
    }
}

/// B-2026-11-16-303: the codes that need a package of several files, which a
/// one-file corpus entry cannot hold. Each package is written to a scratch
/// directory and checked as a package.
#[test]
fn diagnostics_corpus_package_codes_are_produced_and_explained() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "E0111",
            &[
                ("src/main.kara", "import db.helper.secret;\nfn main() {}\n"),
                ("src/db/helper.kara", "private fn secret() {}\n"),
            ],
        ),
        (
            "E0113",
            &[
                ("src/main.kara", "import geometry.volume;\nfn main() {}\n"),
                ("src/geometry.kara", "pub fn area() -> i64 { 1 }\n"),
            ],
        ),
        (
            "E0124",
            &[
                (
                    "src/main.kara",
                    "import alpha.*;\nimport beta.*;\nfn main() { println(dup()); }\n",
                ),
                ("src/alpha.kara", "pub fn dup() -> i64 { 1 }\n"),
                ("src/beta.kara", "pub fn dup() -> i64 { 2 }\n"),
            ],
        ),
        (
            "E_TEST_OUTSIDE_TEST_FILE",
            &[(
                "src/main.kara",
                "fn main() {}\ntest \"lost\" {\n    assert(true);\n}\n",
            )],
        ),
        (
            "E0223",
            &[
                ("src/main.kara", "import a;\nfn main() { a.f(); }\n"),
                ("src/a.kara", "import b;\npub fn f() { b.g(); }\n"),
                ("src/b.kara", "import a;\npub fn g() { a.f(); }\n"),
            ],
        ),
        (
            "E_TEST_REQUIRES",
            &[
                ("src/main.kara", "fn main() {}\n"),
                (
                    "src/main_test.kara",
                    "#[test(requires: [Nope])]\ntest \"x\" {\n    assert(true);\n}\n",
                ),
            ],
        ),
    ];
    let mut bad = Vec::new();
    for (code, files) in cases {
        let dir = tempfile_dir(&format!("pkg_{}", code.to_lowercase()));
        std::fs::write(
            dir.join("kara.toml"),
            format!("[package]\nname = \"{}\"\n", code.to_lowercase()),
        )
        .unwrap();
        for (path, text) in *files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let json = karac(&dir, &["check", "--output=json"]);
        if !json.contains(&format!("\"code\":\"{code}\"")) {
            bad.push(format!("{code}: not reported:\n{json}"));
        }
        let explained = karac(&dir, &["explain", code]);
        if explained.contains("no class page yet") {
            bad.push(format!("{code}: no description:\n{explained}"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    // E0227: a package command run where there is no `kara.toml`.
    let dir = tempfile_dir("no_manifest");
    std::fs::write(dir.join("main.kara"), "fn main() {}\n").unwrap();
    let json = karac(&dir, &["check", "--output=json"]);
    if !json.contains("\"code\":\"E0227\"") {
        bad.push(format!("E0227: not reported:\n{json}"));
    }
    if karac(&dir, &["explain", "E0227"]).contains("no class page yet") {
        bad.push("E0227: no description".to_string());
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

/// B-2026-11-18-3: a bare `size_of()` names no type, and returned the
/// placeholder body's `0`. A program's own non-generic `size_of` is still its
/// own function.
#[test]
fn diagnostics_corpus_bare_size_of_names_no_type() {
    let dir = tempfile_dir("bare_size_of");
    std::fs::write(
        dir.join("main.kara"),
        "fn main() {\n    println(size_of());\n    println(i64.align_of());\n}\n",
    )
    .unwrap();
    let out = karac(&dir, &["check", "--output=json", "main.kara"]);
    assert!(
        out.contains("\"code\":\"E_LAYOUT_QUERY_TYPE_ARG_REQUIRED\""),
        "{out}"
    );
    assert!(out.contains("T.size_of()"), "{out}");
    std::fs::write(
        dir.join("own.kara"),
        "fn size_of() -> i64 { 7 }\nfn main() {\n    println(size_of());\n}\n",
    )
    .unwrap();
    let out = karac(&dir, &["run", "own.kara"]);
    assert!(out.trim_end().ends_with('7'), "{out}");
}
