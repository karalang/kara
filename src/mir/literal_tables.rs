//! Builds a table of literals once, as a static, instead of on every call.
//!
//! `let codes = [".-", "-...", ...]` in a function body is a `Vec` built
//! from an array of literals: on a compiled backend, one allocation for the
//! buffer and one per `String`, then as many frees when it drops, every
//! time the function runs (kata 804 spent 58% of its time in malloc and
//! free, B-2026-10-19-34). When nothing moves the table or borrows it
//! `mut`, nothing can tell it from one built before `main`, so the pass
//! moves it into a static the way a literal in a `ref` slot already is
//! (`literal_static` in the builder): the body borrows the static, and the
//! table is never dropped.
//!
//! The shape, on an elaborated body:
//!
//! - `t = Vec[T].from_array(move a)` is `t`'s only write;
//! - `a = [e0, e1, ...]` is `a`'s only write, and `a` is named nowhere
//!   else; each `ei` is a constant or `move s` of a local `s` written only
//!   by `s = String.from(const "...")` and named nowhere else;
//! - every other mention of `t` is a shared borrow `&t`, a `drop(t)` or a
//!   storage marker.
//!
//! The writes become `goto`s and no-ops, each `&t` the static's borrow, and
//! each `drop(t)` a `goto`. `KARAC_MIR_LITERAL_TABLES=0` turns it off.

use super::interp::Program;
use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;
use crate::ids::DefId;
use std::collections::BTreeSet;

/// Promote every table of literals in `program`'s bodies to a static.
/// Coroutine bodies are left alone: the coroutine transform turns their
/// locals into frame fields.
pub fn promote_literal_tables(program: &mut Program, coroutines: &BTreeSet<String>) {
    let names: Vec<String> = program.bodies.keys().cloned().collect();
    let mut inits = Vec::new();
    for name in names {
        if name.starts_with("static.") || coroutines.contains(&name) {
            continue;
        }
        let body = program.bodies.get_mut(&name).unwrap();
        for table in tables(body) {
            let idx = program.statics.len() as u32;
            let init = format!("static.table{idx}");
            inits.push(init_body(body, &table, &init));
            program.statics.push(StaticDef {
                name: format!("table{idx}"),
                ty: body.local(table.t).ty,
                mutable: false,
                init,
            });
            rewrite(body, &table, idx);
        }
    }
    for b in inits {
        program.add(b);
    }
}

/// One table: its local, where each write is, and what it is made of.
struct Table {
    t: Local,
    /// The block whose `Call` is `t = Vec[T].from_array(move a)`.
    from_array: BasicBlock,
    a: Local,
    /// The statement `a = [...]`.
    aggregate: (BasicBlock, usize),
    /// Each `s = String.from(const "...")` call's block, by element local.
    strings: Vec<(Local, BasicBlock)>,
}

fn callee(func: &Operand) -> Option<&str> {
    match func {
        Operand::Const(Const {
            kind: ConstKind::FnDef(i),
            ..
        }) => Some(&i.name),
        _ => None,
    }
}

/// How many statements and terminators of `body` name `l`.
fn mention_count(body: &Body, l: Local) -> usize {
    body.blocks
        .iter()
        .map(|b| {
            b.statements
                .iter()
                .filter(|s| statement_mentions(&s.kind, l))
                .filter(|s| {
                    !matches!(
                        s.kind,
                        StatementKind::StorageLive(_) | StatementKind::StorageDead(_)
                    )
                })
                .count()
                + usize::from(terminator_mentions(&b.terminator.kind, l))
        })
        .sum()
}

/// The `Call` writing the whole of `l` alone, if `l` has exactly one.
fn only_call_into(body: &Body, l: Local) -> Option<(BasicBlock, &str, &[Operand])> {
    let mut found = None;
    for (i, b) in body.blocks.iter().enumerate() {
        if let TerminatorKind::Call {
            func,
            args,
            destination,
            target: Some(_),
            ..
        } = &b.terminator.kind
        {
            if destination.local == l {
                if !destination.projection.is_empty() || found.is_some() {
                    return None;
                }
                found = Some((BasicBlock(i as u32), callee(func)?, args.as_slice()));
            }
        }
    }
    found
}

fn tables(body: &Body) -> Vec<Table> {
    let mut out = Vec::new();
    for (ti, decl) in body.locals.iter().enumerate() {
        let t = Local(ti as u32);
        if matches!(decl.kind, LocalKind::ReturnPlace | LocalKind::Arg { .. }) {
            continue;
        }
        let Some((from_array, name, args)) = only_call_into(body, t) else {
            continue;
        };
        if !(name.starts_with("Vec[") && name.ends_with("].from_array")) {
            continue;
        }
        let [Operand::Move(ap)] = args else {
            continue;
        };
        if !ap.projection.is_empty() {
            continue;
        }
        let a = ap.local;
        // `a = [...]`, and `a` named by it and the call only.
        let mut aggregate = None;
        for (bi, b) in body.blocks.iter().enumerate() {
            for (si, s) in b.statements.iter().enumerate() {
                if let StatementKind::Assign(p, Rvalue::Aggregate(AggregateKind::Array(_), _)) =
                    &s.kind
                {
                    if p.local == a && p.projection.is_empty() {
                        aggregate = Some((BasicBlock(bi as u32), si));
                    }
                }
            }
        }
        let Some((ab, asi)) = aggregate else {
            continue;
        };
        if mention_count(body, a) != 2 {
            continue;
        }
        let StatementKind::Assign(_, Rvalue::Aggregate(_, ops)) =
            &body.block(ab).statements[asi].kind
        else {
            continue;
        };
        let mut strings = Vec::new();
        let ok = ops.iter().all(|op| match op {
            Operand::Const(c) => !matches!(c.kind, ConstKind::Static(_) | ConstKind::FnDef(_)),
            Operand::Move(sp) if sp.projection.is_empty() => {
                let s = sp.local;
                match only_call_into(body, s) {
                    Some((
                        sb,
                        "String.from",
                        [Operand::Const(Const {
                            kind: ConstKind::Str(_),
                            ..
                        })],
                    )) if mention_count(body, s) == 2 => {
                        strings.push((s, sb));
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        });
        if !ok {
            continue;
        }
        // Every other mention of `t` borrows it shared or drops it.
        let mut others = 0;
        for b in &body.blocks {
            for s in &b.statements {
                if let StatementKind::Assign(d, Rvalue::Ref(BorrowKind::Shared, p)) = &s.kind {
                    if p.local == t
                        && p.projection.is_empty()
                        && d.projection.is_empty()
                        && d.local != t
                    {
                        others += 1;
                    }
                }
            }
            if let TerminatorKind::Drop { place, .. } = &b.terminator.kind {
                if place.local == t && place.projection.is_empty() {
                    others += 1;
                }
            }
        }
        if mention_count(body, t) != 1 + others {
            continue;
        }
        out.push(Table {
            t,
            from_array,
            a,
            aggregate: (ab, asi),
            strings,
        });
    }
    out
}

/// The static's initializer: the table's writes, in order, into its return
/// place.
fn init_body(body: &Body, table: &Table, name: &str) -> Body {
    let si = body.block(table.from_array).terminator.source_info;
    let mut locals = vec![LocalDecl {
        ty: body.local(table.t).ty,
        mutability: Mutability::Mut,
        kind: LocalKind::ReturnPlace,
        source_info: si,
    }];
    let mut map = std::collections::HashMap::new();
    for l in std::iter::once(table.a).chain(table.strings.iter().map(|(s, _)| *s)) {
        map.insert(l, Local(locals.len() as u32));
        let mut d = body.local(l).clone();
        d.kind = LocalKind::Temp;
        locals.push(d);
    }
    let local = |l: Local| Place::local(map[&l]);
    let mut blocks = Vec::new();
    let next = |blocks: &Vec<BasicBlockData>| BasicBlock(blocks.len() as u32 + 1);
    for &(s, sb) in &table.strings {
        let term = &body.block(sb).terminator;
        let TerminatorKind::Call { func, args, .. } = &term.kind else {
            unreachable!("checked in `tables`");
        };
        blocks.push(BasicBlockData {
            statements: Vec::new(),
            terminator: Terminator {
                kind: TerminatorKind::Call {
                    func: func.clone(),
                    args: args.clone(),
                    destination: local(s),
                    target: Some(next(&blocks)),
                    unwind: UnwindAction::Abort,
                },
                source_info: term.source_info,
            },
        });
    }
    let agg = body.block(table.aggregate.0).statements[table.aggregate.1].clone();
    let StatementKind::Assign(_, Rvalue::Aggregate(kind, ops)) = agg.kind else {
        unreachable!("checked in `tables`");
    };
    let ops = ops
        .into_iter()
        .map(|op| match op {
            Operand::Move(p) => Operand::Move(local(p.local)),
            c => c,
        })
        .collect();
    let TerminatorKind::Call { func, .. } = &body.block(table.from_array).terminator.kind else {
        unreachable!("checked in `tables`");
    };
    blocks.push(BasicBlockData {
        statements: vec![Statement {
            kind: StatementKind::Assign(local(table.a), Rvalue::Aggregate(kind, ops)),
            source_info: agg.source_info,
        }],
        terminator: Terminator {
            kind: TerminatorKind::Call {
                func: func.clone(),
                args: vec![Operand::Move(local(table.a))],
                destination: Place::local(Local::RETURN_PLACE),
                target: Some(next(&blocks)),
                unwind: UnwindAction::Abort,
            },
            source_info: si,
        },
    });
    blocks.push(BasicBlockData {
        statements: Vec::new(),
        terminator: Terminator {
            kind: TerminatorKind::Return,
            source_info: si,
        },
    });
    Body {
        instance: InstanceId {
            def: DefId(u32::MAX),
            args: Vec::new(),
            name: name.to_string(),
        },
        locals,
        arg_count: 0,
        blocks,
        scopes: body.scopes.clone(),
        phase: body.phase,
        span: body.span,
        par_regions: Vec::new(),
        provided: Vec::new(),
        suspends: false,
        drop_flags: Vec::new(),
    }
}

/// Take the table's writes and drops out of `body`, and borrow the static
/// where it borrowed the table.
fn rewrite(body: &mut Body, table: &Table, idx: u32) {
    let skip = |body: &mut Body, b: BasicBlock| {
        let term = &mut body.blocks[b.index()].terminator;
        if let TerminatorKind::Call {
            target: Some(target),
            ..
        } = term.kind
        {
            term.kind = TerminatorKind::Goto { target };
        }
    };
    for &(_, sb) in &table.strings {
        skip(body, sb);
    }
    skip(body, table.from_array);
    body.blocks[table.aggregate.0.index()].statements[table.aggregate.1].kind = StatementKind::Nop;
    let tys: Vec<_> = body.locals.iter().map(|d| d.ty).collect();
    for b in &mut body.blocks {
        for s in &mut b.statements {
            if let StatementKind::Assign(d, Rvalue::Ref(BorrowKind::Shared, p)) = &s.kind {
                if p.local == table.t && p.projection.is_empty() {
                    let d = d.clone();
                    // The borrow's type is the destination's: `ref T`.
                    let ty = tys[d.local.index()];
                    s.kind = StatementKind::Assign(
                        d,
                        Rvalue::Use(Operand::Const(Const {
                            ty,
                            kind: ConstKind::Static(idx),
                        })),
                    );
                }
            }
        }
        if let TerminatorKind::Drop { place, target, .. } = &b.terminator.kind {
            if place.local == table.t {
                b.terminator.kind = TerminatorKind::Goto { target: *target };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::{ConstKind, Operand, TerminatorKind};

    const TABLES: &str = r#"
fn morse(word: String) -> String {
    let codes = [".-", "-...", "-.-.", "-.."];
    let mut out = String.new();
    for b in word.bytes() {
        out.push_str(codes[(b - 97) as i64]);
    }
    out
}

fn step(k: i64) -> i64 {
    let d = [1, 0, -1, 0];
    d[k] + d.len() as i64
}

fn grown() -> Vec[String] {
    let mut t = ["x", "y"];
    t.push("z".to_string());
    t
}

fn kept() -> Vec[i64] {
    let t = [7, 8];
    t
}

fn main() {
    for w in ["abc", "dcba", "a"] {
        println(morse(w.to_string()));
    }
    println(f"{step(0)} {step(2)}");
    println(f"{grown().len()} {kept()[1]}");
}
"#;

    /// The calls a body makes, by callee name.
    fn calls(src: &str, f: &str) -> Vec<String> {
        let l = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = &l.program.bodies[f];
        body.blocks
            .iter()
            .filter_map(|b| match &b.terminator.kind {
                TerminatorKind::Call {
                    func:
                        Operand::Const(crate::mir::Const {
                            kind: ConstKind::FnDef(i),
                            ..
                        }),
                    ..
                } => Some(i.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_borrowed_table_of_literals_is_built_once() {
        let morse = calls(TABLES, "morse");
        assert!(
            !morse
                .iter()
                .any(|c| c == "String.from" || c.ends_with(".from_array")),
            "{morse:?}"
        );
        let step = calls(TABLES, "step");
        assert!(!step.iter().any(|c| c.ends_with(".from_array")), "{step:?}");
    }

    #[test]
    fn a_table_pushed_to_or_moved_out_stays_local() {
        assert!(calls(TABLES, "grown")
            .iter()
            .any(|c| c.ends_with(".from_array")));
        assert!(calls(TABLES, "kept")
            .iter()
            .any(|c| c.ends_with(".from_array")));
    }

    #[test]
    fn promoted_tables_print_what_local_ones_do() {
        let (out, code) = run_source(TABLES).unwrap();
        assert_eq!(out, ".--...-.-.\n-..-.-.-....-\n.-\n5 3\n3 8\n");
        assert_eq!(code, Some(0));
    }
}
