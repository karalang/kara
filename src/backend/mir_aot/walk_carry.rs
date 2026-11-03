//! Carries a hashed table's walk index from one trip of a loop to the next
//! (B-2026-11-01-44).
//!
//! The `i`th step of a walk visits the entry at `j = (a * i + c) mod n`
//! (`karac_hash::walk_index`). Compiled code computes it through the
//! internal function `karac.walk.index(i, n, a, c, m)`, `m` the
//! reciprocal of `n` (`src/backend/mir/table.rs`), which reads no memory, so
//! the IR passes hoist and share it like arithmetic. Each call still costs
//! a multiply, a high multiply, another multiply and a correction, on the
//! path from the step's index to the entry it loads, and a Map walk that
//! probes the table at every step ran about 1.3x a walk over a `Vec` of the
//! same keys (kata 170).
//!
//! After the IR passes, a call whose `i` is `iv + k`, where `iv` is a phi of
//! a loop header `h` that the back edge steps by a constant `s`, and whose
//! `n`, `a`, `c` and `m` are defined outside the loop, becomes a phi of `h`:
//! `j0` = the call's value for the first trip, computed at the end of the
//! preheader `p` (which branches only to `h`), and at the latch
//! `j + d mod n`, with `d = s * a mod n` computed once in `p`. `iv` takes
//! `iv + s` on the back edge, so the phi tracks `walk_index(iv + k)` on
//! every trip, whether or not the trip reaches the call. Every other call
//! is expanded in place. `KARAC_MIR_WALK_CARRY=0` expands them all;
//! `=report` names each function with a carried index on stderr.

use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMIntPredicate, LLVMOpcode};

use super::carry_store::{linear, opcode, successors};

/// The marker's name, as `src/backend/mir/table.rs` declares it.
pub(crate) const MARKER: &str = "karac.walk.index";

/// The largest back-edge step carried.
const MAX_STEP: i64 = 1 << 16;

/// Carries or expands every call of the walk-index marker in `m`, then
/// drops the marker. Returns how many calls were carried.
///
/// # Safety
/// `m` is a valid module.
pub(super) unsafe fn carry_walks(m: LLVMModuleRef) -> usize {
    let flag = std::env::var("KARAC_MIR_WALK_CARRY");
    let on = flag.as_deref() != Ok("0");
    let report = flag.as_deref() == Ok("report");
    let mut n = 0;
    unsafe {
        let name = std::ffi::CString::new(MARKER).unwrap_or_default();
        let marker = LLVMGetNamedFunction(m, name.as_ptr());
        if marker.is_null() {
            return 0;
        }
        let b = LLVMCreateBuilderInContext(LLVMGetModuleContext(m));
        let mut calls = Vec::new();
        let mut u = LLVMGetFirstUse(marker);
        while !u.is_null() {
            let user = LLVMGetUser(u);
            if !LLVMIsACallInst(user).is_null() && LLVMGetCalledValue(user) == marker {
                calls.push(user);
            }
            u = LLVMGetNextUse(u);
        }
        let mut per: Vec<(LLVMValueRef, usize)> = Vec::new();
        for call in calls {
            if on && carry(call, b) {
                let f = LLVMGetBasicBlockParent(LLVMGetInstructionParent(call));
                match per.iter_mut().find(|(g, _)| *g == f) {
                    Some((_, k)) => *k += 1,
                    None => per.push((f, 1)),
                }
                LLVMInstructionEraseFromParent(call);
                n += 1;
                continue;
            }
            LLVMPositionBuilderBefore(b, call);
            let o = [0, 1, 2, 3, 4].map(|k| LLVMGetOperand(call, k));
            let j = index(b, o[0], o[1], o[2], o[3], o[4]);
            LLVMReplaceAllUsesWith(call, j);
            LLVMInstructionEraseFromParent(call);
        }
        if report {
            for (f, k) in per {
                let mut len = 0;
                let name = LLVMGetValueName2(f, &mut len);
                let name = std::slice::from_raw_parts(name.cast::<u8>(), len);
                eprintln!("walk-carry: {} in {}", k, String::from_utf8_lossy(name));
            }
        }
        if LLVMGetFirstUse(marker).is_null() {
            LLVMDeleteFunction(marker);
        }
        LLVMDisposeBuilder(b);
    }
    n
}

/// `(a * i + c) mod n` at the builder, for `i < n < 2^32` and `m` the
/// reciprocal of `n`: `karac_hash::walk_index_near`.
pub(super) unsafe fn index(
    b: LLVMBuilderRef,
    i: LLVMValueRef,
    n: LLVMValueRef,
    a: LLVMValueRef,
    c: LLVMValueRef,
    m: LLVMValueRef,
) -> LLVMValueRef {
    unsafe {
        let cx = LLVMGetTypeContext(LLVMTypeOf(i));
        let (i64t, i128t) = (LLVMInt64TypeInContext(cx), LLVMInt128TypeInContext(cx));
        let nm = c"";
        let x = LLVMBuildMul(b, a, i, nm.as_ptr());
        let x = LLVMBuildAdd(b, x, c, nm.as_ptr());
        let wx = LLVMBuildZExt(b, x, i128t, nm.as_ptr());
        let wm = LLVMBuildZExt(b, m, i128t, nm.as_ptr());
        let p = LLVMBuildMul(b, wx, wm, nm.as_ptr());
        let hi = LLVMBuildLShr(b, p, LLVMConstInt(i128t, 64, 0), nm.as_ptr());
        let q = LLVMBuildTrunc(b, hi, i64t, nm.as_ptr());
        let qn = LLVMBuildMul(b, q, n, nm.as_ptr());
        let r = LLVMBuildSub(b, x, qn, nm.as_ptr());
        wrap(b, r, n)
    }
}

/// `r` less `n` when `r >= n`, for `r < 2n`.
unsafe fn wrap(b: LLVMBuilderRef, r: LLVMValueRef, n: LLVMValueRef) -> LLVMValueRef {
    unsafe {
        let nm = c"";
        let over = LLVMBuildICmp(b, LLVMIntPredicate::LLVMIntUGE, r, n, nm.as_ptr());
        let less = LLVMBuildSub(b, r, n, nm.as_ptr());
        LLVMBuildSelect(b, over, less, r, nm.as_ptr())
    }
}

/// Distinct predecessors of each block of `f`.
unsafe fn predecessors(
    f: LLVMValueRef,
) -> rustc_hash::FxHashMap<LLVMBasicBlockRef, Vec<LLVMBasicBlockRef>> {
    let mut preds: rustc_hash::FxHashMap<LLVMBasicBlockRef, Vec<LLVMBasicBlockRef>> =
        Default::default();
    unsafe {
        let mut b = LLVMGetFirstBasicBlock(f);
        while !b.is_null() {
            for s in successors(b) {
                let e = preds.entry(s).or_default();
                if !e.contains(&b) {
                    e.push(b);
                }
            }
            b = LLVMGetNextBasicBlock(b);
        }
    }
    preds
}

/// Replaces walk-index `call` by a phi of its loop, if its loop allows it
/// (see the module comment); the call is left for the caller to erase.
unsafe fn carry(call: LLVMValueRef, bld: LLVMBuilderRef) -> bool {
    use LLVMOpcode::*;
    unsafe {
        let [i, n, a, c, m] = [0, 1, 2, 3, 4].map(|k| LLVMGetOperand(call, k));
        // `i` is `iv + k` for a header phi `iv`.
        let mut iv = i;
        let mut k = 0i64;
        if opcode(i) == Some(LLVMAdd) {
            let (x, y) = (LLVMGetOperand(i, 0), LLVMGetOperand(i, 1));
            if LLVMIsAConstantInt(y).is_null() {
                return false;
            }
            iv = x;
            k = LLVMConstIntGetSExtValue(y);
        }
        if opcode(iv) != Some(LLVMPHI) || LLVMCountIncoming(iv) != 2 {
            return false;
        }
        let h = LLVMGetInstructionParent(iv);
        let f = LLVMGetBasicBlockParent(h);
        let preds = predecessors(f);
        let Some(hp) = preds.get(&h).filter(|v| v.len() == 2) else {
            return false;
        };
        // The latch is the predecessor `h` dominates on the way round: the
        // one whose incoming value steps `iv`.
        let mut found = None;
        for x in 0..2 {
            let (src, val) = (LLVMGetIncomingBlock(iv, x), LLVMGetIncomingValue(iv, x));
            let Some((1, s)) = linear(val, iv, 1).filter(|_| opcode(val) == Some(LLVMAdd)) else {
                continue;
            };
            let other = LLVMGetIncomingBlock(iv, 1 - x);
            if hp.contains(&src) && hp.contains(&other) && src != other {
                found = Some((other, LLVMGetIncomingValue(iv, 1 - x), src, s));
            }
        }
        let Some((p, start, l, s)) = found else {
            return false;
        };
        if s == 0 || !(-MAX_STEP..=MAX_STEP).contains(&s) || successors(p) != [h] {
            return false;
        }
        // The loop: `h` and every block that reaches `l` without `h`.
        let mut body = vec![h, l];
        let mut work = vec![l];
        while let Some(x) = work.pop() {
            if x == h {
                continue;
            }
            for &q in preds.get(&x).map(Vec::as_slice).unwrap_or(&[]) {
                if !body.contains(&q) {
                    body.push(q);
                    work.push(q);
                }
            }
        }
        if body.contains(&p) || !body.contains(&LLVMGetInstructionParent(call)) {
            return false;
        }
        let inside = |v: LLVMValueRef| {
            !LLVMIsAInstruction(v).is_null() && body.contains(&LLVMGetInstructionParent(v))
        };
        if [n, a, c, m].into_iter().any(inside) {
            return false;
        }
        let cx = LLVMGetTypeContext(LLVMTypeOf(i));
        let i64t = LLVMInt64TypeInContext(cx);
        let k64 = |v: u64| LLVMConstInt(i64t, v, 0);
        // In `p`: the first trip's index and the step's distance.
        LLVMPositionBuilderBefore(bld, LLVMGetBasicBlockTerminator(p));
        // No division by zero for an empty table, whose loop never walks.
        let zero = LLVMBuildICmp(bld, LLVMIntPredicate::LLVMIntEQ, n, k64(0), c"".as_ptr());
        let nz = LLVMBuildSelect(bld, zero, k64(1), n, c"".as_ptr());
        // The first trip's `iv + k`, reduced mod `n` as an integer (it may
        // start below 0 or past `n`, where the call would not have been in
        // range but a later trip's is): `a * i + c` is `a * (i mod n) + c`
        // mod `n`.
        let first = LLVMBuildAdd(bld, start, LLVMConstInt(i64t, k as u64, 1), c"".as_ptr());
        let r = LLVMBuildSRem(bld, first, nz, c"".as_ptr());
        let neg = LLVMBuildICmp(bld, LLVMIntPredicate::LLVMIntSLT, r, k64(0), c"".as_ptr());
        let up = LLVMBuildAdd(bld, r, n, c"".as_ptr());
        let first = LLVMBuildSelect(bld, neg, up, r, c"".as_ptr());
        let j0 = index(bld, first, n, a, c, m);
        let sa = LLVMBuildMul(bld, a, k64(s.unsigned_abs()), c"".as_ptr());
        let d = LLVMBuildURem(bld, sa, nz, c"".as_ptr());
        let d = if s < 0 {
            let none = LLVMBuildICmp(bld, LLVMIntPredicate::LLVMIntEQ, d, k64(0), c"".as_ptr());
            let back = LLVMBuildSub(bld, n, d, c"".as_ptr());
            LLVMBuildSelect(bld, none, k64(0), back, c"".as_ptr())
        } else {
            d
        };
        // In `h`: the phi.
        LLVMPositionBuilderBefore(bld, LLVMGetFirstInstruction(h));
        let j = LLVMBuildPhi(bld, i64t, c"walk.j".as_ptr());
        // At the latch: the next trip's.
        LLVMPositionBuilderBefore(bld, LLVMGetBasicBlockTerminator(l));
        let t = LLVMBuildAdd(bld, j, d, c"".as_ptr());
        let next = wrap(bld, t, n);
        let mut vals = [j0, next];
        let mut from = [p, l];
        LLVMAddIncoming(j, vals.as_mut_ptr(), from.as_mut_ptr(), 2);
        LLVMReplaceAllUsesWith(call, j);
        true
    }
}

/// How many calls of `name` `f` makes, for the tests.
#[cfg(test)]
unsafe fn count_calls(f: LLVMValueRef, name: &str) -> usize {
    let mut n = 0;
    unsafe {
        let mut b = LLVMGetFirstBasicBlock(f);
        while !b.is_null() {
            for i in super::carry_store::instructions(b) {
                if !LLVMIsACallInst(i).is_null() {
                    let callee = LLVMGetCalledValue(i);
                    let mut len = 0;
                    let s = LLVMGetValueName2(callee, &mut len);
                    if std::slice::from_raw_parts(s.cast::<u8>(), len) == name.as_bytes() {
                        n += 1;
                    }
                }
            }
            b = LLVMGetNextBasicBlock(b);
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use inkwell::context::Context;
    use inkwell::memory_buffer::MemoryBuffer;
    use inkwell::values::AsValueRef;

    const MARKER_DEF: &str = r#"
define internal i64 @karac.walk.index(i64 %i, i64 %n, i64 %a, i64 %c, i64 %m) {
  ret i64 0
}
"#;

    fn run(ir: &str) -> (usize, String) {
        let cx = Context::create();
        let text = format!("{MARKER_DEF}{ir}\0");
        let buf = MemoryBuffer::create_from_memory_range_copy(text.as_bytes(), "t");
        let m = cx.create_module_from_ir(buf).expect("parse");
        // SAFETY: `m` is live.
        let n = unsafe { super::carry_walks(m.as_mut_ptr()) };
        m.verify()
            .unwrap_or_else(|e| panic!("{e}\n{}", m.print_to_string()));
        let f = m.get_function("f").expect("f");
        // SAFETY: `f` is live.
        let left = unsafe { super::count_calls(f.as_value_ref(), super::MARKER) };
        assert_eq!(left, 0, "{}", m.print_to_string());
        (n, m.print_to_string().to_string())
    }

    /// A walk over `0..len`, its words from outside the loop: the index is
    /// carried; a step that only probes stays expanded.
    #[test]
    fn mir_walk_carry_carries_a_loops_index() {
        let (n, text) = run(r#"
declare void @use(i64)
define void @f(i64 %n, i64 %a, i64 %c, i64 %m) {
entry:
  br label %h
h:
  %i = phi i64 [ 0, %entry ], [ %i1, %h ]
  %j = call i64 @karac.walk.index(i64 %i, i64 %n, i64 %a, i64 %c, i64 %m)
  call void @use(i64 %j)
  %i1 = add i64 %i, 1
  %more = icmp ult i64 %i1, %n
  br i1 %more, label %h, label %exit
exit:
  %last = call i64 @karac.walk.index(i64 %i1, i64 %n, i64 %a, i64 %c, i64 %m)
  call void @use(i64 %last)
  ret void
}
"#);
        assert_eq!(n, 1, "{text}");
        assert!(text.contains("%walk.j = phi i64"), "{text}");
        assert!(!text.contains("@karac.walk.index"), "{text}");
    }

    /// Words made inside the loop, or an index that is not the loop's
    /// counter, are expanded in place.
    #[test]
    fn mir_walk_carry_expands_what_it_cannot_carry() {
        let (n, text) = run(r#"
declare i64 @words(i64)
declare void @use(i64)
define void @f(i64 %n, i64 %a, i64 %c, i64 %m, i64 %x) {
entry:
  br label %h
h:
  %i = phi i64 [ 0, %entry ], [ %i1, %h ]
  %w = call i64 @words(i64 %i)
  %j = call i64 @karac.walk.index(i64 %i, i64 %n, i64 %w, i64 %c, i64 %m)
  call void @use(i64 %j)
  %sq = mul i64 %i, %x
  %k = call i64 @karac.walk.index(i64 %sq, i64 %n, i64 %a, i64 %c, i64 %m)
  call void @use(i64 %k)
  %i1 = add i64 %i, 1
  %more = icmp ult i64 %i1, %n
  br i1 %more, label %h, label %exit
exit:
  ret void
}
"#);
        assert_eq!(n, 0, "{text}");
        assert!(!text.contains("walk.j"), "{text}");
    }
}
