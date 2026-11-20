//! `%ownership` and `%rc` inspect a session the cells it holds were checked
//! by: the Kāra library in scope and the core rules, as `karac check` and
//! every cell run. They type-checked a synthetic program with the plain
//! `karac::typecheck`, which runs neither, so a session whose cells all ran
//! was refused as having "type errors" (B-2026-11-20-97).

use super::session;

fn ran(s: &mut karac::repl::Session, cells: &[&str]) {
    for cell in cells {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
    }
}

#[test]
fn ownership_magic_inspects_a_session_of_library_iterators() {
    let mut s = session();
    ran(
        &mut s,
        &[
            "let words = \"a bb ccc\";",
            "let lens: Vec[i64] = words.split_whitespace().map(|w| w.len()).collect();",
        ],
    );
    let o = s.dispatch_magic("%ownership");
    assert!(o.ok, "{}", o.text);
    assert_eq!(
        o.text.trim_end(),
        "lens: owned (stack)\nwords: owned (stack)"
    );
}

#[test]
fn rc_magic_inspects_a_session_of_library_iterators() {
    let mut s = session();
    ran(
        &mut s,
        &["let lens: Vec[i64] = \"a bb\".split_whitespace().map(|w| w.len()).collect();"],
    );
    let o = s.dispatch_magic("%rc");
    assert!(o.ok, "{}", o.text);
    assert_eq!(o.text.trim_end(), "(no RC fallbacks in current session)");
}
