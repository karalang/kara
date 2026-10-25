//! `karac run` on v2 (M3 item 1, B-2026-10-22-12): checked as `karac check`
//! checks, built to MIR, run on the JIT by default (the MIR interpreter on a
//! build without llvm) and on the MIR interpreter under `--interp`.
//! `--legacy` is refused.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-run-v2-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A reported path with Windows separators written as `/`, so one expected
/// string covers every host.
fn slashed(s: &str) -> String {
    s.replace('\\', "/")
}

/// `path` as a JSON string, with Windows separators written as `/`.
fn json_path(path: &std::path::Path) -> String {
    format!("{:?}", slashed(&path.display().to_string()))
}

/// Both v2 executors: the default, and `--interp`.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

const PANICS: &str = "fn main() {\n    println(\"before\");\n    let v = [1, 2, 3];\n    let i = v.len() + 2;\n    println(f\"{v[i]}\");\n}\n";

const RETURNS_ERR: &str = "fn f(x: i64) -> Result[i64, String] {\n    if x > 1 { return Err(\"too big\"); }\n    Ok(x)\n}\nfn main() -> Result[(), String] {\n    let a = f(1)?;\n    println(f\"a={a}\");\n    let b = f(5)?;\n    println(f\"b={b}\");\n    Ok(())\n}\n";

#[test]
fn run_v2_prints_and_keeps_the_exit_code() {
    let path = write(
        "exit",
        "fn main() -> ExitCode {\n    println(\"hi\");\n    eprintln(\"to stderr\");\n    ExitCode(7)\n}\n",
    );
    for lane in LANES {
        let out = karac(lane, &path);
        assert_eq!(text(&out.stdout), "hi\n", "{lane:?}");
        assert_eq!(text(&out.stderr), "to stderr\n", "{lane:?}");
        assert_eq!(out.status.code(), Some(7), "{lane:?}");
    }
}

#[test]
fn run_v2_reports_a_fault_and_exits_101() {
    let path = write("fault", PANICS);
    for lane in LANES {
        let out = karac(lane, &path);
        let err = text(&out.stderr);
        assert_eq!(text(&out.stdout), "before\n", "{lane:?}");
        assert!(
            err.contains("runtime error: index out of bounds"),
            "{lane:?}: {err}"
        );
        assert!(err.contains("main.kara:5:16"), "{lane:?}: {err}");
        assert_eq!(out.status.code(), Some(101), "{lane:?}");
    }
}

#[test]
fn run_v2_main_err_prints_error_and_exits_1() {
    let path = write("err", RETURNS_ERR);
    for lane in LANES {
        let out = karac(lane, &path);
        assert_eq!(text(&out.stdout), "a=1\n", "{lane:?}");
        assert!(
            text(&out.stderr).starts_with("Error: too big\n"),
            "{lane:?}"
        );
        assert_eq!(out.status.code(), Some(1), "{lane:?}");
    }
}

/// An error `karac check` reports stops the run before anything executes.
#[test]
fn run_v2_refuses_what_check_refuses() {
    let path = write(
        "check",
        "fn main() {\n    println(\"ran\");\n    let x: i64 = \"s\";\n    println(f\"{x}\");\n}\n",
    );
    for lane in LANES {
        let out = karac(lane, &path);
        let err = text(&out.stderr);
        assert_eq!(text(&out.stdout), "", "{lane:?}");
        assert!(err.contains("error[typecheck]"), "{lane:?}: {err}");
        assert!(err.contains("1 error(s) found."), "{lane:?}: {err}");
        assert_eq!(out.status.code(), Some(1), "{lane:?}");
    }
    let mut args = LANES[0].to_vec();
    args.push("--output=json");
    let out = karac(&args, &path);
    assert!(
        text(&out.stdout).contains("\"diagnostics\""),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(out.status.code(), Some(1));
}

/// `--output=json` / `jsonl` turn the program's runtime reports into the JSON
/// objects legacy `karac run` printed.
#[test]
fn run_v2_json_output_carries_the_runtime_reports() {
    let fault = write("json-fault", PANICS);
    let err = write("json-err", RETURNS_ERR);
    for lane in LANES {
        let mut json = lane.to_vec();
        json.push("--output=json");
        let out = karac(&json, &fault);
        let stdout = text(&out.stdout);
        assert!(stdout.starts_with("before\n{\"runtime_errors\":[{\"message\":\"index out of bounds\",\"location\":{\"file\":"), "{lane:?}: {stdout}");
        assert!(
            stdout.contains("\"line\":5,\"col\":16}}]}\n"),
            "{lane:?}: {stdout}"
        );
        assert!(
            stdout.contains("{\"error_return_trace\":[{\"file\":"),
            "{lane:?}: {stdout}"
        );
        assert_eq!(text(&out.stderr), "", "{lane:?}");
        assert_eq!(out.status.code(), Some(101), "{lane:?}");

        // `main`'s error carries the trace of the `?` that returned it
        // (design.md § Error return traces, B-2026-10-25-3).
        let file = json_path(&err);
        let out = karac(&json, &err);
        assert_eq!(
            slashed(&text(&out.stdout)),
            format!("a=1\n{{\"error\":\"too big\"}}\n{{\"error_return_trace\":[{{\"file\":{file},\"line\":8,\"column\":13}}]}}\n"),
            "{lane:?}"
        );
        assert_eq!(out.status.code(), Some(1), "{lane:?}");

        let mut jsonl = lane.to_vec();
        jsonl.push("--output=jsonl");
        let out = karac(&jsonl, &err);
        assert_eq!(
            slashed(&text(&out.stdout)),
            format!("a=1\n{{\"type\":\"error\",\"message\":\"too big\"}}\n{{\"type\":\"error_return_trace\",\"frames\":[{{\"file\":{file},\"line\":8,\"column\":13}}],\"truncated\":false}}\n"),
            "{lane:?}"
        );
    }
}

#[test]
fn run_v2_timeout_exits_124() {
    let path = write(
        "timeout",
        "fn main() {\n    let mut i = 0;\n    while true { i += 1; }\n}\n",
    );
    for lane in LANES {
        let mut args = lane.to_vec();
        args.extend(["--timeout", "1s"]);
        let out = karac(&args, &path);
        assert_eq!(out.status.code(), Some(124), "{lane:?}");
        assert!(
            text(&out.stderr).contains("karac: timed out after 1s"),
            "{lane:?}"
        );
    }
}

/// `env.args()` is the script then what followed `--`.
#[test]
fn run_v2_interp_env_args_is_the_programs_argv() {
    let path = write(
        "args",
        "fn main() {\n    let a = env.args();\n    println(f\"{a.len()}\");\n    for x in a { println(x); }\n}\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .args(["run", "--interp"])
        .arg(&path)
        .args(["--", "one", "two words"])
        .output()
        .unwrap();
    assert_eq!(
        text(&out.stdout),
        format!("3\n{}\none\ntwo words\n", path.display())
    );
}

/// The same on the compiled lanes (B-2026-10-22-27): `karac run`'s JIT,
/// and a `karac build --mir` binary, whose first argument is its own path.
#[cfg(feature = "llvm")]
#[test]
fn run_v2_compiled_env_args_is_the_programs_argv() {
    let path = write(
        "args_llvm",
        "fn main() {\n    let a = env.args();\n    println(f\"{a.len()}\");\n    for x in a { println(x); }\n}\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .arg("run")
        .arg(&path)
        .args(["--", "one", "two words"])
        .output()
        .unwrap();
    assert_eq!(
        text(&out.stdout),
        format!("3\n{}\none\ntwo words\n", path.display()),
        "{}",
        text(&out.stderr)
    );
    let exe = path.with_extension("bin");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--mir"])
        .arg(&path)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    if !built.status.success() {
        // No runtime archive: nothing to run.
        eprintln!("skipped the built leg: {}", text(&built.stderr));
        return;
    }
    let out = Command::new(&exe)
        .args(["one", "two words"])
        .output()
        .unwrap();
    assert_eq!(
        text(&out.stdout),
        format!("3\n{}\none\ntwo words\n", exe.display())
    );
}

#[test]
fn run_v2_refuses_sequential_by_name() {
    let path = write("seq", "fn main() {\n    println(\"hi\");\n}\n");
    let out = karac(&["run", "--sequential"], &path);
    assert!(
        text(&out.stderr).contains("--sequential"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        text(&out.stderr).contains("a different program"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(text(&out.stdout), "");
    assert_eq!(out.status.code(), Some(1));
}

/// `--legacy` named the pipeline M3 deleted and is refused; `KARAC_LEGACY=1`
/// changes nothing, so the program runs on v2.
#[test]
fn run_v2_legacy_flag_is_refused_and_env_is_ignored() {
    let path = write("legacy", PANICS);
    let flag = karac(&["run", "--legacy"], &path);
    assert_eq!(flag.status.code(), Some(2));
    assert!(
        text(&flag.stderr).contains("--legacy is gone"),
        "{}",
        text(&flag.stderr)
    );
    let env = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_LEGACY", "1")
        .arg("run")
        .arg(&path)
        .output()
        .unwrap();
    let err = text(&env.stderr);
    assert!(err.contains("runtime error: index out of bounds"), "{err}");
    assert_eq!(env.status.code(), Some(101));
}

/// `karac <file.kara>` is `karac run <file.kara>`.
#[test]
fn run_v2_shorthand_runs_on_v2() {
    let path = write("shorthand", PANICS);
    let out = karac(&[], &path);
    assert!(text(&out.stderr).contains("runtime error: index out of bounds"));
    assert_eq!(out.status.code(), Some(101));
}

/// A package written to a fresh directory: `files` are `(path under src/,
/// text)`.
fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-run-v2-pkg-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    for (path, text) in files {
        std::fs::write(dir.join("src").join(path), text).unwrap();
    }
    dir
}

fn karac_in(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

const PKG_MAIN: &str = "//! The entry.\nimport util.{double, Pt, pick};\nimport std.secret.{Secret};\n\nfn main() {\n    let p = Pt { x: 2, y: 3 };\n    println(f\"{double(p.x)} {p.y}\");\n    let s = Secret.new(5);\n    println(f\"{s.expose()}\");\n    dbg(p.x + 1);\n    let mut v: Vec[i64] = Vec.new();\n    v.push(1);\n    println(f\"{pick(v, 7)}\");\n}\n";

const PKG_UTIL: &str = "//! Helpers.\nimport std.secret.{Secret};\n\npub struct Pt { pub x: i64, pub y: i64 }\n\npub fn double(x: i64) -> i64 {\n    let s = Secret.new(x);\n    let _ = s.expose();\n    x * 2\n}\n\npub fn pick(v: ref Vec[i64], i: i64) -> i64 {\n    v[i as usize]\n}\n";

/// A package runs as one program (project mode, B-2026-10-22-14): its
/// modules' items in one scope, a `std` import two files share taken once,
/// and every place it reports named by the file and line it came from.
#[test]
fn run_v2_runs_a_package_and_names_its_files() {
    let dir = package("runs", &[("main.kara", PKG_MAIN), ("util.kara", PKG_UTIL)]);
    for lane in LANES {
        let mut args = lane.to_vec();
        args.push("src/main.kara");
        let out = karac_in(&dir, &args);
        let err = slashed(&text(&out.stderr));
        assert_eq!(text(&out.stdout), "4 3\n5\n", "{lane:?}");
        assert!(
            err.contains("[src/main.kara:10] p.x + 1 = 3"),
            "{lane:?}: {err}"
        );
        assert!(
            err.contains("runtime error: index out of bounds\n  at src/util.kara:13:5"),
            "{lane:?}: {err}"
        );
        assert_eq!(out.status.code(), Some(101), "{lane:?}");
    }
}

/// An error of the v2 rules names its module's file and line, in the text
/// header, the snippet's gutter, and the JSON diagnostic: the package-wide
/// check sees it too, and leaves a type refusal to the v2 check of the
/// combined source, which renders it (B-2026-10-25-4).
#[test]
fn run_v2_package_diagnostics_name_their_file() {
    let util = format!("{PKG_UTIL}\npub fn keep(v: Vec[i64]) -> Vec[i64] {{\n    v\n}}\n");
    let dir = package("diag", &[("main.kara", PKG_MAIN), ("util.kara", &util)]);
    let out = karac_in(&dir, &["run", "src/main.kara"]);
    let err = slashed(&text(&out.stderr));
    assert!(err.contains("src/util.kara:17:"), "{err}");
    assert!(err.contains("17 |     v"), "{err}");
    assert_eq!(text(&out.stdout), "");
    assert_eq!(out.status.code(), Some(1));
    let out = karac_in(&dir, &["run", "--output=json", "src/main.kara"]);
    let json = text(&out.stdout).replace("\\\\", "/");
    assert!(
        json.contains("\"file\":\"src/util.kara\",\"line\":17"),
        "{json}"
    );
    assert_eq!(out.status.code(), Some(1));
}

/// An import that renames runs as the name it binds (B-2026-10-23-14; it
/// was refused by name).
#[test]
fn run_v2_package_runs_a_renaming_import() {
    let main = PKG_MAIN
        .replace(
            "import util.{double, Pt, pick};",
            "import util.{double as twice, Pt, pick};",
        )
        .replace("double(p.x)", "twice(p.x)");
    let dir = package("alias", &[("main.kara", &main), ("util.kara", PKG_UTIL)]);
    for lane in LANES {
        let mut args = lane.to_vec();
        args.push("src/main.kara");
        let out = karac_in(&dir, &args);
        let err = slashed(&text(&out.stderr));
        assert_eq!(text(&out.stdout), "4 3\n5\n", "{lane:?}: {err}");
        assert!(
            err.contains("[src/main.kara:10] p.x + 1 = 3"),
            "{lane:?}: {err}"
        );
        assert_eq!(out.status.code(), Some(101), "{lane:?}");
    }
}
