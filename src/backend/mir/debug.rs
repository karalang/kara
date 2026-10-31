//! Where compiled code is, in the source (the M2 debugger check).
//!
//! **Runtime errors** report their place as the MIR interpreter does,
//! `at file:line:col`: an abort in a body of the program's own source names
//! the statement it is in; one in a library body (`unwrap`'s panic) names
//! the program's call into the library, which every call into a library
//! body that can abort records in a global first. Always on: the cost is a
//! constant string per abort site, and two stores per such call (none
//! before `next` and the other library bodies that cannot abort).
//!
//! **Line tables** for a debugger: each body of
//! the program's own source gets a `DISubprogram`, and each statement and
//! terminator the `DILocation` of its MIR `source_info`, so a debugger
//! steps, breaks and backtraces by Kāra file and line. The library bodies
//! appended to every program carry none: their spans are offsets into the
//! appended text, which is no file a debugger could open.
//!
//! Line tables only (`DWARFEmissionKind::LineTablesOnly`): no variable or
//! type descriptions. Off by default, as for the legacy backend
//! (`codegen/debug_info.rs`): `KARAC_DEBUG_INFO=1` turns it on, which also
//! keeps the linker's `strip -x` off the binary.
//!
//! A location is stamped in the scope of the function the builder is in, read
//! back from that function (`get_subprogram`), never from tracked state, so an
//! instruction in glue or a shim (no subprogram) carries none: the rule the
//! verifier enforces, kept by construction.

use std::path::Path;

use inkwell::debug_info::{
    AsDIScope, DICompileUnit, DIFile, DIFlags, DIFlagsConstants, DWARFEmissionKind,
    DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::{FlagBehavior, Linkage};
use inkwell::types::{BasicMetadataTypeEnum, BasicType};
use inkwell::values::{FunctionValue, IntValue, PointerValue};

use super::{Cx, R};
use crate::mir::{ConstKind, Operand, TerminatorKind};
use crate::token::Span;

pub(super) struct Debug<'ctx> {
    di: DebugInfoBuilder<'ctx>,
    unit: DICompileUnit<'ctx>,
    file: DIFile<'ctx>,
}

/// The file name and absolute directory a debugger finds `name` by.
fn file_and_dir(name: &str) -> (String, String) {
    let path = Path::new(name);
    let file = path
        .file_name()
        .map_or_else(|| name.to_string(), |f| f.to_string_lossy().into_owned());
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = match parent {
        Some(p) => p.canonicalize().unwrap_or_else(|_| p.to_path_buf()),
        None => std::env::current_dir().unwrap_or_default(),
    };
    (file, dir.to_string_lossy().into_owned())
}

impl<'ctx> Cx<'ctx, '_> {
    /// Starts the module's debug info for the source file `name`, when
    /// `KARAC_DEBUG_INFO` asks for it.
    pub(super) fn debug_init(&mut self, name: &str) {
        if !crate::backend::read_debug_info_env() {
            return;
        }
        let (file, dir) = file_and_dir(name);
        let i32t = self.llcx.i32_type();
        self.module.add_basic_value_flag(
            "Debug Info Version",
            FlagBehavior::Warning,
            i32t.const_int(3, false),
        );
        self.module.add_basic_value_flag(
            "Dwarf Version",
            FlagBehavior::Warning,
            i32t.const_int(4, false),
        );
        let (di, unit) = self.module.create_debug_info_builder(
            true,
            DWARFSourceLanguage::C,
            &file,
            &dir,
            "karac",
            false,
            "",
            0,
            "",
            DWARFEmissionKind::LineTablesOnly,
            0,
            false,
            false,
            "",
            "",
        );
        let file = unit.get_file();
        self.debug = Some(Debug { di, unit, file });
    }

    /// Whether `span` lies in the program's own source rather than in the
    /// library appended to it.
    pub(super) fn in_user_source(&self, span: Span) -> bool {
        self.program.user_source_len.is_none_or(|n| span.offset < n)
    }

    /// Where `body` starts, for its subprogram: its first located statement
    /// or terminator (a body's own `span` is where its building ended).
    pub(super) fn body_start(&self, body: &crate::mir::Body) -> Span {
        body.blocks
            .iter()
            .flat_map(|b| {
                b.statements
                    .iter()
                    .map(|s| s.source_info.span)
                    .chain(std::iter::once(b.terminator.source_info.span))
            })
            .filter(|s| s.line > 0)
            .min_by_key(|s| s.offset)
            .unwrap_or(body.span)
    }

    /// Gives `f`, named `name` (`linkage` in the object), a subprogram at
    /// `span` when that is in the program's own source, and places the
    /// builder's location there. A body's function, and the C side's
    /// `karac.main`, which calls the Kāra `main`: a call without a location
    /// loses the locations of what is inlined into it.
    pub(super) fn debug_enter(
        &mut self,
        name: &str,
        linkage: &str,
        span: Span,
        f: FunctionValue<'ctx>,
    ) {
        self.builder.unset_current_debug_location();
        let Some(d) = &self.debug else {
            return;
        };
        if !self.in_user_source(span) {
            return;
        }
        let ty =
            d.di.create_subroutine_type(d.file, None, &[], DIFlags::ZERO);
        let line = span.line as u32;
        let sp = d.di.create_function(
            d.unit.as_debug_info_scope(),
            name,
            Some(linkage),
            d.file,
            line,
            ty,
            true,
            true,
            line,
            DIFlags::ZERO,
            false,
        );
        f.set_subprogram(sp);
        if line == 0 {
            // A body the lowering made (a task's root) has no source line,
            // and its statements none either, so nothing would set a
            // location; a call with none fails the verifier in a function
            // with a subprogram. Line 0 says the code is the compiler's
            // (B-2026-10-26-6).
            let loc =
                d.di.create_debug_location(self.llcx, 0, 0, sp.as_debug_info_scope(), None);
            self.builder.set_current_debug_location(loc);
        }
        self.debug_at(span);
    }

    /// Sets the location of the instructions built next to `span`, in the
    /// body being lowered.
    pub(super) fn debug_at(&self, span: Span) {
        let Some(Debug { di, .. }) = &self.debug else {
            return;
        };
        let Some(sp) = self
            .builder
            .get_insert_block()
            .and_then(|b| b.get_parent())
            .and_then(|f| f.get_subprogram())
        else {
            self.builder.unset_current_debug_location();
            return;
        };
        if !self.in_user_source(span) || span.line == 0 {
            return;
        }
        let loc = di.create_debug_location(
            self.llcx,
            span.line as u32,
            span.column as u32,
            sp.as_debug_info_scope(),
            None,
        );
        self.builder.set_current_debug_location(loc);
    }

    /// Leaves the body: what is built next (glue, shims) has no location.
    pub(super) fn debug_leave(&mut self) {
        self.builder.unset_current_debug_location();
    }

    pub(super) fn debug_finish(&self) {
        if let Some(d) = &self.debug {
            d.di.finalize();
        }
    }

    /// The statement or terminator about to be lowered is at `span`: the
    /// place an abort in it reports, and its debugger location.
    pub(super) fn locate(&mut self, span: Span) {
        self.site = span;
        self.debug_at(span);
    }

    /// `file:line:col` of `span`.
    pub(super) fn site_text(span: Span) -> String {
        let (file, line) = crate::mir::interp::source_at(span.line);
        format!("{file}:{line}:{}", span.column)
    }

    /// The count of threads whose error return trace has frames, which the
    /// runtime keeps (`karac_mir_error_trace_push`, B-2026-10-25-3).
    pub(super) fn error_trace_live(&self) -> PointerValue<'ctx> {
        let name = "karac.trace.live";
        let g = self.module.get_global(name).unwrap_or_else(|| {
            let t = self.llcx.i64_type();
            let g = self.module.add_global(t, None, name);
            g.set_initializer(&t.const_zero());
            g.set_linkage(Linkage::Internal);
            g.set_alignment(8);
            g
        });
        g.as_pointer_value()
    }

    /// Whether the body `name` is located: a library body that can
    /// abort, which takes the place of the program's call into it as two
    /// trailing parameters, forwarded from library body to library body,
    /// so that its abort reports the program's line. A place in a global
    /// cost two stores before each such call, on every trip of a loop
    /// making one (kata 38: 1.18x legacy's instructions, B-2026-10-31-2),
    /// and the JIT on Apple, which cannot link a thread-local, shared it
    /// between the pool's workers, so an abort on one could report
    /// another's call (B-2026-10-28-44).
    pub(super) fn located(&mut self, name: &str) -> bool {
        if let Some(&l) = self.located.get(name) {
            return l;
        }
        let program = self.program;
        let l = match program.bodies.get(name) {
            Some(body) => !self.in_user_source(self.body_start(body)) && self.may_abort(name),
            None => false,
        };
        self.located.insert(name.to_string(), l);
        l
    }

    /// The function of the located body `name`, declared now (and the body
    /// scheduled): its by-value or pointer convention, then the place.
    pub(super) fn located_function(&mut self, name: &str) -> R<FunctionValue<'ctx>> {
        if let Some(&f) = self.lfns.get(name) {
            return Ok(f);
        }
        let program = self.program;
        let body = program
            .bodies
            .get(name)
            .ok_or_else(|| format!("no body named `{name}`"))?;
        let place: [BasicMetadataTypeEnum<'ctx>; 2] =
            [self.ptr_ty().into(), self.llcx.i64_type().into()];
        let ty = match self.value_sig(body) {
            Some((params, ret)) => {
                let mut ps: Vec<BasicMetadataTypeEnum> =
                    params.iter().flatten().map(|t| (*t).into()).collect();
                ps.extend(place);
                match ret {
                    Some(r) => r.fn_type(&ps, false),
                    None => self.llcx.void_type().fn_type(&ps, false),
                }
            }
            None => {
                let mut ps: Vec<BasicMetadataTypeEnum> =
                    (0..=body.arg_count).map(|_| self.ptr_ty().into()).collect();
                ps.extend(place);
                self.llcx.void_type().fn_type(&ps, false)
            }
        };
        let f = self.module.add_function(
            &format!("{}.at", super::fn_symbol(name)),
            ty,
            Some(Linkage::Internal),
        );
        self.lfns.insert(name.to_string(), f);
        self.schedule(body);
        Ok(f)
    }

    /// The place a call into a located body passes: the call's own, in a
    /// body of the program's source; in a located body, the place it was
    /// given; otherwise none.
    pub(super) fn site_args(&mut self) -> R<(PointerValue<'ctx>, IntValue<'ctx>)> {
        let own = match self.body {
            Some(b) => self.in_user_source(self.body_start(b)),
            None => false,
        };
        if own {
            if self.in_user_source(self.site) && self.site.line > 0 {
                return self.str_const(&Self::site_text(self.site));
            }
            return Ok(self.no_site());
        }
        Ok(self.given_site().unwrap_or_else(|| self.no_site()))
    }

    /// The place parameters of the function being built, when it is the
    /// located function being lowered (a shim built meanwhile is not).
    fn given_site(&self) -> Option<(PointerValue<'ctx>, IntValue<'ctx>)> {
        let (f, p, n) = self.site_params?;
        let here = self.builder.get_insert_block()?.get_parent()?;
        (here == f).then_some((p, n))
    }

    fn no_site(&self) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        (
            self.ptr_ty().const_null(),
            self.llcx.i64_type().const_zero(),
        )
    }

    /// Whether the body `name` can reach an `Abort`: its own, or one in a
    /// body it calls. A cycle counts as not aborting until shown otherwise.
    fn may_abort(&mut self, name: &str) -> bool {
        if let Some(&a) = self.aborts.get(name) {
            return a;
        }
        self.aborts.insert(name.to_string(), false);
        let program = self.program;
        let Some(body) = program.bodies.get(name) else {
            return false;
        };
        let mut aborts = false;
        for b in &body.blocks {
            match &b.terminator.kind {
                TerminatorKind::Abort { .. } => aborts = true,
                TerminatorKind::Call {
                    func: Operand::Const(c),
                    ..
                } => {
                    if let ConstKind::FnDef(inst) = &c.kind {
                        // In a program with an isolating group, an `Abort`
                        // is a call of `__panic` (core-semantics §11.12).
                        aborts =
                            inst.name == crate::mir::coroutine::PANIC || self.may_abort(&inst.name);
                    }
                }
                _ => {}
            }
            if aborts {
                break;
            }
        }
        self.aborts.insert(name.to_string(), aborts);
        aborts
    }

    /// The place an abort built now reports: the statement being lowered,
    /// in a body of the program's own source; in a located body, the place
    /// of the program's call into it; otherwise none.
    pub(super) fn abort_site(&mut self) -> R<(PointerValue<'ctx>, IntValue<'ctx>)> {
        let own = match self.body {
            Some(b) => self.in_user_source(self.body_start(b)),
            None => false,
        };
        if own && self.in_user_source(self.site) && self.site.line > 0 {
            return self.str_const(&Self::site_text(self.site));
        }
        Ok(self.given_site().unwrap_or_else(|| self.no_site()))
    }
}
