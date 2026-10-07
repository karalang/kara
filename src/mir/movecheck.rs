//! The move check (`docs/spikes/mir-types.md` §3, the `Checked` phase):
//! no place is read, borrowed or moved while it may already have been
//! moved out, or before it was ever initialized (core semantics C1).
//!
//! It reuses drop elaboration's move paths and initialization dataflow,
//! tracking every place whose type is not `Copy`, and is read-only: a
//! body that passes is marked `Checked` and is otherwise unchanged. A
//! `Drop` is not a use (elaboration decides which drops run), and an
//! assignment re-initializes its place.

use super::elaborate::{
    apply, dataflow, entry_state, gather_move_paths, rvalue_operands, statement_effects,
    terminator_effects, MovePaths, State,
};
use super::pretty::place as show_place;
use super::syntax::*;
use super::ty::TyInterner;

/// Checks `body` and, when it passes and is still `Built`, marks it
/// `Checked`. The errors name each offending use by block and statement.
pub fn check_moves(body: &mut Body, tys: &TyInterner) -> Result<(), Vec<String>> {
    let errs = move_errors(body, tys)?;
    if !errs.is_empty() {
        return Err(errs);
    }
    if body.phase == MirPhase::Built {
        body.phase = MirPhase::Checked;
    }
    Ok(())
}

fn move_errors(body: &Body, tys: &TyInterner) -> Result<Vec<String>, Vec<String>> {
    let paths = gather_move_paths(body, tys, &|t| !tys.is_copy(t)).map_err(|e| vec![e])?;
    let stmt_effects: Vec<Vec<_>> = body
        .blocks
        .iter()
        .map(|b| {
            b.statements
                .iter()
                .map(|s| statement_effects(&paths, &s.kind))
                .collect()
        })
        .collect();
    let term_effects: Vec<_> = body
        .blocks
        .iter()
        .map(|b| terminator_effects(&paths, &b.terminator.kind))
        .collect();
    let entry = entry_state(body, &paths);
    let states = dataflow(body, &paths, &entry, &stmt_effects, &term_effects);
    let reachable = reachable_blocks(body);

    let mut errs = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        if !reachable[bi] {
            continue;
        }
        let mut st = states[bi].clone();
        for (si, s) in block.statements.iter().enumerate() {
            let at = format!("bb{bi}[{si}]");
            for p in statement_uses(&s.kind) {
                check_use(body, tys, &paths, &st, &p, &at, &mut errs);
            }
            apply(&paths, &mut st, &stmt_effects[bi][si]);
        }
        let at = format!("bb{bi}[term]");
        for p in terminator_uses(&block.terminator.kind) {
            check_use(body, tys, &paths, &st, &p, &at, &mut errs);
        }
    }
    Ok(errs)
}

/// The places a statement reads, borrows or moves. An assignment's
/// destination is a write, not a use, except that writing through a
/// reference reads the reference (`written_through`).
fn statement_uses(s: &StatementKind) -> Vec<Place> {
    match s {
        StatementKind::Assign(dest, rv) => {
            let mut v: Vec<Place> = rvalue_operands(rv)
                .into_iter()
                .filter_map(Operand::place)
                .cloned()
                .collect();
            match rv {
                Rvalue::Ref(_, p)
                | Rvalue::Retain(p)
                | Rvalue::Discriminant(p)
                | Rvalue::Len(p) => v.push(p.clone()),
                _ => {}
            }
            v.extend(written_through(dest));
            v
        }
        StatementKind::SetDiscriminant(p, _) => written_through(p).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn terminator_uses(t: &TerminatorKind) -> Vec<Place> {
    match t {
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => std::iter::once(func)
            .chain(args)
            .filter_map(Operand::place)
            .cloned()
            .chain(written_through(destination))
            .collect(),
        TerminatorKind::SwitchInt { discr, .. } => discr.place().into_iter().cloned().collect(),
        // A drop is not a use of the dropped place, but dropping through a
        // reference reads the reference.
        TerminatorKind::Drop { place, .. } => written_through(place).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// A place written or dropped through a dereference reads the reference
/// it goes through: the prefix before its last `Deref`.
fn written_through(p: &Place) -> Option<Place> {
    let last = p.projection.iter().rposition(|e| *e == ProjElem::Deref)?;
    Some(Place {
        local: p.local,
        projection: p.projection[..last].to_vec(),
    })
}

/// A use of `place` is an error when any part of it may be uninitialized,
/// or when it has no move path of its own and its nearest tracked
/// ancestor may be uninitialized as a whole.
fn check_use(
    body: &Body,
    tys: &TyInterner,
    paths: &MovePaths,
    st: &State,
    place: &Place,
    at: &str,
    errs: &mut Vec<String>,
) {
    let bad = match paths.lookup(place) {
        Some(p) => paths.subtree(p).into_iter().any(|q| st.uninit[q]),
        None => {
            let mut prefix = place.clone();
            loop {
                if prefix.projection.pop().is_none() {
                    break false;
                }
                if let Some(a) = paths.lookup(&prefix) {
                    break st.uninit[a];
                }
            }
        }
    };
    if bad {
        errs.push(format!(
            "{at}: use of {}, which may have been moved or never initialized",
            show_place(body, tys, place)
        ));
    }
}

/// Blocks reachable from the entry; the builder may leave dead blocks
/// (after a `return` in the middle of a block, say), and a use there is
/// never executed.
fn reachable_blocks(body: &Body) -> Vec<bool> {
    let mut seen = vec![false; body.blocks.len()];
    let mut work = vec![0usize];
    while let Some(b) = work.pop() {
        if b >= seen.len() || seen[b] {
            continue;
        }
        seen[b] = true;
        work.extend(
            body.blocks[b]
                .terminator
                .kind
                .successors()
                .iter()
                .map(|s| s.index()),
        );
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_module;

    /// Parses `src` and move-checks every body; the errors of `main`.
    fn check(src: &str) -> Vec<String> {
        let mut m = parse_module(src).unwrap_or_else(|e| panic!("{e}"));
        let mut out = Vec::new();
        for b in &mut m.bodies {
            let name = b.instance.name.clone();
            match check_moves(b, &m.tys) {
                Ok(()) => assert_eq!(b.phase, MirPhase::Checked, "{name}"),
                Err(es) if name == "main" => out = es,
                Err(es) => panic!("{name}: {es:?}"),
            }
        }
        out
    }

    const PRELUDE: &str = "
struct P { a: String, b: String }

fn take(_1: String) -> () {
    let mut _0: ();
    bb0: {
        drop(_1) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}
";

    fn with_main(params: &str, locals: &str, blocks: &str) -> Vec<String> {
        check(&format!(
            "{PRELUDE}\nfn main({params}) -> () {{\n    let mut _0: ();\n{locals}{blocks}}}\n"
        ))
    }

    #[test]
    fn mir_movecheck_use_after_move_is_an_error() {
        let errs = with_main(
            "",
            "    let _1: String;\n    let _2: ();\n    let _3: ref String;\n",
            "    bb0: {
        _1 = String.from(const \"a\") -> bb1;
    }
    bb1: {
        _2 = take(move _1) -> bb2;
    }
    bb2: {
        _3 = &_1;
        _2 = println(copy _3) -> bb3;
    }
    bb3: {
        drop(_1) -> bb4;
    }
    bb4: {
        _0 = const ();
        return;
    }
",
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].starts_with("bb2[0]: use of _1"), "{errs:?}");
    }

    #[test]
    fn mir_movecheck_conditional_move_then_use_is_an_error() {
        let errs = with_main(
            "_1: bool",
            "    let _2: String;\n    let _3: ();\n",
            "    bb0: {
        _2 = String.from(const \"a\") -> bb1;
    }
    bb1: {
        switchInt(copy _1) -> [0: bb3, otherwise: bb2];
    }
    bb2: {
        _3 = take(move _2) -> bb3;
    }
    bb3: {
        _3 = take(move _2) -> bb4;
    }
    bb4: {
        _0 = const ();
        return;
    }
",
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].starts_with("bb3[term]: use of _2"), "{errs:?}");
    }

    /// Moving out of one field leaves the other usable, but not the whole.
    #[test]
    fn mir_movecheck_partial_move_blocks_the_whole_not_the_sibling() {
        let src = |last: &str| {
            with_main(
                "",
                "    let _1: P;\n    let _2: String;\n    let _3: String;\n    let _4: ();\n    let _5: P;\n",
                &format!(
                    "    bb0: {{
        _2 = String.from(const \"a\") -> bb1;
    }}
    bb1: {{
        _3 = String.from(const \"b\") -> bb2;
    }}
    bb2: {{
        _1 = P {{ move _2, move _3 }};
        _4 = take(move _1.0) -> bb3;
    }}
    bb3: {{
        {last}
        drop(_1) -> bb4;
    }}
    bb4: {{
        _0 = const ();
        return;
    }}
"
                ),
            )
        };
        assert_eq!(src("_2 = move _1.1;"), Vec::<String>::new());
        let errs = src("_5 = move _1;");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].starts_with("bb3[0]: use of _1"), "{errs:?}");
    }

    /// A struct with no Drop body is still moved, not copied: a type that
    /// needs no drop is tracked all the same.
    #[test]
    fn mir_movecheck_tracks_non_copy_types_without_drops() {
        let errs = check(
            "
struct Q { n: i64 }

fn eat(_1: Q) -> () {
    let mut _0: ();
    bb0: {
        _0 = const ();
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: Q;
    let _2: ();
    let _3: i64;
    bb0: {
        _1 = Q { const 1_i64 };
        _2 = eat(move _1) -> bb1;
    }
    bb1: {
        _3 = copy _1.0;
        _0 = const ();
        return;
    }
}
",
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].starts_with("bb1[0]: use of _1.0"), "{errs:?}");
    }

    /// Writing or dropping through a reference reads the reference, so a
    /// moved one cannot be written through (`*acc = v` after `a = acc`).
    #[test]
    fn mir_movecheck_write_through_a_moved_reference_is_an_error() {
        let src = |stmt: &str| {
            format!(
                "
fn bump(_1: mut ref i64) -> () {{
    let mut _0: ();
    let _2: mut ref i64;
    let _3: i64;
    bb0: {{
        _3 = copy (*_1);
        {stmt}
        (*_1) = copy _3;
        _0 = const ();
        return;
    }}
}}

fn main() -> () {{
    let mut _0: ();
    bb0: {{
        _0 = const ();
        return;
    }}
}}
"
            )
        };
        let mut m = parse_module(&src("_2 = move _1;")).unwrap_or_else(|e| panic!("{e}"));
        let errs = check_moves(&mut m.bodies[0], &m.tys).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].starts_with("bb0[2]: use of _1"), "{errs:?}");
        let mut ok = parse_module(&src("_2 = &mut (*_1);")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(check_moves(&mut ok.bodies[0], &ok.tys), Ok(()));
    }

    /// Every Built core pin is a valid program: it passes, Drops of moved
    /// places included.
    #[test]
    fn mir_movecheck_accepts_the_core_built_pins() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files: Vec<_> = std::fs::read_dir(root.join("tests/mir/core-built"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        let mut bad = Vec::new();
        for path in &files {
            let src = std::fs::read_to_string(path).unwrap();
            let mut m = parse_module(&src).unwrap();
            for b in &mut m.bodies {
                if let Err(es) = check_moves(b, &m.tys) {
                    bad.push(format!("{}: {}: {es:?}", path.display(), b.instance.name));
                }
            }
        }
        assert_eq!(files.len(), 23);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
