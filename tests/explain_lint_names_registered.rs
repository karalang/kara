//! `karac explain E_LINT_LEVEL_NO_ARGS` and `E_LINT_LEVEL_NON_IDENT_ARG`
//! showed `#[allow(unused_variables)]` as the way to write a lint level,
//! and Kāra has no `unused_variables` lint: the attribute as shown draws
//! `warning[unknown_lint]` (kata 2765, B-2026-11-20-144). Every lint an explain
//! text writes inside `#[allow(..)]` must be in `karac lint --list`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_lint_level_explanations_name_registered_lints() {
    let listed: Vec<String> = karac(&["lint", "--list"])
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    assert!(listed.iter().any(|l| l == "unused_mut"), "{listed:?}");
    for code in ["E_LINT_LEVEL_NO_ARGS", "E_LINT_LEVEL_NON_IDENT_ARG"] {
        let text = karac(&["explain", code]);
        let mut named = 0;
        for piece in text.split("#[allow(").skip(1) {
            let args = piece.split(")]").next().unwrap();
            for name in args.split(',').map(str::trim) {
                named += 1;
                assert!(
                    listed.iter().any(|l| l == name),
                    "{code} names `{name}`, which `karac lint --list` does not hold"
                );
            }
        }
        assert!(named > 0, "{code}: {text}");
    }
}
