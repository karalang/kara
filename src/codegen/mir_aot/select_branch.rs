//! Keeps a conditional step of a loop's index a branch (B-2026-10-22-4).
//!
//! `if going_down { cur = cur + 1 } else { cur = cur - 1 }` optimises to
//! `cur' = cur + select(c, 1, -1)`, which x86 lowers without a branch
//! (`setne`, `lea`). When `cur` indexes memory, every load and store through
//! it then waits on the compare chain that computes `c`, trip after trip,
//! where a branch would have let the predictor run ahead: kata 6's
//! `rows[cur].push(..)` loop ran 3x slower at the same instruction count
//! (565 ms against 212 ms with the select written as a branch).
//!
//! So, after the IR passes, a select of two integer constants that is one
//! step of such a recurrence is given `!prof` branch weights, which is what
//! CodeGenPrepare reads when it decides to emit a select as a branch. The
//! shape, all three required:
//!
//! - `s = select i1 c, K1, K2` with `K1` and `K2` integer constants, and `s`
//!   used once, by `a = add p, s` (either operand order);
//! - `p` a phi that has `a` as one of its incoming values;
//! - `p` reaches a `getelementptr` index through at most [`DEPTH`]
//!   integer casts and arithmetic.
//!
//! The weights only steer instruction selection and block layout; the
//! program computes the same values. `KARAC_MIR_SELECT_BRANCH=0` turns it
//! off.

use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::LLVMOpcode;

/// How many casts and arithmetic steps may lie between the phi and the
/// address it indexes.
const DEPTH: usize = 3;

/// Marks each select in `m` that steps a loop's index.
///
/// # Safety
/// `m` is a valid module.
pub(super) unsafe fn mark_index_steps(m: LLVMModuleRef) {
    if std::env::var("KARAC_MIR_SELECT_BRANCH").as_deref() == Ok("0") {
        return;
    }
    unsafe {
        let cx = LLVMGetModuleContext(m);
        let mut weights = None;
        let mut f = LLVMGetFirstFunction(m);
        while !f.is_null() {
            let mut b = LLVMGetFirstBasicBlock(f);
            while !b.is_null() {
                let mut i = LLVMGetFirstInstruction(b);
                while !i.is_null() {
                    if is_index_step(i) {
                        let (kind, node) = *weights.get_or_insert_with(|| prof(cx));
                        LLVMSetMetadata(i, kind, node);
                    }
                    i = LLVMGetNextInstruction(i);
                }
                b = LLVMGetNextBasicBlock(b);
            }
            f = LLVMGetNextFunction(f);
        }
    }
}

/// `!prof !{"branch_weights", 1000, 1}`, as a metadata kind and value.
unsafe fn prof(cx: LLVMContextRef) -> (u32, LLVMValueRef) {
    unsafe {
        let name = "prof";
        let kind = LLVMGetMDKindIDInContext(cx, name.as_ptr().cast(), name.len() as u32);
        let tag = "branch_weights";
        let i32t = LLVMInt32TypeInContext(cx);
        let mut ops = [
            LLVMMDStringInContext2(cx, tag.as_ptr().cast(), tag.len()),
            LLVMValueAsMetadata(LLVMConstInt(i32t, 1000, 0)),
            LLVMValueAsMetadata(LLVMConstInt(i32t, 1, 0)),
        ];
        let md = LLVMMDNodeInContext2(cx, ops.as_mut_ptr(), ops.len());
        (kind, LLVMMetadataAsValue(cx, md))
    }
}

fn opcode(v: LLVMValueRef) -> Option<LLVMOpcode> {
    unsafe { (!LLVMIsAInstruction(v).is_null()).then(|| LLVMGetInstructionOpcode(v)) }
}

/// The users of `v`.
unsafe fn users(v: LLVMValueRef) -> Vec<LLVMValueRef> {
    let mut out = Vec::new();
    unsafe {
        let mut u = LLVMGetFirstUse(v);
        while !u.is_null() {
            out.push(LLVMGetUser(u));
            u = LLVMGetNextUse(u);
        }
    }
    out
}

unsafe fn is_index_step(s: LLVMValueRef) -> bool {
    unsafe {
        if opcode(s) != Some(LLVMOpcode::LLVMSelect)
            || LLVMIsAConstantInt(LLVMGetOperand(s, 1)).is_null()
            || LLVMIsAConstantInt(LLVMGetOperand(s, 2)).is_null()
            || !LLVMGetMetadata(s, LLVMGetMDKindID("prof".as_ptr().cast(), 4)).is_null()
        {
            return false;
        }
        let [a] = users(s)[..] else { return false };
        if opcode(a) != Some(LLVMOpcode::LLVMAdd) {
            return false;
        }
        let other = if LLVMGetOperand(a, 0) == s {
            LLVMGetOperand(a, 1)
        } else {
            LLVMGetOperand(a, 0)
        };
        if opcode(other) != Some(LLVMOpcode::LLVMPHI) {
            return false;
        }
        let carried = (0..LLVMCountIncoming(other)).any(|k| LLVMGetIncomingValue(other, k) == a);
        carried && indexes(other, DEPTH)
    }
}

/// Does `v` reach a `getelementptr` index through at most `depth` integer
/// casts and arithmetic?
unsafe fn indexes(v: LLVMValueRef, depth: usize) -> bool {
    use LLVMOpcode::*;
    unsafe {
        users(v).into_iter().any(|u| match opcode(u) {
            Some(LLVMGetElementPtr) => {
                (1..LLVMGetNumOperands(u) as u32).any(|k| LLVMGetOperand(u, k) == v)
            }
            Some(
                LLVMZExt | LLVMSExt | LLVMTrunc | LLVMMul | LLVMShl | LLVMAdd | LLVMSub | LLVMOr,
            ) => depth > 0 && indexes(u, depth - 1),
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use inkwell::context::Context;
    use inkwell::memory_buffer::MemoryBuffer;

    /// `cur' = cur + select(c, 1, -1)` indexing `rows` is marked; the same
    /// step that indexes nothing (a count) is not.
    #[test]
    fn mir_select_branch_marks_only_an_index_step() {
        let ir = r#"
define i64 @f(ptr %rows, i1 %c0, i64 %n) {
entry:
  br label %loop
loop:
  %cur = phi i64 [ 0, %entry ], [ %cur1, %loop ]
  %cnt = phi i64 [ 0, %entry ], [ %cnt1, %loop ]
  %i = phi i64 [ 0, %entry ], [ %i1, %loop ]
  %off = mul i64 %cur, 24
  %p = getelementptr i8, ptr %rows, i64 %off
  store i64 %i, ptr %p
  %c = icmp eq i64 %cur, 3
  %d = select i1 %c, i64 -1, i64 1
  %cur1 = add i64 %cur, %d
  %e = select i1 %c0, i64 1, i64 0
  %cnt1 = add i64 %cnt, %e
  %i1 = add i64 %i, 1
  %done = icmp eq i64 %i1, %n
  br i1 %done, label %exit, label %loop
exit:
  ret i64 %cnt1
}
"#;
        let cx = Context::create();
        let buf = MemoryBuffer::create_from_memory_range_copy(format!("{ir}\0").as_bytes(), "t");
        let m = cx.create_module_from_ir(buf).expect("parse");
        // SAFETY: `m` is live.
        unsafe { super::mark_index_steps(m.as_mut_ptr()) };
        let text = m.print_to_string().to_string();
        let line = |name: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(name))
                .unwrap_or_else(|| panic!("no {name} in {text}"))
                .to_string()
        };
        assert!(line("%d = select").contains("!prof"), "{text}");
        assert!(!line("%e = select").contains("!prof"), "{text}");
    }
}
