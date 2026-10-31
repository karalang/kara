//! The command-line rough edges a first session with `karac` hits, from the
//! README dogfood rehearsal: `karac new --cli` scaffolds a command-line tool
//! (B-2026-10-30-20), `karac run` with no file runs the project's entry
//! (B-2026-10-30-18), a stray argument to `run` points at `--`
//! (B-2026-10-30-19), and `karac query ownership` takes a bare file the way
//! `effects` does (B-2026-10-30-17).

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn karac_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-cli-rehearsal-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn karac_new_cli_scaffolds_a_tool_that_runs_and_tests() {
    let dir = scratch("new-cli");
    let new = karac_in(&dir, &["new", "wc", "--cli"]);
    assert!(new.status.success(), "{}", text(&new.stderr));
    assert!(
        text(&new.stdout).contains("Scaffolded command-line project `wc`"),
        "{}",
        text(&new.stdout)
    );
    let proj = dir.join("wc");
    let main = std::fs::read_to_string(proj.join("src/main.kara")).unwrap();
    assert!(main.contains("env.args()") && main.contains("fs.read_to_string"));
    std::fs::write(proj.join("in.txt"), "a b c\nd e\n").unwrap();

    // No file: the project's src/main.kara. A usage error exits 2, and a
    // file that cannot be read exits 1 saying why in words.
    let bare = karac_in(&proj, &["run"]);
    assert_eq!(
        (bare.status.code(), text(&bare.stderr)),
        (Some(2), "usage: wc <file>\n".to_string())
    );
    let missing = karac_in(&proj, &["run", "--", "nope.txt"]);
    assert_eq!(
        (missing.status.code(), text(&missing.stderr)),
        (Some(1), "wc: nope.txt: file not found\n".to_string())
    );
    let run = karac_in(&proj, &["run", "--", "in.txt"]);
    assert_eq!(
        (run.status.code(), text(&run.stdout)),
        (Some(0), "2 5 10 in.txt\n".to_string()),
        "{}",
        text(&run.stderr)
    );
    // A stray argument says where program arguments go.
    let stray = karac_in(&proj, &["run", "src/main.kara", "in.txt"]);
    assert_eq!(stray.status.code(), Some(1));
    assert!(
        text(&stray.stderr).contains(
            "help: arguments for the program go after `--`: karac run src/main.kara -- in.txt"
        ),
        "{}",
        text(&stray.stderr)
    );
    let test = karac_in(&proj, &["test"]);
    assert!(test.status.success(), "{}", text(&test.stdout));
    assert!(
        text(&test.stdout).contains("\"passed\":2,\"failed\":0"),
        "{}",
        text(&test.stdout)
    );
    let check = karac_in(&proj, &["check"]);
    assert!(check.status.success(), "{}", text(&check.stderr));
    // The scaffold is what `karac fmt` writes.
    for f in ["src/main.kara", "src/main_test.kara"] {
        let fmt = karac_in(&proj, &["fmt", f]);
        assert_eq!(
            text(&fmt.stdout),
            std::fs::read_to_string(proj.join(f)).unwrap(),
            "{f}"
        );
    }

    // Outside a project there is still no file to run.
    let outside = karac_in(&dir, &["run"]);
    assert_eq!(outside.status.code(), Some(1));
    assert!(
        text(&outside.stderr).contains("error: missing file argument"),
        "{}",
        text(&outside.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn karac_query_ownership_on_a_bare_file_lists_every_function() {
    let dir = scratch("query-ownership");
    std::fs::write(
        dir.join("app.kara"),
        "fn total(xs: Vec[i64]) -> i64 { xs.iter().sum() }
fn grab(s: own String) -> i64 { s.len() }
fn main() { println(total([1, 2].to_vec()) + grab(\"x\".to_string())); }
",
    )
    .unwrap();
    let all = karac_in(&dir, &["query", "ownership", "app.kara"]);
    assert!(all.status.success(), "{}", text(&all.stderr));
    let out = text(&all.stdout);
    assert!(out.starts_with("{\"functions\":["), "{out}");
    let at = |name: &str| {
        out.find(&format!("\"function\":\"{name}\""))
            .unwrap_or_else(|| panic!("no {name} in {out}"))
    };
    assert!(at("grab") < at("main") && at("main") < at("total"), "{out}");
    assert!(
        out.contains("\"function\":\"grab\",\"parameters\":[{\"name\":\"s\""),
        "{out}"
    );
    let one = karac_in(&dir, &["query", "ownership", "app.kara.total"]);
    assert!(
        text(&one.stdout).starts_with("{\"function\":\"total\",\"parameters\":[{\"name\":\"xs\""),
        "{}",
        text(&one.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn karac_lint_list_prints_the_registry() {
    // design.md § Built-in lints names `karac lint --list` as the way to read
    // the registry (B-2026-10-30-22).
    let dir = std::env::temp_dir();
    let list = karac_in(&dir, &["lint", "--list"]);
    assert!(list.status.success(), "{}", text(&list.stderr));
    let out = text(&list.stdout);
    assert!(out.starts_with("LINT "), "{out}");
    for name in [
        "deprecated",
        "lossless_cast",
        "redundant_cast",
        "unknown_lint",
    ] {
        assert!(
            out.lines().any(|l| l.starts_with(&format!("{name} "))),
            "no {name} in:\n{out}"
        );
    }
    let json = karac_in(&dir, &["lint", "--list", "--output=json"]);
    let json = text(&json.stdout);
    assert!(
        json.contains("{\"name\":\"lossless_cast\",\"default_level\":\"warn\""),
        "{json}"
    );
    let bare = karac_in(&dir, &["lint"]);
    assert_eq!(bare.status.code(), Some(1));
}
