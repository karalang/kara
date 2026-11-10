//! Two refusals from the http_cache app that named no remedy:
//!
//! - B-2026-11-09-96: a tuple of borrowed elements moved where owned ones
//!   are wanted (`out.push((k, v))` in `for (k, v) in headers` over a bare
//!   parameter) takes `.clone()` on each borrowed element, as the
//!   one-element form does, or `own` on the parameter it was bound out of.
//! - B-2026-11-09-97: an `Option[String]` against the `Some` of a `Str`
//!   (`opt != Some(TOKEN)`) takes `.to_string()` in the `Some`.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-http-cache-diag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// Refused with `needle`; `karac fix` writes `fixed`, which checks and prints `want`.
fn refused_then_fixed(name: &str, src: &str, needle: &str, fixed: &[&str], want: &str) {
    let path = scratch(name, src);
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(text(&check).contains(needle), "{}", text(&check));
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let after = std::fs::read_to_string(&path).unwrap();
    for f in fixed {
        assert!(after.contains(f), "{after}");
    }
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
}

#[test]
fn a_tuple_of_borrowed_loop_elements_takes_clone_on_each() {
    refused_then_fixed(
        "tuple_for.kara",
        "fn with_label(headers: Vec[(String, String)]) -> Vec[(String, String)] {
    let mut out: Vec[(String, String)] = Vec.new();
    for (k, v) in headers {
        out.push((k, v));
    }
    out
}
fn main() {
    let h = [(\"a\".to_string(), \"b\".to_string())];
    let l = with_label(h);
    println(f\"{l.len()} {l[0].0}{l[0].1}\");
}
",
        "`k` and `v` are borrowed, and a borrowed value cannot be moved out of what it \
         borrows. Take owned copies with `.clone()`",
        &["out.push((k.clone(), v.clone()));"],
        "1 ab\n",
    );
}

#[test]
fn a_tuple_with_one_borrowed_element_clones_only_that_one() {
    refused_then_fixed(
        "tuple_one.kara",
        "fn tally(headers: Vec[(String, i64)]) -> Vec[(String, i64)] {
    let mut out: Vec[(String, i64)] = Vec.new();
    for (k, n) in headers {
        out.push((k, n + 1));
    }
    out
}
fn main() {
    let t = tally([(\"x\".to_string(), 1)]);
    println(f\"{t[0].0} {t[0].1}\");
}
",
        "`k` is borrowed",
        &["out.push((k.clone(), n + 1));"],
        "x 2\n",
    );
}

#[test]
fn a_tuple_bound_out_of_a_bare_parameter_takes_its_own() {
    refused_then_fixed(
        "tuple_param.kara",
        "fn pairs(p: (String, String)) -> Vec[(String, String)] {
    let (a, b) = p;
    let mut out: Vec[(String, String)] = Vec.new();
    out.push((b, a));
    out
}
fn main() {
    let v = pairs((\"a\".to_string(), \"b\".to_string()));
    println(f\"{v[0].0}{v[0].1}\");
}
",
        "bound out of parameter `p`",
        &["fn pairs(p: own (String, String))"],
        "ba\n",
    );
}

#[test]
fn an_option_string_against_some_of_a_str_constant_takes_to_string() {
    refused_then_fixed(
        "option_str.kara",
        "const PURGE_TOKEN: Str = \"s3cret\";
fn main() {
    let h: Option[String] = Some(\"s3cret\".to_string());
    let g: Option[String] = None;
    let lm: Str = \"Tue\";
    if h != Some(PURGE_TOKEN) { println(\"refused\"); } else { println(\"purge\"); }
    if g == Some(lm) { println(\"fresh\"); } else { println(\"stale\"); }
}
",
        "cannot compare 'Option[String]' and 'Option[Str]': an `Option` compares items of \
         one type",
        &[
            "h != Some(PURGE_TOKEN.to_string())",
            "g == Some(lm.to_string())",
        ],
        "purge\nstale\n",
    );
}
