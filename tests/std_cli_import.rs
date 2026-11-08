//! B-2026-11-08-29 — the deferred `std.cli` parser's items (`Parser`, `Arg`,
//! `Args`, `CliError`, `Subcommand`, `SubcommandResult` and the row types it
//! stores) are reached through `import std.cli` (deferred.md § std.cli), as
//! `std.regex` and `std.json` are. A program's own type of one of those
//! names shadows nothing it can see, so it draws no `prelude_shadow`
//! warning; a bare use is refused by `karac check` with the import as its
//! machine fix.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn file(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-std-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn a_program_s_own_cli_named_types_draw_no_prelude_shadow_warning() {
    let path = file(
        "own",
        "enum CliError { Usage(String), Io(String) }\n\
         struct Parser { pos: i64 }\n\
         struct Args { names: Vec[String] }\n\
         struct Arg { name: String }\n\
         struct SubcommandResult { name: String }\n\
         fn main() {\n    \
             let p = Parser { pos: 1 };\n    \
             let a = Args { names: Vec.new() };\n    \
             let e = CliError.Usage(\"x\".to_string());\n    \
             match e { CliError.Usage(s) => println(s), CliError.Io(s) => println(s) }\n    \
             println(f\"{p.pos} {a.names.len()}\");\n\
         }\n",
    );
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "{out}");
    assert!(!out.contains("prelude_shadow"), "{out}");
    let (ok, out) = karac(&["run", p]);
    assert!(ok && out.contains("x\n1 0"), "{out}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_bare_cli_parser_is_refused_with_the_import_as_its_fix() {
    let path = file(
        "bare",
        "fn main() {\n    let p = Parser.new(\"greet\").arg(\"--name\", Arg.string());\n    println(p.program_name);\n}\n",
    );
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(
        !ok && out.contains("`Parser` is in the library module `std.cli`")
            && out.contains("deferred.md § std.cli"),
        "{out}"
    );
    karac(&["fix", p]);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.starts_with("import std.cli.{Arg, Parser};\n\nfn main() {"),
        "{after}"
    );
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["run", p]);
    assert!(ok && out.contains("greet"), "{out}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
