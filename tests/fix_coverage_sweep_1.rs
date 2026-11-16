//! Fixes for the refusals the corpus fix-coverage sweep found most often
//! with no machine fix (review thread, 2026-11-16): a `par` block written as
//! statements, a `let` that keeps a view of a temporary, and the legacy
//! `ExitStatus.code` field. Each test runs `karac fix` and checks the
//! program it leaves.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

/// Writes `src`, runs `karac fix`, and returns the fixed text and the
/// output of `karac check` on it.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-fixcov-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let fix = karac(&["fix", p]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    (after, text)
}

#[test]
fn par_statement_branches_are_rewritten_with_commas() {
    let (after, check) = fixed(
        "parstmt",
        "fn a() -> i64 { 1 }
fn b() -> i64 { 2 }
fn main() {
    par {
        a();
        b();
    }
    par {
        { a(); }
        { b(); }
    };
}
",
    );
    assert!(
        after.contains("par {\n        a(),\n        b(),\n    }"),
        "{after}"
    );
    assert!(after.contains("{ a(); },\n        { b(); }"), "{after}");
    assert!(check.contains("All checks passed"), "{check}");
}

#[test]
fn par_let_branches_bind_the_blocks_tuple() {
    let (after, check) = fixed(
        "parlet",
        "fn read(n: i64) -> i64 { n * 2 }
fn main() {
    par {
        let a = read(1);
        let _ = read(2);
        let b = read(3);
    }
    println(f\"{a} {b}\");
}
",
    );
    assert!(
        after.contains(
            "    let (a, _, b) = par {\n        read(1),\n        read(2),\n        read(3),\n    };"
        ),
        "{after}"
    );
    assert!(check.contains("All checks passed"), "{check}");
}

#[test]
fn par_wildcard_lets_need_no_tuple() {
    let (after, check) = fixed(
        "parwild",
        "fn read(n: i64) -> i64 { n * 2 }
fn main() {
    par {
        let _ = read(1);
        let _ = read(2);
    }
}
",
    );
    assert!(
        after.contains("    par {\n        read(1),\n        read(2),\n    }"),
        "{after}"
    );
    assert!(check.contains("All checks passed"), "{check}");
}

#[test]
fn an_annotated_par_let_gets_no_rewrite() {
    let dir = std::env::temp_dir().join(format!("karac-fixcov-parann-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(
        &f,
        "fn main() {
    par {
        let a: i64 = 1;
        let b = 2;
    }
}
",
    )
    .unwrap();
    let out = karac(&["check", "--output=json", f.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("comma-separated expressions"), "{text}");
    assert!(!text.contains("fix_diff"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_view_of_a_temporary_binds_the_temporary_first() {
    let (after, check) = fixed(
        "tempview",
        "fn main() {
    let line = \"GET /a HTTP/1.1 200\".to_string();
    let req: Vec[Str] = line.substring(0, 15).split(\" \").collect();
    println(f\"{req.len()} {req[1]}\");
}
",
    );
    assert!(
        after.contains(
            "    let req_src = line.substring(0, 15);\n    let req: Vec[Str] = req_src.split(\" \").collect();"
        ),
        "{after}"
    );
    assert!(check.contains("All checks passed"), "{check}");
}

#[test]
fn the_legacy_exit_status_code_field_becomes_the_method() {
    let dir = std::env::temp_dir().join(format!("karac-fixcov-code-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(
        &f,
        "fn main() {
    let st = Command.new(\"true\").status().unwrap();
    if st.code == 0 {
        println(\"ok\");
    }
}
",
    )
    .unwrap();
    let p = f.to_str().unwrap();
    let out = karac(&["check", "--output=json", p]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("`code` is a method now"), "{text}");
    let _ = karac(&["fix", p]);
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(
        after.contains("if st.code().unwrap_or(-1) == 0 {"),
        "{after}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_par_tail_gathering_its_lets_goes_with_them() {
    let (after, check) = fixed(
        "partail",
        "fn read(n: i64) -> i64 { n * 2 }
fn main() {
    let (a, b) = par {
        let a = read(1);
        let b = read(2);
        (a, b)
    };
    println(f\"{a} {b}\");
}
",
    );
    assert!(
        after.contains("    let (a, b) = par {\n        read(1),\n        read(2),\n    };"),
        "{after}"
    );
    assert!(check.contains("All checks passed"), "{check}");
}
