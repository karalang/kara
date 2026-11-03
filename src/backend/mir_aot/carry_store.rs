//! Carries the value a loop stores into the next trip's load of it
//! (B-2026-10-31-38).
//!
//! `dp[c] = cost + min(dp[c], dp[c - 1])` reads, at trip `c`, the word trip
//! `c - 1` just stored. LLVM's LoopLoadElimination forwards such a store
//! through a phi, so the value stays in a register; but in LLVM 18 it needs
//! LoopAccessAnalysis, which gives up on a loop with more than one exiting
//! block, and every checked add or index check is one more exit (its branch
//! to the panic). So in a MIR loop the load stays, and each trip waits on
//! store-to-load forwarding from the trip before: kata 64's DP ran 1.77x
//! legacy's cycles on the M5 at the same instruction count, and the same
//! loop in C with an overflow check takes 2.3x the time of the forwarded
//! one on x86.
//!
//! So, after the IR passes, this does the forwarding itself, in a loop
//! simple enough that it needs no dependence analysis:
//!
//! - the loop is a chain of blocks `h -> b1 -> ... -> l -> h`: every block
//!   but `h` has one predecessor, the one before it, and one successor in
//!   the loop, so every trip that reaches the back edge ran all of them in
//!   order; any other successor leaves the loop. `h`'s other predecessor
//!   `p` branches only to `h`;
//! - one instruction in the loop writes memory: a plain store `*S = v`. A
//!   call is allowed only to an intrinsic that neither reads nor writes
//!   memory (`llvm.sadd.with.overflow`, `llvm.smin`, ...);
//! - `h` has a phi `i` that the back edge steps by a constant `K`, and in
//!   bytes `S = R + a*i + cs` and a plain load `L = R + a*i + cl` in `h`, of
//!   `v`'s type, with the same root pointer `R` defined outside the loop,
//!   `a != 0`, and `cs = cl + a*K`.
//!
//! Then the load at trip `t + 1` reads the address the store wrote at trip
//! `t`. Nothing else in the loop writes memory, and the store of trip
//! `t + 1` writes `R + a*i + cs`, `a*K` bytes away, so the word still holds
//! `v`: the load becomes `phi [*(R + a*i0 + cl), p], [v, l]`. The first
//! trip's load moves to the end of `p`, which branches only to `h`. That
//! reads the word even when the first trip would have left the loop (a
//! panic) before its load, so it is done only when no exit comes before the
//! load, or when `p` or the blocks just above it already read or wrote that
//! word with nothing since that could free it (`dp[0] = ...` before a row
//! loop from `c = 1`). `KARAC_MIR_CARRY_STORE=0` turns it off; `=report` names
//! each function it changed, on stderr.

use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::target::{LLVMABISizeOfType, LLVMGetModuleDataLayout, LLVMTargetDataRef};
use llvm_sys::{LLVMAtomicOrdering, LLVMOpcode};

/// Each block's distinct predecessors.
type Preds = rustc_hash::FxHashMap<LLVMBasicBlockRef, Vec<LLVMBasicBlockRef>>;

/// How many blocks above the preheader are searched for an access that
/// shows the first trip's word is there to read.
const ABOVE: usize = 8;

/// The most blocks a loop's chain may have.
const MAX_BLOCKS: usize = 64;

/// How deep an index's arithmetic is followed.
const DEPTH: usize = 8;

/// Intrinsics that neither read nor write memory, by name prefix.
const PURE: &[&str] = &[
    "llvm.sadd.with.overflow.",
    "llvm.ssub.with.overflow.",
    "llvm.smul.with.overflow.",
    "llvm.uadd.with.overflow.",
    "llvm.usub.with.overflow.",
    "llvm.umul.with.overflow.",
    "llvm.sadd.sat.",
    "llvm.ssub.sat.",
    "llvm.uadd.sat.",
    "llvm.usub.sat.",
    "llvm.smin.",
    "llvm.smax.",
    "llvm.umin.",
    "llvm.umax.",
    "llvm.abs.",
    "llvm.ctpop.",
    "llvm.ctlz.",
    "llvm.cttz.",
    "llvm.fshl.",
    "llvm.fshr.",
    "llvm.bswap.",
    "llvm.bitreverse.",
    "llvm.expect.",
    "llvm.fabs.",
    "llvm.sqrt.",
    "llvm.fma.",
    "llvm.fmuladd.",
    "llvm.minnum.",
    "llvm.maxnum.",
    "llvm.copysign.",
    "llvm.floor.",
    "llvm.ceil.",
    "llvm.trunc.",
    "llvm.round.",
];

/// Forwards each store in `m` that a loop's next trip loads back, and
/// returns how many loads it replaced.
///
/// # Safety
/// `m` is a valid module.
pub(super) unsafe fn carry_stores(m: LLVMModuleRef) -> usize {
    let flag = std::env::var("KARAC_MIR_CARRY_STORE");
    if flag.as_deref() == Ok("0") {
        return 0;
    }
    let report = flag.as_deref() == Ok("report");
    if report {
        dump(m);
    }
    let mut n = 0;
    unsafe {
        let td = LLVMGetModuleDataLayout(m);
        let b = LLVMCreateBuilderInContext(LLVMGetModuleContext(m));
        let mut f = LLVMGetFirstFunction(m);
        while !f.is_null() {
            if LLVMCountBasicBlocks(f) > 0 {
                let k = function(f, td, b);
                if k > 0 && report {
                    let mut len = 0;
                    let name = LLVMGetValueName2(f, &mut len);
                    let name = std::slice::from_raw_parts(name.cast::<u8>(), len);
                    eprintln!("carry-store: {} in {}", k, String::from_utf8_lossy(name));
                }
                n += k;
            }
            f = LLVMGetNextFunction(f);
        }
        LLVMDisposeBuilder(b);
    }
    n
}

/// With `KARAC_MIR_CARRY_STORE=report`, `KARAC_MIR_OPT_LL=<path>` keeps
/// each module as the passes left it, at `<path>.<k>`.
unsafe fn dump(m: LLVMModuleRef) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let Ok(path) = std::env::var("KARAC_MIR_OPT_LL") else {
        return;
    };
    let k = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        let text = LLVMPrintModuleToString(m);
        let ir = std::ffi::CStr::from_ptr(text)
            .to_string_lossy()
            .into_owned();
        LLVMDisposeMessage(text);
        let _ = std::fs::write(format!("{path}.{k}"), ir);
    }
}

fn opcode(v: LLVMValueRef) -> Option<LLVMOpcode> {
    unsafe { (!LLVMIsAInstruction(v).is_null()).then(|| LLVMGetInstructionOpcode(v)) }
}

unsafe fn successors(b: LLVMBasicBlockRef) -> Vec<LLVMBasicBlockRef> {
    unsafe {
        let t = LLVMGetBasicBlockTerminator(b);
        if t.is_null() {
            return Vec::new();
        }
        (0..LLVMGetNumSuccessors(t))
            .map(|k| LLVMGetSuccessor(t, k))
            .collect()
    }
}

unsafe fn instructions(b: LLVMBasicBlockRef) -> Vec<LLVMValueRef> {
    let mut out = Vec::new();
    unsafe {
        let mut i = LLVMGetFirstInstruction(b);
        while !i.is_null() {
            out.push(i);
            i = LLVMGetNextInstruction(i);
        }
    }
    out
}

unsafe fn function(f: LLVMValueRef, td: LLVMTargetDataRef, bld: LLVMBuilderRef) -> usize {
    unsafe {
        let mut blocks = Vec::new();
        let mut b = LLVMGetFirstBasicBlock(f);
        while !b.is_null() {
            blocks.push(b);
            b = LLVMGetNextBasicBlock(b);
        }
        // Distinct predecessors, by edge source.
        let mut preds: Preds = Default::default();
        for &b in &blocks {
            for s in successors(b) {
                let e = preds.entry(s).or_default();
                if !e.contains(&b) {
                    e.push(b);
                }
            }
        }
        let mut n = 0;
        for &h in &blocks {
            let Some(ps) = preds.get(&h) else { continue };
            let [x, y] = ps[..] else { continue };
            for (p, l) in [(x, y), (y, x)] {
                if let Some(chain) = chain(h, l, &preds) {
                    n += carry(h, p, l, &chain, &preds, td, bld);
                    break;
                }
            }
        }
        n
    }
}

/// The loop `h -> ... -> l -> h`, when it is a chain: from `h`, each block
/// has one successor that leads on to `l` without passing `h`, and each
/// block after `h` has one predecessor, the block before it.
unsafe fn chain(
    h: LLVMBasicBlockRef,
    l: LLVMBasicBlockRef,
    preds: &Preds,
) -> Option<Vec<LLVMBasicBlockRef>> {
    unsafe {
        if !successors(l).contains(&h) {
            return None;
        }
        // Walk back from `l` while each block has one predecessor.
        let mut rev = vec![l];
        let mut cur = l;
        while cur != h {
            let [p] = preds.get(&cur)?[..] else {
                return None;
            };
            if rev.contains(&p) || rev.len() >= MAX_BLOCKS {
                return None;
            }
            rev.push(p);
            cur = p;
        }
        rev.reverse();
        // Each block's successors in the loop are exactly the next block.
        for (k, &b) in rev.iter().enumerate() {
            let next = rev.get(k + 1).copied().unwrap_or(h);
            if successors(b).iter().any(|s| rev.contains(s) && *s != next) {
                return None;
            }
        }
        Some(rev)
    }
}

/// `p` as `root + a*iv + c` bytes, `a` and `c` wrapping.
unsafe fn address(
    p: LLVMValueRef,
    iv: LLVMValueRef,
    inside: &dyn Fn(LLVMValueRef) -> bool,
    td: LLVMTargetDataRef,
) -> Option<(LLVMValueRef, i64, i64)> {
    unsafe {
        if opcode(p) == Some(LLVMOpcode::LLVMGetElementPtr) && LLVMGetNumOperands(p) == 2 {
            let idx = LLVMGetOperand(p, 1);
            let size = LLVMABISizeOfType(td, LLVMGetGEPSourceElementType(p)) as i64;
            let found = linear(idx, iv, DEPTH).and_then(|(ai, ci)| {
                let (r, a, c) = address(LLVMGetOperand(p, 0), iv, inside, td)?;
                Some((
                    r,
                    a.wrapping_add(ai.wrapping_mul(size)),
                    c.wrapping_add(ci.wrapping_mul(size)),
                ))
            });
            if found.is_some() {
                return found;
            }
        }
        (!inside(p)).then_some((p, 0, 0))
    }
}

/// An `i64` `v` as `a*iv + c`, wrapping.
unsafe fn linear(v: LLVMValueRef, iv: LLVMValueRef, depth: usize) -> Option<(i64, i64)> {
    use LLVMOpcode::*;
    unsafe {
        let t = LLVMTypeOf(v);
        if LLVMGetTypeKind(t) != llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind
            || LLVMGetIntTypeWidth(t) != 64
        {
            return None;
        }
        if v == iv {
            return Some((1, 0));
        }
        if !LLVMIsAConstantInt(v).is_null() {
            return Some((0, LLVMConstIntGetSExtValue(v)));
        }
        if depth == 0 {
            return None;
        }
        let op = |k| LLVMGetOperand(v, k);
        let konst = |x: LLVMValueRef| {
            (!LLVMIsAConstantInt(x).is_null()).then(|| LLVMConstIntGetSExtValue(x))
        };
        let scale = |(a, c): (i64, i64), s: i64| (a.wrapping_mul(s), c.wrapping_mul(s));
        match opcode(v)? {
            LLVMAdd => {
                let (a, c) = linear(op(0), iv, depth - 1)?;
                let (b, d) = linear(op(1), iv, depth - 1)?;
                Some((a.wrapping_add(b), c.wrapping_add(d)))
            }
            LLVMSub => {
                let (a, c) = linear(op(0), iv, depth - 1)?;
                let (b, d) = linear(op(1), iv, depth - 1)?;
                Some((a.wrapping_sub(b), c.wrapping_sub(d)))
            }
            LLVMMul => match (konst(op(0)), konst(op(1))) {
                (_, Some(s)) => Some(scale(linear(op(0), iv, depth - 1)?, s)),
                (Some(s), _) => Some(scale(linear(op(1), iv, depth - 1)?, s)),
                _ => None,
            },
            LLVMShl => match konst(op(1)) {
                Some(s @ 0..=62) => Some(scale(linear(op(0), iv, depth - 1)?, 1i64 << s)),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Does `i` possibly write memory, or leave the block other than by its
/// terminator?
unsafe fn writes(i: LLVMValueRef) -> bool {
    use LLVMOpcode::*;
    unsafe {
        match opcode(i) {
            Some(LLVMStore | LLVMAtomicRMW | LLVMAtomicCmpXchg | LLVMFence | LLVMVAArg) => true,
            Some(LLVMInvoke | LLVMCallBr) => true,
            Some(LLVMCall) => {
                let callee = LLVMGetCalledValue(i);
                if LLVMIsAFunction(callee).is_null() {
                    return true;
                }
                let mut len = 0;
                let name = LLVMGetValueName2(callee, &mut len);
                let name = std::slice::from_raw_parts(name.cast::<u8>(), len);
                !PURE.iter().any(|p| name.starts_with(p.as_bytes()))
            }
            _ => false,
        }
    }
}

unsafe fn plain(i: LLVMValueRef) -> bool {
    unsafe {
        LLVMGetVolatile(i) == 0
            && LLVMGetOrdering(i) == LLVMAtomicOrdering::LLVMAtomicOrderingNotAtomic
    }
}

/// Does `p`, or a block above it with one predecessor each, access the
/// word at `root + a*i0 + c` as a `ty`, with nothing between that access and
/// the end of `p` that could free it (no write but plain stores)?
unsafe fn accessed_before(
    p: LLVMBasicBlockRef,
    preds: &Preds,
    ty: LLVMTypeRef,
    root: LLVMValueRef,
    (a, c): (i64, i64),
    i0: LLVMValueRef,
    td: LLVMTargetDataRef,
) -> bool {
    use LLVMOpcode::*;
    unsafe {
        let at = |x: LLVMValueRef| {
            let Some((r, a1, c1)) = address(x, i0, &|_| false, td) else {
                return false;
            };
            if r != root {
                return false;
            }
            if LLVMIsAConstantInt(i0).is_null() {
                return (a1, c1) == (a, c);
            }
            let k = LLVMConstIntGetSExtValue(i0);
            a1.wrapping_mul(k).wrapping_add(c1) == a.wrapping_mul(k).wrapping_add(c)
        };
        let mut b = p;
        for _ in 0..=ABOVE {
            for i in instructions(b).into_iter().rev() {
                let ptr = match opcode(i) {
                    Some(LLVMLoad) => LLVMGetOperand(i, 0),
                    Some(LLVMStore) => LLVMGetOperand(i, 1),
                    _ if writes(i) => return false,
                    _ => continue,
                };
                let vt = if opcode(i) == Some(LLVMLoad) {
                    LLVMTypeOf(i)
                } else {
                    LLVMTypeOf(LLVMGetOperand(i, 0))
                };
                if plain(i) && vt == ty && at(ptr) {
                    return true;
                }
            }
            let Some([up]) = preds.get(&b).map(|v| &v[..]) else {
                return false;
            };
            b = *up;
        }
        false
    }
}

unsafe fn carry(
    h: LLVMBasicBlockRef,
    p: LLVMBasicBlockRef,
    l: LLVMBasicBlockRef,
    chain: &[LLVMBasicBlockRef],
    preds: &Preds,
    td: LLVMTargetDataRef,
    bld: LLVMBuilderRef,
) -> usize {
    use LLVMOpcode::*;
    unsafe {
        if successors(p) != [h] {
            return 0;
        }
        // The one write in the loop.
        let mut store = None;
        for &b in chain {
            for i in instructions(b) {
                if writes(i) {
                    if opcode(i) != Some(LLVMStore) || store.is_some() || !plain(i) {
                        return 0;
                    }
                    store = Some(i);
                }
            }
        }
        let Some(st) = store else { return 0 };
        let v = LLVMGetOperand(st, 0);
        let ty = LLVMTypeOf(v);
        let inside = |x: LLVMValueRef| {
            !LLVMIsAInstruction(x).is_null() && chain.contains(&LLVMGetInstructionParent(x))
        };
        for iv in instructions(h) {
            if opcode(iv) != Some(LLVMPHI) {
                break;
            }
            if LLVMCountIncoming(iv) != 2 {
                continue;
            }
            let (mut from_p, mut from_l) = (None, None);
            for k in 0..2 {
                let src = LLVMGetIncomingBlock(iv, k);
                let val = LLVMGetIncomingValue(iv, k);
                if src == p {
                    from_p = Some(val);
                } else if src == l {
                    from_l = Some(val);
                }
            }
            let (Some(i0), Some(next)) = (from_p, from_l) else {
                continue;
            };
            // The back edge steps `iv` by a constant.
            let Some((1, step)) = linear(next, iv, 1).filter(|_| opcode(next) == Some(LLVMAdd))
            else {
                continue;
            };
            let Some((root, a, cs)) = address(LLVMGetOperand(st, 1), iv, &inside, td) else {
                continue;
            };
            if a == 0 {
                continue;
            }
            let cl = cs.wrapping_sub(a.wrapping_mul(step));
            // The loads of the word, in the first block that has one. The
            // first trip reads it where the loop is entered, which is safe
            // when the original read it on every first trip (no exit comes
            // before it), or when the code just before the loop accessed it.
            let mut loads = Vec::new();
            let mut exited = false;
            for &b in chain {
                loads = instructions(b)
                    .into_iter()
                    .filter(|&ld| {
                        opcode(ld) == Some(LLVMLoad)
                            && plain(ld)
                            && LLVMTypeOf(ld) == ty
                            && address(LLVMGetOperand(ld, 0), iv, &inside, td)
                                == Some((root, a, cl))
                    })
                    .collect();
                if !loads.is_empty() {
                    break;
                }
                exited |= successors(b).iter().any(|s| !chain.contains(s));
            }
            if loads.is_empty() || exited && !accessed_before(p, preds, ty, root, (a, cl), i0, td) {
                continue;
            }
            let cx = LLVMGetTypeContext(ty);
            let i64t = LLVMInt64TypeInContext(cx);
            // The first trip's word, read at the end of `p`.
            LLVMPositionBuilderBefore(bld, LLVMGetBasicBlockTerminator(p));
            let mut off = i0;
            if a != 1 {
                off = LLVMBuildMul(bld, off, LLVMConstInt(i64t, a as u64, 1), c"".as_ptr());
            }
            if cl != 0 {
                off = LLVMBuildAdd(bld, off, LLVMConstInt(i64t, cl as u64, 1), c"".as_ptr());
            }
            let mut idx = [off];
            let at = LLVMBuildGEP2(
                bld,
                LLVMInt8TypeInContext(cx),
                root,
                idx.as_mut_ptr(),
                1,
                c"carried.at".as_ptr(),
            );
            let first = LLVMBuildLoad2(bld, ty, at, c"carried.first".as_ptr());
            LLVMSetAlignment(first, LLVMGetAlignment(loads[0]));
            LLVMPositionBuilderBefore(bld, LLVMGetFirstInstruction(h));
            let phi = LLVMBuildPhi(bld, ty, c"carried".as_ptr());
            let mut vals = [first, v];
            let mut srcs = [p, l];
            LLVMAddIncoming(phi, vals.as_mut_ptr(), srcs.as_mut_ptr(), 2);
            for &ld in &loads {
                LLVMReplaceAllUsesWith(ld, phi);
                LLVMInstructionEraseFromParent(ld);
            }
            return loads.len();
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use inkwell::context::Context;
    use inkwell::memory_buffer::MemoryBuffer;

    fn run(ir: &str) -> (usize, String) {
        let cx = Context::create();
        let buf = MemoryBuffer::create_from_memory_range_copy(format!("{ir}\0").as_bytes(), "t");
        let m = cx.create_module_from_ir(buf).expect("parse");
        // SAFETY: `m` is live.
        let n = unsafe { super::carry_stores(m.as_mut_ptr()) };
        m.verify()
            .unwrap_or_else(|e| panic!("{e}\n{}", m.print_to_string()));
        (n, m.print_to_string().to_string())
    }

    /// Kata 64's DP row after the passes: `dp[c - 1]` is the word the trip
    /// before stored, behind an overflow exit.
    const DP: &str = r#"
declare { i64, i1 } @llvm.sadd.with.overflow.i64(i64, i64)
declare i64 @llvm.smin.i64(i64, i64)
declare void @abort_at() noreturn
define void @f(ptr %buf, i64 %n, i64 %k) {
entry:
  %prev = getelementptr i8, ptr %buf, i64 -8
  br label %pre
pre:
  br label %h
h:
  %c = phi i64 [ 1, %pre ], [ %c1, %l ]
  %off = shl i64 %c, 3
  %at = getelementptr inbounds i8, ptr %buf, i64 %off
  %here = load i64, ptr %at, align 1
  %at1 = getelementptr i8, ptr %prev, i64 %off
  %back = load i64, ptr %at1, align 1
  %m = call i64 @llvm.smin.i64(i64 %here, i64 %back)
  %s = call { i64, i1 } @llvm.sadd.with.overflow.i64(i64 %m, i64 %k)
  %o = extractvalue { i64, i1 } %s, 1
  br i1 %o, label %panic, label %l
l:
  %v = extractvalue { i64, i1 } %s, 0
  store i64 %v, ptr %at, align 1
  %c1 = add nuw nsw i64 %c, 1
  %more = icmp ult i64 %c1, %n
  br i1 %more, label %h, label %exit
panic:
  call void @abort_at()
  unreachable
exit:
  ret void
}
"#;

    #[test]
    fn mir_carry_store_forwards_the_previous_trips_store() {
        let (n, text) = run(DP);
        assert_eq!(n, 1, "{text}");
        assert!(
            text.contains("%carried = phi i64 [ %carried.first, %pre ], [ %v, %l ]"),
            "{text}"
        );
        assert!(!text.contains("%back = load"), "{text}");
        // `dp[c]` itself is the word this trip stores, not the last one's.
        assert!(text.contains("%here = load"), "{text}");
    }

    /// A second write in the loop, or a call, could change the word in
    /// between; a step the addresses do not match leaves it too.
    #[test]
    fn mir_carry_store_leaves_a_loop_that_may_write_elsewhere() {
        let call = DP.replace(
            "  %c1 = add nuw nsw i64 %c, 1\n",
            "  call void @abort_at()\n  %c1 = add nuw nsw i64 %c, 1\n",
        );
        let other = DP.replace(
            "  %c1 = add nuw nsw i64 %c, 1\n",
            "  store i64 0, ptr %prev, align 1\n  %c1 = add nuw nsw i64 %c, 1\n",
        );
        let stride = DP.replace("%c1 = add nuw nsw i64 %c, 1", "%c1 = add nuw nsw i64 %c, 2");
        for ir in [call, other, stride] {
            let (n, text) = run(&ir);
            assert_eq!(n, 0, "{text}");
        }
    }

    /// The first trip's word is read only where the loop is entered: a
    /// preheader that may branch elsewhere is left alone.
    #[test]
    fn mir_carry_store_needs_a_preheader_that_enters_the_loop() {
        let ir = DP.replace(
            "pre:\n  br label %h",
            "pre:\n  %go = icmp sgt i64 %n, 1\n  br i1 %go, label %h, label %exit",
        );
        let (n, text) = run(&ir);
        assert_eq!(n, 0, "{text}");
    }

    /// Behind a checked step that may exit first, the loads move into a
    /// block of their own: the first trip's word is read early only when
    /// the preheader just wrote it (`dp[0] = ...`, the word at `c - 1` for
    /// `c = 1`), and a call in between, which could free it, undoes that.
    #[test]
    fn mir_carry_store_reads_early_only_a_word_just_written() {
        let late = DP
            .replace(
                "  %off = shl i64 %c, 3\n",
                "  %t = call { i64, i1 } @llvm.smul.with.overflow.i64(i64 %c, i64 %k)\n  \
                 %to = extractvalue { i64, i1 } %t, 1\n  br i1 %to, label %panic, label %b\n\
                 b:\n  %off = shl i64 %c, 3\n",
            )
            .replace(
                "declare i64 @llvm.smin",
                "declare { i64, i1 } @llvm.smul.with.overflow.i64(i64, i64)\ndeclare i64 @llvm.smin",
            );
        let (n, text) = run(&late);
        assert_eq!(n, 0, "nothing shows dp[0] is there: {text}");
        let written = late.replace("pre:\n", "pre:\n  store i64 %k, ptr %buf, align 1\n");
        let (n, text) = run(&written);
        assert_eq!(n, 1, "{text}");
        assert!(text.contains("%carried = phi i64"), "{text}");
        let freed = late.replace(
            "pre:\n",
            "pre:\n  store i64 %k, ptr %buf, align 1\n  call void @abort_at()\n",
        );
        let (n, text) = run(&freed);
        assert_eq!(n, 0, "{text}");
    }
}
