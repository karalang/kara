//! B-2026-11-20-134: `%ownership` names a binding a later cell moved away
//! as `moved`, not `owned`, agreeing with the cell that is refused for
//! using it.

#[test]
fn repl_ownership_lists_a_moved_binding_as_moved() {
    let mut s = karac::repl::Session::new();
    for cell in [
        "let w = [1, 2].to_vec();",
        "let z = w;",
        "let n = 3;",
        "let k = n;",
    ] {
        let out = s.evaluate_cell_captured(cell);
        assert!(out.errors.is_empty(), "{cell}: {:?}", out.errors);
    }
    let o = s.dispatch_magic("%ownership");
    assert!(o.ok, "{}", o.text);
    let rows: Vec<&str> = o.text.lines().collect();
    assert!(rows.contains(&"w: moved"), "{}", o.text);
    assert!(rows.iter().any(|r| r.starts_with("z: owned")), "{}", o.text);
    // A copy is not a move.
    assert!(rows.iter().any(|r| r.starts_with("n: owned")), "{}", o.text);
    let used = s.evaluate_cell_captured("println(w.len());");
    assert!(!used.errors.is_empty(), "the checker refuses `w` too");
}
