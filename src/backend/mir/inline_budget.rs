//! A bound on how large the JIT's inliner can make one function
//! (B-2026-11-10-5).
//!
//! The inliner takes a function with one call site whatever its size: it
//! is the last call, so the body leaves the module once inlined. A program
//! that splits a table across a hundred such functions, all called from one
//! place, gets them folded into that place: mini_tldextract's `data_files`
//! became one function of 183,000 instructions, and code generation is far
//! worse than linear in a function's size. LLVM spent 67 s on it, 40 s of
//! that in the machine scheduler and 18 s in CodeGenPrepare, where the same
//! program with no inlining compiles in about 3 s.
//!
//! [`cap_inline_growth`] estimates each function's size once the
//! single-call functions below it are inlined, and keeps the largest of
//! them out of line (`noinline`) until the estimate fits the budget. What
//! it keeps out is large by construction, so the call it leaves costs
//! nothing next to the body. `KARAC_MIR_JIT_INLINE_BUDGET=<n>` sets the
//! budget in instructions; `0` turns the bound off.

use std::collections::HashMap;
use std::ffi::CStr;

use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMAttributeFunctionIndex, LLVMLinkage};

/// Instructions, counted before `sroa`, that one function may reach through
/// the inlining of its single-call callees. The largest function a program
/// writes by hand in the corpus apps is about a third of this.
const BUDGET: usize = 10_000;

fn budget() -> usize {
    std::env::var("KARAC_MIR_JIT_INLINE_BUDGET")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(BUDGET)
}

unsafe fn attribute_kind(name: &CStr) -> u32 {
    LLVMGetEnumAttributeKindForName(name.as_ptr(), name.to_bytes().len())
}

unsafe fn size(f: LLVMValueRef) -> usize {
    let mut n = 0;
    let mut b = LLVMGetFirstBasicBlock(f);
    while !b.is_null() {
        let mut i = LLVMGetFirstInstruction(b);
        while !i.is_null() {
            n += 1;
            i = LLVMGetNextInstruction(i);
        }
        b = LLVMGetNextBasicBlock(b);
    }
    n
}

/// The function holding the one call of `f`, when `f` is an internal
/// definition whose only use is that direct call: the shape the inliner
/// takes regardless of size.
unsafe fn sole_caller(f: LLVMValueRef) -> Option<LLVMValueRef> {
    if LLVMIsDeclaration(f) != 0 {
        return None;
    }
    if !matches!(
        LLVMGetLinkage(f),
        LLVMLinkage::LLVMInternalLinkage | LLVMLinkage::LLVMPrivateLinkage
    ) {
        return None;
    }
    let only = LLVMGetFirstUse(f);
    if only.is_null() || !LLVMGetNextUse(only).is_null() {
        return None;
    }
    let user = LLVMGetUser(only);
    if LLVMIsACallInst(user).is_null() || LLVMGetCalledValue(user) != f {
        return None;
    }
    Some(LLVMGetBasicBlockParent(LLVMGetInstructionParent(user)))
}

/// Mark `noinline` the single-call functions whose inlining would take a
/// caller past the budget, largest first. Returns how many were marked.
///
/// # Safety
/// `m` must be a live module that nothing else is using.
pub(super) unsafe fn cap_inline_growth(m: LLVMModuleRef) -> usize {
    cap_inline_growth_to(m, budget())
}

unsafe fn cap_inline_growth_to(m: LLVMModuleRef, budget: usize) -> usize {
    if budget == 0 {
        return 0;
    }
    let noinline = attribute_kind(c"noinline");
    let alwaysinline = attribute_kind(c"alwaysinline");
    let fixed = |f: LLVMValueRef, kind: u32| {
        !LLVMGetEnumAttributeAtIndex(f, LLVMAttributeFunctionIndex, kind).is_null()
    };

    let mut defs = Vec::new();
    let mut f = LLVMGetFirstFunction(m);
    while !f.is_null() {
        if LLVMIsDeclaration(f) == 0 {
            defs.push(f);
        }
        f = LLVMGetNextFunction(f);
    }
    let own: HashMap<LLVMValueRef, usize> = defs.iter().map(|&f| (f, size(f))).collect();
    let mut below: HashMap<LLVMValueRef, Vec<LLVMValueRef>> = HashMap::new();
    for &f in &defs {
        if fixed(f, noinline) || fixed(f, alwaysinline) {
            continue;
        }
        if let Some(caller) = sole_caller(f).filter(|&c| c != f) {
            below.entry(caller).or_default().push(f);
        }
    }

    // Each function's size once what sits below it is inlined, callees
    // before callers. A single-call chain cannot loop back on itself
    // except through a self-call, which `sole_caller` leaves out, but the
    // placeholder keeps a malformed module from recursing forever.
    let ctx = LLVMGetModuleContext(m);
    let mut grown: HashMap<LLVMValueRef, usize> = HashMap::new();
    let mut marked = 0;
    let mut stack: Vec<(LLVMValueRef, bool)> = defs.iter().map(|&f| (f, false)).collect();
    while let Some((f, expanded)) = stack.pop() {
        if !expanded {
            if grown.contains_key(&f) {
                continue;
            }
            grown.insert(f, own[&f]);
            stack.push((f, true));
            for &c in below.get(&f).into_iter().flatten() {
                if !grown.contains_key(&c) {
                    stack.push((c, false));
                }
            }
            continue;
        }
        let mut kids: Vec<(usize, LLVMValueRef)> = below
            .get(&f)
            .into_iter()
            .flatten()
            .map(|&c| (grown[&c], c))
            .collect();
        let mut total = own[&f] + kids.iter().map(|k| k.0).sum::<usize>();
        kids.sort_by_key(|k| std::cmp::Reverse(k.0));
        for (size, c) in kids {
            if total <= budget {
                break;
            }
            let attr = LLVMCreateEnumAttribute(ctx, noinline, 0);
            LLVMAddAttributeAtIndex(c, LLVMAttributeFunctionIndex, attr);
            total -= size;
            marked += 1;
        }
        grown.insert(f, total);
    }
    marked
}

#[cfg(test)]
mod tests {
    use inkwell::context::Context;
    use inkwell::memory_buffer::MemoryBuffer;

    /// A function of `n` instructions plus its `ret`, with linkage `link`.
    fn body(name: &str, link: &str, n: usize) -> String {
        let mut s = format!("define {link} i64 @{name}(i64 %x0) {{\n");
        for i in 1..=n {
            s.push_str(&format!("  %x{i} = add i64 %x{}, 1\n", i - 1));
        }
        s.push_str(&format!("  ret i64 %x{n}\n}}\n"));
        s
    }

    fn noinline(ir: &str, budget: usize) -> (usize, Vec<String>) {
        let cx = Context::create();
        // The IR parser reads a NUL-terminated buffer.
        let bytes = [ir.as_bytes(), b"\0"].concat();
        let buf = MemoryBuffer::create_from_memory_range_copy(&bytes, "t");
        let m = cx.create_module_from_ir(buf).expect("test IR parses");
        // SAFETY: the module is live and only this test uses it.
        let kept = unsafe { super::cap_inline_growth_to(m.as_mut_ptr(), budget) };
        let kind = inkwell::attributes::Attribute::get_named_enum_kind_id("noinline");
        let mut out: Vec<String> = m
            .get_functions()
            .filter(|f| {
                f.get_enum_attribute(inkwell::attributes::AttributeLoc::Function, kind)
                    .is_some()
            })
            .map(|f| f.get_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        (kept, out)
    }

    const CALLER: &str = "define i64 @top(i64 %x) {
  %a = call i64 @big(i64 %x)
  %b = call i64 @mid(i64 %a)
  %c = call i64 @small(i64 %b)
  %d = call i64 @shared(i64 %c)
  %e = call i64 @shared(i64 %d)
  ret i64 %e
}
";

    fn program() -> String {
        [
            CALLER.to_string(),
            body("big", "internal", 400),
            body("mid", "internal", 300),
            body("small", "internal", 20),
            body("shared", "internal", 500),
        ]
        .concat()
    }

    #[test]
    fn inline_budget_keeps_the_largest_single_call_callees_out_of_line() {
        // top (6) + big (401) + mid (301) + small (21) = 729: over 500, so
        // `big` goes and leaves 328; under 300 `mid` goes as well.
        assert_eq!(noinline(&program(), 500), (1, vec!["big".to_string()]));
        assert_eq!(
            noinline(&program(), 300),
            (2, vec!["big".to_string(), "mid".to_string()])
        );
        // A budget it fits leaves the module alone; `shared`, called
        // twice, is the inliner's ordinary size judgement and never ours.
        assert_eq!(noinline(&program(), 1_000), (0, vec![]));
        assert_eq!(noinline(&program(), 0), (0, vec![]));
    }

    #[test]
    fn inline_budget_counts_what_a_callee_inlines_itself() {
        // `outer` alone is small, but `inner` inlined into it is not, so
        // `inner` stays out of line and `top` then fits.
        let ir = [
            "define i64 @top(i64 %x) {\n  %a = call i64 @outer(i64 %x)\n  ret i64 %a\n}\n"
                .to_string(),
            "define internal i64 @outer(i64 %x) {\n  %a = call i64 @inner(i64 %x)\n  ret i64 %a\n}\n"
                .to_string(),
            body("inner", "internal", 400),
        ]
        .concat();
        assert_eq!(noinline(&ir, 300), (1, vec!["inner".to_string()]));
        // An exported function can be called from elsewhere, so the inliner
        // keeps its body whatever happens here; it is not counted.
        let exported = ir.replace("define internal i64 @inner", "define i64 @inner");
        assert_eq!(noinline(&exported, 300), (0, vec![]));
    }
}
