//! The book probe's rows against the compiler (probe-fuzzers/book2): derive
//! arguments design.md does not define (B-2026-11-16-262).

use karac::{run_playground, PlaygroundDiagnostic, PlaygroundResult};

fn errors(r: &PlaygroundResult) -> Vec<&PlaygroundDiagnostic> {
    r.diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect()
}

/// `src` is refused with exactly one error, at `line:column`, whose message
/// contains `msg`.
fn one_error(src: &str, line: usize, column: usize, msg: &str) {
    let r = run_playground(src);
    let errs = errors(&r);
    assert_eq!(errs.len(), 1, "{src}\n{errs:#?}");
    assert!(errs[0].message.contains(msg), "{src}\n{:#?}", errs[0]);
    assert_eq!((errs[0].line, errs[0].column), (line, column), "{src}");
}

// ── Derive arguments (B-2026-11-16-262) ────────────────────────────────

#[test]
fn derive_display_takes_only_snake_case_on_an_enum() {
    for arg in ["all_fields", "bogus_arg", "snake_case, all_fields"] {
        one_error(
            &format!(
                "#[derive(Display({arg}))]\nenum E {{ Up }}\nfn main() {{\n    println(f\"{{E.Up}}\");\n}}\n"
            ),
            1,
            10,
            "`Display` takes one argument in a derive, `snake_case`, on an enum",
        );
    }
    one_error(
        "#[derive(Display(all_fields))]\nstruct P { pub a: i64, b: i64 }\nfn main() {\n    \
         println(f\"{P { a: 1, b: 2 }}\");\n}\n",
        1,
        10,
        "`Display` takes one argument in a derive",
    );
    one_error(
        "#[derive(Display(snake_case))]\nstruct P { a: i64 }\nfn main() {\n    \
         println(f\"{P { a: 1 }}\");\n}\n",
        1,
        10,
        "a struct has no variants; write `Display`",
    );
    one_error(
        "#[derive(Clone, Eq(strict), PartialEq)]\nstruct P { a: i64 }\nfn main() {\n    \
         println(\"x\");\n}\n",
        1,
        17,
        "`Eq` takes no arguments in a derive",
    );
    let r = run_playground(
        "#[derive(Display(snake_case))]\nenum Mode { FastPath }\nfn main() {\n    \
         println(f\"{Mode.FastPath}\");\n}\n",
    );
    assert!(r.ok, "{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), "fast_path\n");
}

// ── CN-4, acronyms as words (B-2026-11-16-260) ─────────────────────────

fn acronym_warnings(src: &str) -> Vec<PlaygroundDiagnostic> {
    karac::check_source(src)
        .into_iter()
        .filter(|d| d.message.contains("treats an acronym as a word"))
        .collect()
}

#[test]
fn acronym_in_a_type_name_warns_with_the_rename() {
    let src =
        "struct HTTPClient {\n    port: i64,\n}\n\n#[derive(PartialEq)]\nenum Failure {\n    IOError,\n    Other,\n}\n\n\
               fn make() -> HTTPClient {\n    HTTPClient { port: 80 }\n}\n\n\
               fn main() {\n    let c: HTTPClient = make();\n    let f = Failure.IOError;\n    \
               let same = f == Failure.IOError;\n    println(f\"{c.port} {same}\");\n}\n";
    let ws = acronym_warnings(src);
    assert_eq!(ws.len(), 2, "{ws:#?}");
    assert!(ws[0].message.contains("so write `HttpClient`"), "{ws:#?}");
    assert_eq!((ws[0].line, ws[0].column), (1, 8), "{ws:#?}");
    assert!(ws[1].message.contains("so write `IoError`"), "{ws:#?}");
    // `karac fix` renames every use, as the warning says.
    let dir = std::env::temp_dir().join(format!("karac-acronym-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.kara"), src).unwrap();
    let st = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", "a.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let out = std::fs::read_to_string(dir.join("a.kara")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
    assert!(!out.contains("HTTP") && !out.contains("IOError"), "{out}");
    assert!(acronym_warnings(&out).is_empty(), "{out}");
    assert_eq!(run_playground(src).stdout, run_playground(&out).stdout);
}

#[test]
fn acronym_lint_spares_two_capitals_and_an_allow() {
    for name in ["AVal", "JValue", "Point3D", "H2o"] {
        let src =
            format!("struct {name} {{\n    a: i64,\n}}\nfn main() {{\n    println(\"x\");\n}}\n");
        assert!(acronym_warnings(&src).is_empty(), "{name}");
    }
    let src = "#[allow(acronym_case)]\nstruct URLParts {\n    a: i64,\n}\nfn main() {\n    println(\"x\");\n}\n";
    assert!(acronym_warnings(src).is_empty());
}

// ── `noblock` (B-2026-11-16-261) ───────────────────────────────────────

/// `#[noblock]` is not in design.md's attribute table: an unknown attribute
/// like any other. An extern's effects are exactly its `with` clause, so a
/// foreign function that does not block says so by leaving `blocks` out.
#[test]
fn noblock_is_an_unknown_attribute() {
    one_error(
        "unsafe extern \"C\" {\n    #[noblock]\n    fn abs(x: i32) -> i32;\n}\nfn main() {}\n",
        2,
        5,
        "unknown attribute `noblock`",
    );
    // `@noblock` is not attribute syntax (B-2026-11-25-61): the one error
    // is that, with its fix to `#[noblock]`.
    one_error(
        "unsafe extern \"C\" {\n    @noblock\n    fn abs(x: i32) -> i32;\n}\nfn main() {}\n",
        2,
        5,
        "`@noblock` is not an attribute: write `#[noblock]`",
    );
}
