//! Gives a checked `x ± 1` back its overflow flag, after the IR passes
//! (B-2026-11-02-3).
//!
//! `body.rs` checks `x + 1` as `x > MAX - 1` and a `nsw` add rather than
//! through `llvm.sadd.with.overflow`, because LLVM's loop and range
//! analyses see through the compare and not through the intrinsic. After
//! the passes the compare is `icmp eq x, MAX` and the add has sunk below
//! the branch to the panic, so a target with flags runs a compare, the
//! branch and the add: `cmp; b.eq; add` on arm64 where the intrinsic is
//! `adds; b.vs`, one instruction more on every checked step. Kata 242's
//! byte loop ran 21 instructions to legacy's 19 for that alone, 1.105x
//! its count on the M5.
//!
//! So, once the analyses are done, this puts the intrinsic back where the
//! shape is exactly that:
//!
//! - a block `B` ends in `br i1 c, T, F`, with `c = icmp eq|ne X, K` in `B`
//!   used only by that branch, `X` not a constant, and `K` the one value
//!   past which the step overflows: signed `MAX` (`+ 1`), signed `MIN`
//!   (`- 1`), or unsigned `MAX` (`+ 1`, `nuw`);
//! - the successor taken when `c` says "no overflow" has `B` as its only
//!   predecessor and holds `a = add nsw|nuw X, ±1` of the matching sign
//!   and flag (a `- 1` is `add X, -1` by then).
//!
//! Then `c` becomes the intrinsic's flag in `B`, and `a`'s uses its value.
//! The add now runs on the panic path too, which computes and drops it.
//! `KARAC_MIR_OVERFLOW_FLAG=0` turns it off; `=report` prints how many it
//! rewrote, on stderr.

use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMIntPredicate, LLVMOpcode};

/// The step a bound guards: the intrinsic's name and the add's constant.
#[derive(Clone, Copy)]
struct Step {
    intrinsic: &'static str,
    signed: bool,
    by: i64,
}

/// Rewrites each checked `x ± 1` in `m` to its overflow intrinsic, and
/// returns how many it rewrote.
///
/// # Safety
/// `m` is a valid module.
pub(super) unsafe fn flag_steps(m: LLVMModuleRef) -> usize {
    let flag = std::env::var("KARAC_MIR_OVERFLOW_FLAG");
    if flag.as_deref() == Ok("0") {
        return 0;
    }
    let mut n = 0;
    unsafe {
        let b = LLVMCreateBuilderInContext(LLVMGetModuleContext(m));
        let mut f = LLVMGetFirstFunction(m);
        while !f.is_null() {
            let mut bb = LLVMGetFirstBasicBlock(f);
            while !bb.is_null() {
                if block(m, b, bb) {
                    n += 1;
                }
                bb = LLVMGetNextBasicBlock(bb);
            }
            f = LLVMGetNextFunction(f);
        }
        LLVMDisposeBuilder(b);
    }
    if flag.as_deref() == Ok("report") {
        eprintln!("overflow-flag: {n}");
    }
    n
}

/// The step `K` bounds for an `X` of `bits` bits, or `None`.
fn step_of(k: u64, bits: u32) -> Option<Step> {
    if !(8..=64).contains(&bits) {
        return None;
    }
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let k = k & mask;
    let smax = mask >> 1;
    let smin = smax + 1;
    if k == smax {
        Some(Step {
            intrinsic: "llvm.sadd.with.overflow",
            signed: true,
            by: 1,
        })
    } else if k == smin {
        Some(Step {
            intrinsic: "llvm.ssub.with.overflow",
            signed: true,
            by: -1,
        })
    } else if k == mask {
        Some(Step {
            intrinsic: "llvm.uadd.with.overflow",
            signed: false,
            by: 1,
        })
    } else {
        None
    }
}

/// How many uses `v` has.
unsafe fn uses(v: LLVMValueRef) -> usize {
    let mut n = 0;
    unsafe {
        let mut u = LLVMGetFirstUse(v);
        while !u.is_null() {
            n += 1;
            u = LLVMGetNextUse(u);
        }
    }
    n
}

/// Whether `bb`'s only predecessor is the block ending in `br`: every use
/// of `bb` is that branch, which names it once.
unsafe fn only_from(bb: LLVMBasicBlockRef, br: LLVMValueRef) -> bool {
    unsafe {
        let v = LLVMBasicBlockAsValue(bb);
        let mut u = LLVMGetFirstUse(v);
        let mut n = 0;
        while !u.is_null() {
            if LLVMGetUser(u) != br {
                return false;
            }
            n += 1;
            u = LLVMGetNextUse(u);
        }
        n == 1
    }
}

/// The add `x + by` with `step`'s flag in `bb`, or null.
unsafe fn find_add(bb: LLVMBasicBlockRef, x: LLVMValueRef, step: Step) -> LLVMValueRef {
    unsafe {
        let mut i = LLVMGetFirstInstruction(bb);
        while !i.is_null() {
            if LLVMGetInstructionOpcode(i) == LLVMOpcode::LLVMAdd {
                let flagged = if step.signed {
                    LLVMGetNSW(i) != 0
                } else {
                    LLVMGetNUW(i) != 0
                };
                let (p, q) = (LLVMGetOperand(i, 0), LLVMGetOperand(i, 1));
                let other = if p == x {
                    q
                } else if q == x {
                    p
                } else {
                    std::ptr::null_mut()
                };
                if flagged
                    && !other.is_null()
                    && !LLVMIsAConstantInt(other).is_null()
                    && LLVMConstIntGetSExtValue(other) == step.by
                {
                    return i;
                }
            }
            i = LLVMGetNextInstruction(i);
        }
        std::ptr::null_mut()
    }
}

/// Rewrites the step `bb`'s branch guards, if it is one.
unsafe fn block(m: LLVMModuleRef, b: LLVMBuilderRef, bb: LLVMBasicBlockRef) -> bool {
    unsafe {
        let br = LLVMGetBasicBlockTerminator(bb);
        if br.is_null()
            || LLVMGetInstructionOpcode(br) != LLVMOpcode::LLVMBr
            || LLVMIsConditional(br) == 0
        {
            return false;
        }
        let c = LLVMGetCondition(br);
        if LLVMIsAICmpInst(c).is_null() || LLVMGetInstructionParent(c) != bb || uses(c) != 1 {
            return false;
        }
        let pred = LLVMGetICmpPredicate(c);
        let goes_on = match pred {
            LLVMIntPredicate::LLVMIntEQ => LLVMGetSuccessor(br, 1),
            LLVMIntPredicate::LLVMIntNE => LLVMGetSuccessor(br, 0),
            _ => return false,
        };
        let (x, k) = (LLVMGetOperand(c, 0), LLVMGetOperand(c, 1));
        if LLVMIsAConstantInt(k).is_null() || !LLVMIsAConstant(x).is_null() {
            return false;
        }
        let ty = LLVMTypeOf(x);
        let Some(step) = step_of(LLVMConstIntGetZExtValue(k), LLVMGetIntTypeWidth(ty)) else {
            return false;
        };
        if LLVMGetSuccessor(br, 0) == LLVMGetSuccessor(br, 1) || !only_from(goes_on, br) {
            return false;
        }
        let add = find_add(goes_on, x, step);
        if add.is_null() {
            return false;
        }
        let name = step.intrinsic;
        let id = LLVMLookupIntrinsicID(name.as_ptr().cast(), name.len());
        if id == 0 {
            return false;
        }
        let mut tys = [ty];
        let f = LLVMGetIntrinsicDeclaration(m, id, tys.as_mut_ptr(), 1);
        let fty = LLVMIntrinsicGetType(LLVMGetModuleContext(m), id, tys.as_mut_ptr(), 1);
        LLVMPositionBuilderBefore(b, c);
        let one = LLVMConstInt(ty, 1, 0);
        let mut args = [x, one];
        let r = LLVMBuildCall2(b, fty, f, args.as_mut_ptr(), 2, c"".as_ptr());
        let v = LLVMBuildExtractValue(b, r, 0, c"".as_ptr());
        let o = LLVMBuildExtractValue(b, r, 1, c"step.ovf".as_ptr());
        let o = if pred == LLVMIntPredicate::LLVMIntNE {
            LLVMBuildNot(b, o, c"".as_ptr())
        } else {
            o
        };
        LLVMReplaceAllUsesWith(add, v);
        LLVMInstructionEraseFromParent(add);
        LLVMSetCondition(br, o);
        LLVMInstructionEraseFromParent(c);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::step_of;

    #[test]
    fn overflow_flag_bounds_name_their_step() {
        let s = step_of(i64::MAX as u64, 64).unwrap();
        assert_eq!((s.intrinsic, s.by), ("llvm.sadd.with.overflow", 1));
        let s = step_of(i64::MIN as u64, 64).unwrap();
        assert_eq!((s.intrinsic, s.by), ("llvm.ssub.with.overflow", -1));
        let s = step_of(u64::MAX, 64).unwrap();
        assert_eq!((s.intrinsic, s.by), ("llvm.uadd.with.overflow", 1));
        let s = step_of(0x7f, 8).unwrap();
        assert_eq!(s.intrinsic, "llvm.sadd.with.overflow");
        let s = step_of(0xff, 8).unwrap();
        assert_eq!(s.intrinsic, "llvm.uadd.with.overflow");
        assert!(step_of(5, 64).is_none());
        assert!(step_of(0, 128).is_none());
    }
}
