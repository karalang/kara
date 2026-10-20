//! Parallel code generation for a large program (M2 compile time).
//!
//! LLVM optimises and emits a module on one thread, and a program whose IR
//! is megabytes spends seconds there. [`plan`] cuts the module into units,
//! [`compile_units`] optimises and emits each unit on its own thread, and the
//! objects are merged by a partial link (`cc -r`), so the caller still links
//! one object.
//!
//! Each unit is a copy of the whole module that keeps some of the bodies:
//!
//! - an internal function that is small or `alwaysinline` (the `.ptr`
//!   wrappers), and an internal constant, is copied into every unit that
//!   reaches it, so inlining across the cut is what it was;
//! - every other definition is owned by one unit. A function called from one
//!   place goes with its caller, and the groups are balanced by size;
//! - a definition another unit reaches becomes a hidden external symbol, so
//!   the internal functions nothing else reaches keep the interprocedural
//!   passes (argument promotion) that need internal linkage.
//!
//! `KARAC_MIR_CODEGEN_UNITS=<n>` sets the number of units; `1` turns the
//! split off.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMLinkage, LLVMTypeKind, LLVMVisibility};

/// Instructions below which a module is one unit, and per further unit.
const MIN_UNIT_INSTS: usize = 40_000;
/// An internal function at most this many instructions long is copied.
const COPY_MAX_INSTS: usize = 40;
/// At most this many units, whatever the machine.
const MAX_UNITS: usize = 8;

/// How a module is cut: which definitions each unit keeps.
pub(super) struct Plan {
    /// For each unit, a flag per definition (in [`definitions`] order).
    keep: Vec<Vec<bool>>,
    /// The definitions' names, to check a copy lines up with the plan.
    names: Vec<String>,
}

impl Plan {
    pub(super) fn units(&self) -> usize {
        self.keep.len()
    }
}

/// The number of units the environment asks for, if it says.
fn requested_units() -> Option<usize> {
    std::env::var("KARAC_MIR_CODEGEN_UNITS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|n| n.max(1))
}

/// Every function and global the module defines, functions first, in module
/// order (which a bitcode round trip keeps).
unsafe fn definitions(m: LLVMModuleRef) -> Vec<LLVMValueRef> {
    let mut out = Vec::new();
    let mut f = LLVMGetFirstFunction(m);
    while !f.is_null() {
        if LLVMIsDeclaration(f) == 0 {
            out.push(f);
        }
        f = LLVMGetNextFunction(f);
    }
    let mut g = LLVMGetFirstGlobal(m);
    while !g.is_null() {
        if LLVMIsDeclaration(g) == 0 {
            out.push(g);
        }
        g = LLVMGetNextGlobal(g);
    }
    out
}

unsafe fn name_of(v: LLVMValueRef) -> String {
    let mut len = 0usize;
    let p = LLVMGetValueName2(v, &mut len);
    if p.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(std::slice::from_raw_parts(p as *const u8, len)).into_owned()
}

unsafe fn is_internal(v: LLVMValueRef) -> bool {
    matches!(
        LLVMGetLinkage(v),
        LLVMLinkage::LLVMInternalLinkage | LLVMLinkage::LLVMPrivateLinkage
    )
}

unsafe fn has_fn_attr(f: LLVMValueRef, name: &str) -> bool {
    let kind = LLVMGetEnumAttributeKindForName(name.as_ptr() as *const _, name.len());
    kind != 0
        && !LLVMGetEnumAttributeAtIndex(f, llvm_sys::LLVMAttributeFunctionIndex, kind).is_null()
}

/// The definitions `v` names, through constant expressions.
unsafe fn collect_refs(
    v: LLVMValueRef,
    index: &HashMap<LLVMValueRef, usize>,
    seen: &mut HashSet<LLVMValueRef>,
    out: &mut Vec<usize>,
) {
    if !LLVMIsAGlobalValue(v).is_null() {
        if let Some(&i) = index.get(&v) {
            out.push(i);
        }
        return;
    }
    if LLVMIsAConstant(v).is_null() || !seen.insert(v) {
        return;
    }
    for k in 0..LLVMGetNumOperands(v) {
        collect_refs(LLVMGetOperand(v, k as u32), index, seen, out);
    }
}

/// Cut `m` into units, or `None` when it is better compiled whole. Names any
/// definition a unit shares and promotes it to a hidden external symbol, so
/// it is called after the module is complete and before it is copied.
pub(super) fn plan(m: LLVMModuleRef) -> Option<Plan> {
    let requested = requested_units();
    if requested == Some(1)
        || !cfg!(unix)
        || crate::target::active_target() != "native"
        || std::env::var("KARAC_DEBUG_INFO").as_deref() == Ok("1")
    {
        return None;
    }
    unsafe {
        if !LLVMGetFirstGlobalAlias(m).is_null() || !LLVMGetFirstGlobalIFunc(m).is_null() {
            return None;
        }
        let defs = definitions(m);
        let index: HashMap<LLVMValueRef, usize> =
            defs.iter().enumerate().map(|(i, &v)| (v, i)).collect();
        let n = defs.len();
        let mut is_fn = vec![false; n];
        let mut size = vec![0usize; n];
        let mut refs: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, &v) in defs.iter().enumerate() {
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            if !LLVMIsAFunction(v).is_null() {
                is_fn[i] = true;
                let mut bb = LLVMGetFirstBasicBlock(v);
                while !bb.is_null() {
                    let mut ins = LLVMGetFirstInstruction(bb);
                    while !ins.is_null() {
                        size[i] += 1;
                        for k in 0..LLVMGetNumOperands(ins) {
                            collect_refs(
                                LLVMGetOperand(ins, k as u32),
                                &index,
                                &mut seen,
                                &mut out,
                            );
                        }
                        ins = LLVMGetNextInstruction(ins);
                    }
                    bb = LLVMGetNextBasicBlock(bb);
                }
            } else {
                let init = LLVMGetInitializer(v);
                if !init.is_null() {
                    collect_refs(init, &index, &mut seen, &mut out);
                }
            }
            out.sort_unstable();
            out.dedup();
            out.retain(|&j| j != i);
            refs[i] = out;
        }
        let total: usize = size.iter().sum();
        let threads = std::thread::available_parallelism().map_or(1, |p| p.get());
        let units = requested
            .unwrap_or_else(|| (total / MIN_UNIT_INSTS).min(threads))
            .min(MAX_UNITS);
        if std::env::var("KARAC_MIR_TIME").as_deref() == Ok("1") {
            eprintln!("[mir-time] codegen units: {units} for {total} instructions");
        }
        if units <= 1 {
            return None;
        }

        // Copied into each unit that reaches it, or owned by one.
        let copied: Vec<bool> = (0..n)
            .map(|i| {
                let v = defs[i];
                if !is_internal(v) {
                    false
                } else if is_fn[i] {
                    !has_fn_attr(v, "noinline")
                        && (size[i] <= COPY_MAX_INSTS || has_fn_attr(v, "alwaysinline"))
                } else {
                    LLVMIsGlobalConstant(v) != 0
                }
            })
            .collect();

        // What each owned function reaches through copies: the definitions
        // it calls once the copies are inlined.
        let owned_fns: Vec<usize> = (0..n).filter(|&i| is_fn[i] && !copied[i]).collect();
        let reach = |roots: &[usize]| -> (Vec<usize>, Vec<usize>) {
            // (copies reached, owned definitions reached)
            let mut seen = vec![false; n];
            let mut stack: Vec<usize> = roots.to_vec();
            let (mut copies, mut owned) = (Vec::new(), Vec::new());
            for &r in roots {
                seen[r] = true;
            }
            while let Some(i) = stack.pop() {
                for &j in &refs[i] {
                    if seen[j] {
                        continue;
                    }
                    seen[j] = true;
                    if copied[j] {
                        copies.push(j);
                        stack.push(j);
                    } else {
                        owned.push(j);
                    }
                }
            }
            (copies, owned)
        };

        // Group a function called from one place with its caller, smallest
        // first, while the group stays under a share of a unit.
        let mut callers: Vec<Vec<usize>> = vec![Vec::new(); n];
        for &f in &owned_fns {
            for g in reach(&[f]).1 {
                if is_fn[g] {
                    callers[g].push(f);
                }
            }
        }
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut group_size = size.clone();
        let cap = total / units / 2;
        let mut by_size = owned_fns.clone();
        by_size.sort_by_key(|&i| (size[i], i));
        for &g in &by_size {
            if callers[g].len() != 1 || !is_internal(defs[g]) {
                continue;
            }
            let (a, b) = (find(&mut parent, g), find(&mut parent, callers[g][0]));
            if a != b && group_size[a] + group_size[b] <= cap {
                parent[a] = b;
                group_size[b] += group_size[a];
            }
        }
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for &f in &owned_fns {
            let r = find(&mut parent, f);
            groups.entry(r).or_default().push(f);
        }
        let mut groups: Vec<(usize, Vec<usize>)> = groups
            .into_iter()
            .map(|(r, fs)| (group_size[r], fs))
            .collect();
        groups.sort_by(|a, b| b.0.cmp(&a.0).then(a.1[0].cmp(&b.1[0])));
        let mut owner = vec![usize::MAX; n];
        let mut load = vec![0usize; units];
        for (s, fs) in groups {
            let u = (0..units).min_by_key(|&u| (load[u], u)).unwrap();
            load[u] += s;
            for f in fs {
                owner[f] = u;
            }
        }
        // An owned global goes with the first unit that reaches it.
        let mut keep = vec![vec![false; n]; units];
        let mut reached_owned: Vec<Vec<usize>> = vec![Vec::new(); units];
        for u in 0..units {
            let roots: Vec<usize> = owned_fns
                .iter()
                .copied()
                .filter(|&f| owner[f] == u)
                .collect();
            let (copies, owned) = reach(&roots);
            for i in roots.into_iter().chain(copies) {
                keep[u][i] = true;
            }
            for &j in &owned {
                if owner[j] == usize::MAX {
                    owner[j] = u;
                }
            }
            reached_owned[u] = owned;
        }
        for i in 0..n {
            if !copied[i] && owner[i] == usize::MAX {
                owner[i] = 0;
            }
        }
        // Globals owned by a unit, and what their initializers reach.
        for u in 0..units {
            let roots: Vec<usize> = (0..n)
                .filter(|&i| !is_fn[i] && !copied[i] && owner[i] == u)
                .collect();
            let (copies, owned) = reach(&roots);
            for i in roots.into_iter().chain(copies) {
                keep[u][i] = true;
            }
            reached_owned[u].extend(owned);
        }
        // A definition another unit reaches is shared by name.
        for (u, reached) in reached_owned.iter().enumerate() {
            for &j in reached {
                if owner[j] != u && is_internal(defs[j]) {
                    let v = defs[j];
                    if name_of(v).is_empty() {
                        let name = format!("karac.cgu.{j}\0");
                        LLVMSetValueName2(v, name.as_ptr() as *const _, name.len() - 1);
                    }
                    LLVMSetLinkage(v, LLVMLinkage::LLVMExternalLinkage);
                    LLVMSetVisibility(v, LLVMVisibility::LLVMHiddenVisibility);
                }
            }
        }
        let names = defs.iter().map(|&v| name_of(v)).collect();
        Some(Plan { keep, names })
    }
}

/// Empty `f`'s body, leaving a declaration.
unsafe fn strip_body(f: LLVMValueRef) -> Result<(), String> {
    let mut all = Vec::new();
    let mut bb = LLVMGetFirstBasicBlock(f);
    while !bb.is_null() {
        let mut ins = LLVMGetFirstInstruction(bb);
        while !ins.is_null() {
            all.push(ins);
            ins = LLVMGetNextInstruction(ins);
        }
        bb = LLVMGetNextBasicBlock(bb);
    }
    // Uses go first (a token has no poison, but nothing a token is used by
    // is a phi, so erasing the unused instructions frees it).
    for &ins in &all {
        let ty = LLVMTypeOf(ins);
        let kind = LLVMGetTypeKind(ty);
        if kind != LLVMTypeKind::LLVMVoidTypeKind
            && kind != LLVMTypeKind::LLVMTokenTypeKind
            && !LLVMGetFirstUse(ins).is_null()
        {
            LLVMReplaceAllUsesWith(ins, LLVMGetPoison(ty));
        }
    }
    while !all.is_empty() {
        let before = all.len();
        all.retain(|&ins| {
            if LLVMGetFirstUse(ins).is_null() {
                LLVMInstructionEraseFromParent(ins);
                false
            } else {
                true
            }
        });
        if all.len() == before {
            return Err(format!("cannot empty {}", name_of(f)));
        }
    }
    loop {
        let bb = LLVMGetFirstBasicBlock(f);
        if bb.is_null() {
            break;
        }
        LLVMDeleteBasicBlock(bb);
    }
    Ok(())
}

/// Keep `unit`'s definitions in `m` (a copy of the planned module) and
/// declare or drop the rest.
unsafe fn cut(m: LLVMModuleRef, plan: &Plan, unit: usize) -> Result<(), String> {
    let defs = definitions(m);
    if defs.len() != plan.names.len()
        || defs.iter().zip(&plan.names).any(|(&v, n)| name_of(v) != *n)
    {
        return Err("a copy of the module does not line up with its plan".into());
    }
    let keep = &plan.keep[unit];
    for (i, &v) in defs.iter().enumerate() {
        if keep[i] {
            continue;
        }
        if !LLVMIsAFunction(v).is_null() {
            strip_body(v)?;
        } else if LLVMGetLinkage(v) != LLVMLinkage::LLVMAppendingLinkage {
            LLVMSetInitializer(v, std::ptr::null_mut());
        }
    }
    // What is left unshared is reached from nothing kept: drop it.
    for (i, &v) in defs.iter().enumerate() {
        if keep[i] || !(is_internal(v) || LLVMGetLinkage(v) == LLVMLinkage::LLVMAppendingLinkage) {
            continue;
        }
        if !LLVMGetFirstUse(v).is_null() {
            LLVMReplaceAllUsesWith(v, LLVMGetPoison(LLVMTypeOf(v)));
        }
        if !LLVMIsAFunction(v).is_null() {
            LLVMDeleteFunction(v);
        } else {
            LLVMDeleteGlobal(v);
        }
    }
    Ok(())
}

/// Optimise and emit each unit of the planned `module` on its own thread,
/// then merge the objects into `obj`. `compile` is what one unit runs.
pub(super) fn compile_units(
    module: LLVMModuleRef,
    plan: &Plan,
    obj: &Path,
    compile: &(dyn Fn(&inkwell::module::Module<'_>, &Path) -> Result<(), String> + Sync),
) -> Result<(), String> {
    let bitcode = unsafe {
        let buf = llvm_sys::bit_writer::LLVMWriteBitcodeToMemoryBuffer(module);
        let mut bytes = std::slice::from_raw_parts(
            LLVMGetBufferStart(buf) as *const u8,
            LLVMGetBufferSize(buf),
        )
        .to_vec();
        LLVMDisposeMemoryBuffer(buf);
        // inkwell reads a buffer as ending in a NUL past the data.
        bytes.push(0);
        bytes
    };
    let parts: Vec<PathBuf> = (0..plan.units())
        .map(|u| {
            let mut p = obj.as_os_str().to_owned();
            p.push(format!(".cgu{u}.o"));
            PathBuf::from(p)
        })
        .collect();
    let results: Vec<Result<(), String>> = std::thread::scope(|s| {
        let handles: Vec<_> = parts
            .iter()
            .enumerate()
            .map(|(u, part)| {
                let bitcode = &bitcode;
                s.spawn(move || -> Result<(), String> {
                    let llcx = inkwell::context::Context::create();
                    let buf = inkwell::memory_buffer::MemoryBuffer::create_from_memory_range_copy(
                        bitcode, "unit",
                    );
                    let m = inkwell::module::Module::parse_bitcode_from_buffer(&buf, &llcx)
                        .map_err(|e| format!("read unit {u}: {}", e.to_string()))?;
                    unsafe { cut(m.as_mut_ptr(), plan, u)? };
                    compile(&m, part)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("a codegen unit panicked".into()))
            })
            .collect()
    });
    let merged = results
        .into_iter()
        .collect::<Result<(), String>>()
        .and_then(|()| {
            let out = std::process::Command::new("cc")
                .args(["-r", "-nostdlib", "-o"])
                .arg(obj)
                .args(&parts)
                .output()
                .map_err(|e| format!("cc -r: {e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "cc -r: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        });
    for p in &parts {
        let _ = std::fs::remove_file(p);
    }
    merged
}
