//! Walks the chars of an all-ASCII literal one byte at a time.
//!
//! `StrChars.next` decodes the char at `pos` and steps `pos` by its width.
//! The width is 1 for an ASCII byte and comes from the decoder otherwise,
//! so the two meet in a PHI and LLVM cannot see that `pos` counts the
//! trips: a search over a constant string, such as #127's
//! `for ch in alphabet.chars() { if i == target { return ch } ... }` over
//! `"abcdefghijklmnopqrstuvwxyz"`, stays a loop of 26 decodes where it
//! could be one indexed load (280 instructions a call against legacy's 10).
//! The legacy backend proves the same thing on the AST
//! (`codegen/ascii_const_chars.rs`, where the measurements are); this is
//! that proof on an elaborated MIR body, and the `next` it unlocks is
//! written out in the body:
//!
//! ```text
//!   g = Ge(copy (*m).1, copy (*m).2); switchInt(copy g) -> [0: A, otherwise: N]
//!   N: d = Option.None; goto T
//!   A: r = &(*(*m).0); c = String.ascii_at_byte(move r, copy (*m).1) -> B
//!   B: q = Add(copy (*m).1, const 1); (*m).1 = copy q; d = Option.Some(copy c); goto T
//! ```
//!
//! `ascii_at_byte` is the byte at `pos` as a char, bounds checked. On an
//! ASCII text every byte starts a char of width 1, so this is
//! `StrChars.next` exactly; `pos < back <= len` keeps `pos + 1` in range.
//!
//! Getting the proof wrong is a silent miscompile (a 2-byte `é` would come
//! out as two chars), so every rule is a whitelist:
//!
//! - `s = String.from(const "<ascii>")` is `s`'s only write, and every
//!   other mention of `s` is a whole shared borrow `r = &s`, a `drop(s)` or
//!   a storage marker;
//! - `it = String.chars(move r)` is the iterator's only write, with `r`
//!   written only by `r = &s`;
//! - every other mention of `it` is `m = &mut it`, a `drop(it)` or a
//!   storage marker, and each such `m` is written only there and named
//!   nowhere but as the argument of `StrChars.next(move m)`.
//!
//! Nothing else can reach the text or the cursor: Kāra has no borrow of a
//! `String` that writes but `&mut`, and the cursor's only borrow is the one
//! each `next` takes. `KARAC_MIR_ASCII_CHARS=0` turns it off.

use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;
use super::ty::{IntTy, TyInterner, TyKind};
use crate::ids::DefId;

/// Writes out each `StrChars.next` in `body` whose text is a literal of
/// ASCII bytes.
pub fn walk_ascii_chars(body: &mut Body, tys: &TyInterner) {
    let sites = sites(body);
    for (b, m) in sites {
        rewrite(body, tys, b, m);
    }
}

fn fn_name(func: &Operand) -> Option<&str> {
    match func {
        Operand::Const(Const {
            kind: ConstKind::FnDef(i),
            ..
        }) => Some(i.name.as_str()),
        _ => None,
    }
}

fn whole(p: &Place) -> Option<Local> {
    p.projection.is_empty().then_some(p.local)
}

fn moved_local(o: &Operand) -> Option<Local> {
    match o {
        Operand::Move(p) => whole(p),
        _ => None,
    }
}

/// `x = &[mut] y`, both whole: `(x, kind, y)`.
fn borrow(s: &StatementKind) -> Option<(Local, BorrowKind, Local)> {
    match s {
        StatementKind::Assign(d, Rvalue::Ref(k, p)) => Some((whole(d)?, *k, whole(p)?)),
        _ => None,
    }
}

/// Every statement and terminator of `body` that mentions `l`.
fn mentions(body: &Body, l: Local) -> Vec<Mention<'_>> {
    let mut out = Vec::new();
    for bb in &body.blocks {
        for s in &bb.statements {
            if statement_mentions(&s.kind, l) {
                out.push(Mention::Stmt(&s.kind));
            }
        }
        if terminator_mentions(&bb.terminator.kind, l) {
            out.push(Mention::Term(&bb.terminator.kind));
        }
    }
    out
}

enum Mention<'b> {
    Stmt(&'b StatementKind),
    Term(&'b TerminatorKind),
}

/// A storage marker or a whole `drop(l)`.
fn is_storage_or_drop(m: &Mention, l: Local) -> bool {
    match m {
        Mention::Stmt(StatementKind::StorageLive(x) | StatementKind::StorageDead(x)) => *x == l,
        Mention::Term(TerminatorKind::Drop { place, .. }) => whole(place) == Some(l),
        _ => false,
    }
}

/// Is `s` a `String` whose only value is an ASCII literal, borrowed only
/// shared?
fn ascii_text(body: &Body, s: Local) -> bool {
    let mut defs = 0;
    for m in mentions(body, s) {
        if is_storage_or_drop(&m, s) {
            continue;
        }
        match m {
            Mention::Stmt(k) if matches!(borrow(k), Some((_, BorrowKind::Shared, y)) if y == s) => {
            }
            Mention::Term(TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            }) if whole(destination) == Some(s)
                && fn_name(func) == Some("String.from")
                && matches!(args.as_slice(), [Operand::Const(Const {
                    kind: ConstKind::Str(t),
                    ..
                })] if t.is_ascii()) =>
            {
                defs += 1;
            }
            _ => return false,
        }
    }
    defs == 1
}

/// The `y` of `x`'s only write when that write is `x = &y`.
fn only_shared_borrow_of(body: &Body, x: Local) -> Option<Local> {
    let mut found = None;
    for bb in &body.blocks {
        for st in &bb.statements {
            let writes = match &st.kind {
                StatementKind::Assign(d, _) | StatementKind::SetDiscriminant(d, _) => d.local == x,
                _ => false,
            };
            if writes {
                match borrow(&st.kind) {
                    Some((_, BorrowKind::Shared, y)) if found.is_none() => found = Some(y),
                    _ => return None,
                }
            }
        }
        if let TerminatorKind::Call { destination, .. } = &bb.terminator.kind {
            if destination.local == x {
                return None;
            }
        }
    }
    found
}

/// The blocks whose terminator is a `StrChars.next(move m)` over an ASCII
/// literal, with their `m`.
fn sites(body: &Body) -> Vec<(usize, Local)> {
    let mut out = Vec::new();
    // Each iterator: `it = String.chars(move r)`, `r = &s`, `s` ASCII.
    let mut iters = Vec::new();
    for bb in &body.blocks {
        if let TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } = &bb.terminator.kind
        {
            if fn_name(func) != Some("String.chars") {
                continue;
            }
            let (Some(it), [a]) = (whole(destination), args.as_slice()) else {
                continue;
            };
            let Some(r) = moved_local(a) else { continue };
            if only_shared_borrow_of(body, r).is_some_and(|s| ascii_text(body, s)) {
                iters.push(it);
            }
        }
    }
    'iters: for it in iters {
        let mut defs = 0;
        let mut cursors = Vec::new();
        for m in mentions(body, it) {
            if is_storage_or_drop(&m, it) {
                continue;
            }
            match m {
                Mention::Stmt(k) => match borrow(k) {
                    Some((c, BorrowKind::Mut, y)) if y == it => cursors.push(c),
                    _ => continue 'iters,
                },
                Mention::Term(TerminatorKind::Call {
                    func, destination, ..
                }) if whole(destination) == Some(it) && fn_name(func) == Some("String.chars") => {
                    defs += 1
                }
                _ => continue 'iters,
            }
        }
        if defs != 1 {
            continue;
        }
        let mut found = Vec::new();
        for &c in &cursors {
            // Written once, by `c = &mut it`, and otherwise only the
            // argument of a `next`.
            let mut writes = 0;
            for m in mentions(body, c) {
                match m {
                    Mention::Stmt(k) if matches!(borrow(k), Some((x, BorrowKind::Mut, y)) if x == c && y == it) => {
                        writes += 1
                    }
                    Mention::Stmt(
                        StatementKind::StorageLive(_) | StatementKind::StorageDead(_),
                    ) => {}
                    Mention::Term(TerminatorKind::Call {
                        func,
                        args,
                        destination,
                        ..
                    }) if fn_name(func) == Some("StrChars.next")
                        && destination.local != c
                        && matches!(args.as_slice(), [a] if moved_local(a) == Some(c)) => {}
                    _ => continue 'iters,
                }
            }
            if writes != 1 {
                continue 'iters;
            }
        }
        for (i, bb) in body.blocks.iter().enumerate() {
            if let TerminatorKind::Call { func, args, .. } = &bb.terminator.kind {
                if fn_name(func) == Some("StrChars.next") {
                    if let [a] = args.as_slice() {
                        if let Some(c) = moved_local(a) {
                            if cursors.contains(&c) {
                                found.push((i, c));
                            }
                        }
                    }
                }
            }
        }
        out.extend(found);
    }
    out
}

fn rewrite(body: &mut Body, tys: &TyInterner, b: usize, m: Local) {
    let TerminatorKind::Call {
        destination,
        target: Some(target),
        unwind,
        ..
    } = body.blocks[b].terminator.kind.clone()
    else {
        return;
    };
    let info = body.blocks[b].terminator.source_info;
    let TyKind::MutRef(it_ty) = tys.kind(body.local(m).ty) else {
        return;
    };
    let Ok(opt) = super::place_ty::place_ty(body, tys, &destination) else {
        return;
    };
    let opt = opt.ty;
    let TyKind::Adt(a) = tys.kind(opt) else {
        return;
    };
    let variants: Vec<String> = tys.adt(a).variants.iter().map(|v| v.name.clone()).collect();
    let (Some(none), Some(some)) = (
        variants.iter().position(|v| v == "None"),
        variants.iter().position(|v| v == "Some"),
    ) else {
        return;
    };
    let (Some(text_ty), Some(i64_ty)) =
        (tys.field_ty(it_ty, None, 0), tys.field_ty(it_ty, None, 1))
    else {
        return;
    };
    if !matches!(tys.kind(i64_ty), TyKind::Int(IntTy::I64))
        || tys.field_ty(it_ty, None, 2) != Some(i64_ty)
    {
        return;
    }
    let bool_ty = tys.intern(TyKind::Bool);
    let char_ty = tys.intern(TyKind::Char);
    let mut fresh = |ty| {
        let l = Local(body.locals.len() as u32);
        body.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: info,
        });
        l
    };
    let (g, r, c, q) = (
        fresh(bool_ty),
        fresh(text_ty),
        fresh(char_ty),
        fresh(i64_ty),
    );
    let cursor = Place::local(m).project(ProjElem::Deref);
    let pos = cursor.field(1, i64_ty);
    let back = cursor.field(2, i64_ty);
    let text = cursor.field(0, text_ty).project(ProjElem::Deref);
    let n = body.blocks.len() as u32;
    let (none_bb, at_bb, step_bb) = (BasicBlock(n), BasicBlock(n + 1), BasicBlock(n + 2));
    let stmt = |kind| Statement {
        kind,
        source_info: info,
    };
    let assign = |p: Place, rv| stmt(StatementKind::Assign(p, rv));
    let blk = &mut body.blocks[b];
    blk.statements.push(assign(
        Place::local(g),
        Rvalue::BinaryOp(BinOp::Ge, Operand::Copy(pos.clone()), Operand::Copy(back)),
    ));
    blk.terminator.kind = TerminatorKind::SwitchInt {
        discr: Operand::Copy(Place::local(g)),
        targets: SwitchTargets::if_else(none_bb, at_bb),
    };
    let goto = Terminator {
        kind: TerminatorKind::Goto { target },
        source_info: info,
    };
    body.blocks.push(BasicBlockData {
        statements: vec![assign(
            destination.clone(),
            Rvalue::Aggregate(
                AggregateKind::Adt {
                    ty: opt,
                    variant: VariantIdx(none as u32),
                },
                Vec::new(),
            ),
        )],
        terminator: goto.clone(),
    });
    let func = Operand::Const(Const {
        ty: tys.intern(TyKind::Unit),
        kind: ConstKind::FnDef(InstanceId {
            def: DefId(u32::MAX),
            args: Vec::new(),
            name: "String.ascii_at_byte".into(),
        }),
    });
    body.blocks.push(BasicBlockData {
        statements: vec![assign(
            Place::local(r),
            Rvalue::Ref(BorrowKind::Shared, text),
        )],
        terminator: Terminator {
            kind: TerminatorKind::Call {
                func,
                args: vec![Operand::Move(Place::local(r)), Operand::Copy(pos.clone())],
                destination: Place::local(c),
                target: Some(step_bb),
                unwind,
            },
            source_info: info,
        },
    });
    let one = Operand::Const(Const {
        ty: i64_ty,
        kind: ConstKind::Scalar(1),
    });
    body.blocks.push(BasicBlockData {
        statements: vec![
            assign(
                Place::local(q),
                Rvalue::BinaryOp(BinOp::Add, Operand::Copy(pos.clone()), one),
            ),
            assign(pos, Rvalue::Use(Operand::Copy(Place::local(q)))),
            assign(
                destination,
                Rvalue::Aggregate(
                    AggregateKind::Adt {
                        ty: opt,
                        variant: VariantIdx(some as u32),
                    },
                    vec![Operand::Copy(Place::local(c))],
                ),
            ),
        ],
        terminator: goto,
    });
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::TerminatorKind;

    const SRC: &str = r#"
fn find(n: i64) -> char {
    let a: String = "abcdefghijklmnopqrstuvwxyz";
    let mut i = 0;
    for ch in a.chars() {
        if i == n {
            return ch;
        }
        i = i + 1;
    }
    '?'
}
fn wide(n: i64) -> char {
    let a: String = "aéb€c";
    let mut i = 0;
    for ch in a.chars() {
        if i == n {
            return ch;
        }
        i = i + 1;
    }
    '?'
}
fn grown() -> String {
    let mut a: String = "ab";
    a.push('é');
    let mut out: String = "";
    for ch in a.chars() {
        out.push(ch);
    }
    out
}
fn manual() -> String {
    let a: String = "xyz";
    let mut it = a.chars();
    let mut out: String = "";
    loop {
        match it.next() {
            Some(c) => out.push(c),
            None => break,
        }
    }
    out
}
fn backwards() -> String {
    let a: String = "abc";
    let mut out: String = "";
    for ch in a.chars().rev() {
        out.push(ch);
    }
    out
}
fn main() {
    println(f"{find(0)} {find(25)} {find(26)} {wide(1)} {wide(3)} {grown()} {manual()} {backwards()}");
}
"#;

    /// How many `ascii_at_byte` steps `f`'s body takes.
    fn steps(f: &str) -> usize {
        let l = build_source(SRC).unwrap_or_else(|e| panic!("{e}"));
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == f)
            .unwrap_or_else(|| panic!("no body {f}"));
        body.blocks
            .iter()
            .filter(|b| {
                matches!(&b.terminator.kind, TerminatorKind::Call { func, .. }
                    if format!("{func:?}").contains("ascii_at_byte"))
            })
            .count()
    }

    /// An ASCII literal's cursor, in a `for` or stepped by hand, is
    /// written out; a literal with a wide char, a text that grows, and a
    /// cursor walked backwards are not.
    #[test]
    fn mir_ascii_chars_writes_out_only_a_proven_text() {
        assert_eq!(steps("find"), 1);
        assert_eq!(steps("manual"), 1);
        assert_eq!(steps("wide"), 0);
        assert_eq!(steps("grown"), 0);
        assert_eq!(steps("backwards"), 0);
        let (out, _) = run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out, "a z ? é € abé xyz cba\n");
    }
}
