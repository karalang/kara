//! The `String` locals a char's `to_string()` fills that live only to be
//! lent (B-2026-11-10-25): codegen writes the char's UTF-8 bytes into a
//! stack buffer of the frame and points the `String` at it, with no
//! capacity, rather than allocate. `"aeiou".contains(c.to_string())`, the
//! char-pattern fix-it's form, allocated and freed a `String` per call.
//!
//! A local other than the result and the arguments qualifies when every
//! mention of it is a `char.to_string` call that fills it, a shared borrow of the whole local, its drop, or its
//! storage markers. Never moved or copied, its value cannot leave the frame,
//! and a borrow cannot outlive the local, so the buffer outlives every
//! reader. No capacity means its drop frees nothing.

use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;

/// Whether each local of `body` is such a `String`.
pub fn borrowed_char_strings(body: &Body) -> Vec<bool> {
    let n = body.locals.len();
    let mut filled = vec![false; n];
    for b in &body.blocks {
        if let TerminatorKind::Call {
            func, destination, ..
        } = &b.terminator.kind
        {
            if is_char_to_string(func) && destination.projection.is_empty() {
                filled[destination.local.index()] = true;
            }
        }
    }
    for (l, f) in filled.iter_mut().enumerate() {
        // The result leaves by `Return`, which names no local, and an
        // argument's slot is the caller's.
        if l <= body.arg_count {
            *f = false;
        }
        if !*f {
            continue;
        }
        let l = Local(l as u32);
        *f = body.blocks.iter().all(|b| {
            terminator_ok(&b.terminator.kind, l)
                && b.statements.iter().all(|s| statement_ok(&s.kind, l))
        });
    }
    filled
}

fn is_char_to_string(func: &Operand) -> bool {
    matches!(
        func,
        Operand::Const(Const { kind: ConstKind::FnDef(i), .. }) if i.name == "char.to_string"
    )
}

fn terminator_ok(t: &TerminatorKind, l: Local) -> bool {
    if !terminator_mentions(t, l) {
        return true;
    }
    match t {
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            is_char_to_string(func)
                && *destination == Place::local(l)
                && args.iter().all(|a| a.place().is_none_or(|p| p.local != l))
        }
        TerminatorKind::Drop { place, .. } => *place == Place::local(l),
        _ => false,
    }
}

fn statement_ok(s: &StatementKind, l: Local) -> bool {
    if !statement_mentions(s, l) {
        return true;
    }
    match s {
        StatementKind::Assign(d, Rvalue::Ref(BorrowKind::Shared, p)) => {
            d.local != l && *p == Place::local(l)
        }
        StatementKind::StorageLive(_) | StatementKind::StorageDead(_) => true,
        _ => false,
    }
}
