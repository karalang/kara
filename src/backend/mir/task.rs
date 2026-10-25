//! Tasks under the coroutine transform (B-2026-10-14-49): the readiness
//! native `__ready`, `TaskGroup.spawn_frame`, and the poll functions the
//! runtime's executor (`runtime/src/mir_task.rs`) runs tasks through.
//!
//! A `TaskHandle` is a malloc'd block: a state word (`RUNNING`, `DONE`,
//! `DETACHED` once its handle dropped first, or `CANCELLED`), then the
//! task's result at [`Cx::task_result_offset`]. A task's poll function
//! resumes its frame once; when the frame returns, it stores the result and
//! marks the block done, or, detached, drops the result and frees the
//! block. A frame that returned by its cancel exits (core-semantics §11.9)
//! has no result: the block is marked cancelled, or, detached, freed.

use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::callable::Shim;
use super::{Cx, R};
use crate::mir::layout;
use crate::mir::{ConstKind, Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

/// A handle's task is still running.
pub(super) const RUNNING: u64 = 0;
/// Its result is in the block.
pub(super) const DONE: u64 = 1;
/// Its handle dropped while the task ran; the task frees the block.
pub(super) const DETACHED: u64 = 2;
/// The task was torn down (core-semantics §11.9): no result.
pub(super) const CANCELLED: u64 = 3;

impl<'ctx> Cx<'ctx, '_> {
    /// Where a `TaskHandle[T]`'s block holds the `T`: after the state word,
    /// at `T`'s alignment.
    pub(super) fn task_result_offset(&self, held: Ty) -> R<u64> {
        let align = self.layout(held)?.align.max(1);
        Ok(8u64.div_ceil(align) * align)
    }

    /// The str constant an operand is.
    pub(super) fn str_operand(a: &Operand) -> R<&str> {
        match a {
            Operand::Const(c) => match &c.kind {
                ConstKind::Str(s) => Ok(s),
                _ => Err("a task native's name is not a str constant".into()),
            },
            _ => Err("a task native's name is not a constant".into()),
        }
    }

    /// `__ready(const "<native>", args..)`: whether that native can complete
    /// now, recorded with the executor (always true outside one).
    pub(super) fn ready_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let (name, rest) = args
            .split_first()
            .ok_or("__ready takes the native's name")?;
        let name = Self::str_operand(name)?.to_string();
        let (ty, method) = name.rsplit_once('.').unwrap_or((name.as_str(), ""));
        let base = ty.split('[').next().unwrap_or(ty);
        let i8t = self.llcx.i8_type();
        let r = match (base, method, rest) {
            ("sleep_ms", "", [ms, ..]) => {
                let ms = self.int_arg(ms)?;
                self.rt_flag("ready_sleep", &[ms.into()])?
            }
            // Returned, or torn down.
            ("TaskHandle", "join" | "try_join", [h, ..]) => {
                let (cell, _) = self.cell_arg(h)?;
                let block = self.load_ptr(cell)?;
                let state = self.load_i64(block)?;
                let done = self
                    .builder
                    .build_int_compare(IntPredicate::NE, state, self.i64(RUNNING), "")
                    .map_err(e)?;
                self.ready_flag(done)?
            }
            ("Receiver" | "Channel", "recv" | "recv_blocking", [c, ..])
            | ("Sender" | "Channel", "send", [c, ..]) => {
                let h = self.chan_handle(c)?;
                let send = i8t.const_int(u64::from(method == "send"), false);
                let ok = self.rt_flag("chan_ready", &[h.into(), send.into()])?;
                self.ready_flag(ok)?
            }
            ("TcpListener" | "TlsListener", "accept", [s, ..]) | ("TcpStream", "read", [s, ..]) => {
                let (at, _) = self.behind_refs(s)?;
                let fd = self.load_i64(at)?;
                self.rt_flag("ready_fd", &[fd.into(), i8t.const_zero().into()])?
            }
            ("TcpStream", "write", [s, ..]) => {
                let (at, _) = self.behind_refs(s)?;
                let fd = self.load_i64(at)?;
                self.rt_flag("ready_fd", &[fd.into(), i8t.const_int(1, false).into()])?
            }
            // The connection's own readiness, its handshake and records
            // moved on first (`runtime/src/mir_tls.rs`).
            ("TlsStream", "read" | "write", [s, ..]) => {
                let (at, _) = self.behind_refs(s)?;
                let fd = self.load_i64(at)?;
                let write = i8t.const_int(u64::from(method == "write"), false);
                self.rt_flag("ready_tls", &[fd.into(), write.into()])?
            }
            ("UdpSocket", "recv" | "recv_from", [s, ..]) => {
                let (at, _) = self.behind_refs(s)?;
                let fd = self.load_i64(at)?;
                self.rt_flag("ready_fd", &[fd.into(), i8t.const_zero().into()])?
            }
            // The child's exit makes its wait descriptor readable.
            ("Child", "wait", [c, ..]) => {
                let (at, _) = self.behind_refs(c)?;
                let handle = self.load_ptr(at)?;
                let i64t = self.llcx.i64_type();
                let f = self.extern_fn(
                    "karac_mir_child_wait_fd",
                    i64t.fn_type(&[self.ptr_ty().into()], false),
                );
                let fd = self
                    .builder
                    .build_call(f, &[handle.into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                self.rt_flag("ready_fd", &[fd.into(), i8t.const_zero().into()])?
            }
            // A child's output pipe: bytes or its end to read, or (`drain`,
            // for `read_to_end`) its end reached, what comes first drained
            // (`runtime/src/mir_process.rs`).
            ("ChildStdout" | "ChildStderr", "read" | "read_to_end" | "read_to_string", [p, ..]) => {
                let (at, _) = self.behind_refs(p)?;
                let handle = self.load_ptr(at)?;
                let drain = i8t.const_int(u64::from(method != "read"), false);
                self.rt_flag("ready_pipe", &[handle.into(), drain.into()])?
            }
            // Ready at once, armed, unless the task has a connect under way.
            ("TcpStream", "connect", _) => self.rt_flag("ready_connect", &[])?,
            // The request goes to a helper; ready once its answer is in
            // (`runtime/src/mir_http.rs`).
            ("Client", "get" | "post", _) | ("RequestBuilder", "send", _) => {
                let (op, (a, an), (b, bn), h) = self.http_request(base, method, rest)?;
                let i32t = self.llcx.i32_type();
                self.rt_flag(
                    "ready_http",
                    &[
                        i32t.const_int(op, false).into(),
                        a.into(),
                        an.into(),
                        b.into(),
                        bn.into(),
                        h.into(),
                    ],
                )?
            }
            // A value to lend, by the timeout (B-2026-10-21-44).
            ("Pool", "acquire", [p, d, ..]) => {
                let h = self.pool_handle(p)?;
                let ns = self.duration_ns(d)?;
                self.rt_flag("ready_pool", &[h.into(), ns.into()])?
            }
            ("Mutex", "acquire", [m, ..]) => {
                let (cell, _) = self.cell_arg(m)?;
                self.rt_flag("mutex_ready", &[cell.into()])?
            }
            // A channel to change, or the deadline (core-semantics §11.11).
            ("__select_park", "", [epoch, until, ..]) => {
                let epoch = self.int_arg(epoch)?;
                let until = self.int_arg(until)?;
                self.rt_flag("ready_select", &[epoch.into(), until.into()])?
            }
            ("TaskGroup", "spawn_frame" | "end", [g, ..]) => {
                let g = self.group_handle(g)?;
                let end = i8t.const_int(u64::from(method == "end"), false);
                self.rt_flag("ready_group", &[g.into(), end.into()])?
            }
            _ => return Err(format!("__ready of {name}, which does not suspend")),
        };
        self.store_bool(dest, r)
    }

    /// The runtime state a `TaskGroup` operand (the group, or a reference
    /// to it) points at.
    pub(super) fn group_handle(&mut self, g: &Operand) -> R<PointerValue<'ctx>> {
        let (at, _) = self.behind_refs(g)?;
        self.load_ptr(at)
    }

    /// Reports a readiness codegen worked out to the executor, which notes
    /// what a task that cannot go on waits for.
    fn ready_flag(&mut self, ready: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        let b = self
            .builder
            .build_int_z_extend(ready, self.llcx.i8_type(), "")
            .map_err(e)?;
        self.rt_flag("ready", &[b.into()])
    }

    /// `TaskGroup.spawn_frame(g, const "<task>#resume", move closure)`: a
    /// fresh frame of that resume body's frame type, holding the closure in
    /// its field 1, queued with the executor; the handle is its block.
    pub(super) fn spawn_frame(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let [group, name, closure] = args else {
            return Err("spawn_frame takes a group, a resume body and a closure".into());
        };
        let resume = Self::str_operand(name)?.to_string();
        let (frame_ty, held) = self.task_types(&resume)?;
        let malloc = self.libc_fn("malloc");
        let call_malloc = |cx: &Self, size: u64, what: &str| -> R<PointerValue<'ctx>> {
            Ok(cx
                .builder
                .build_call(malloc, &[cx.i64(size.max(1)).into()], what)
                .map_err(e)?
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value())
        };
        let size = self.layout(frame_ty)?.size;
        let frame = call_malloc(self, size, "frame")?;
        self.builder
            .build_memset(frame, 8, self.llcx.i8_type().const_zero(), self.i64(size))
            .map_err(e)?;
        let ct = self.operand_type(closure)?;
        let src = self.operand_ptr(closure)?;
        let at = self.offset(frame, self.field_offset(frame_ty, None, 1)?)?;
        self.copy(at, src, ct)?;
        let off = self.task_result_offset(held)?;
        let block = call_malloc(self, off + self.layout(held)?.size, "task")?;
        self.store(block, self.i64(RUNNING).into())?;
        let poll = self.task_poll(&resume, true);
        let g = self.group_handle(group)?;
        let p = self.ptr_ty();
        let spawn = self.extern_fn(
            "karac_mir_task_spawn",
            self.llcx
                .void_type()
                .fn_type(&[p.into(), p.into(), p.into(), p.into()], false),
        );
        self.builder
            .build_call(
                spawn,
                &[
                    g.into(),
                    poll.as_global_value().as_pointer_value().into(),
                    frame.into(),
                    block.into(),
                ],
                "",
            )
            .map_err(e)?;
        let d = self.place_ptr(dest)?;
        self.store(d, block.into())
    }

    /// The frame type the resume body `resume` takes, and the `T` of the
    /// `Poll[T]` it returns.
    pub(super) fn task_types(&self, resume: &str) -> R<(Ty, Ty)> {
        let body = self
            .program
            .bodies
            .get(resume)
            .ok_or_else(|| format!("no resume body `{resume}`"))?;
        let TyKind::MutRef(frame) = self.tys.kind(body.locals[1].ty) else {
            return Err(format!("{resume} does not take its frame"));
        };
        let held = self
            .tys
            .field_ty(body.return_ty(), Some(0), 0)
            .ok_or("a `Poll` without a payload")?;
        Ok((frame, held))
    }

    /// The poll function of the task whose root is `resume`: of a spawned
    /// task (`a` its frame, `b` its handle's block), or of `main` (`a` the
    /// slot holding its frame's reference, `b` its `Poll` slot).
    pub(super) fn task_poll(&mut self, resume: &str, spawned: bool) -> FunctionValue<'ctx> {
        self.shim_fn(Shim::TaskPoll {
            resume: resume.to_string(),
            spawned,
        })
    }

    /// Builds the poll function `f` of `resume` (see [`Cx::task_poll`]).
    pub(super) fn build_task_poll(
        &mut self,
        f: FunctionValue<'ctx>,
        resume: &str,
        spawned: bool,
    ) -> R<()> {
        let param = |i: u32| -> R<PointerValue<'ctx>> {
            Ok(f.get_nth_param(i)
                .ok_or("a task poll is missing a parameter")?
                .into_pointer_value())
        };
        let (a, b) = (param(0)?, param(1)?);
        let resume_fn = self.direct_function(resume)?;
        let program = self.program;
        let body = &program.bodies[resume];
        let poll_ty = body.return_ty();
        let (frame_ref, ret) = if spawned {
            let slot = self.c_main_slot(body.locals[1].ty, "frame.ref")?;
            self.store(slot, a.into())?;
            (slot, self.c_main_slot(poll_ty, "poll")?)
        } else {
            (a, b)
        };
        self.builder
            .build_call(resume_fn, &[ret.into(), frame_ref.into()], "")
            .map_err(e)?;
        // `Poll[T]` is `Ready(T) | Pending`: tag 0 is `Ready`.
        let tag = layout::enum_tag(self.tys, poll_ty).ok_or("a resume returns no Poll")?;
        let tag_ty = self.int_ty(tag.size as u32 * 8);
        let t = self
            .builder
            .build_load(tag_ty, ret, "tag")
            .map_err(e)?
            .into_int_value();
        let ready = self
            .builder
            .build_int_compare(IntPredicate::EQ, t, tag_ty.const_zero(), "")
            .map_err(e)?;
        if spawned {
            let (_, held) = self.task_types(resume)?;
            self.if_then(ready, |cx| {
                let v = cx.offset(ret, cx.field_offset(poll_ty, Some(0), 0)?)?;
                let state = cx.load_i64(b)?;
                let detached = cx
                    .builder
                    .build_int_compare(IntPredicate::EQ, state, cx.i64(DETACHED), "")
                    .map_err(e)?;
                let torn = cx.rt_flag("unwinding", &[])?;
                let f = cx.cur_fn()?;
                let (returned, keep, drop, cancelled, gone, end) = (
                    cx.llcx.append_basic_block(f, "returned"),
                    cx.llcx.append_basic_block(f, "keep"),
                    cx.llcx.append_basic_block(f, "detached"),
                    cx.llcx.append_basic_block(f, "cancelled"),
                    cx.llcx.append_basic_block(f, "cancelled.gone"),
                    cx.llcx.append_basic_block(f, "freed"),
                );
                cx.builder
                    .build_conditional_branch(torn, cancelled, returned)
                    .map_err(e)?;
                cx.builder.position_at_end(returned);
                cx.builder
                    .build_conditional_branch(detached, drop, keep)
                    .map_err(e)?;
                cx.builder.position_at_end(keep);
                let off = cx.task_result_offset(held)?;
                cx.copy(cx.offset(b, off)?, v, held)?;
                cx.store(b, cx.i64(DONE).into())?;
                cx.builder.build_unconditional_branch(end).map_err(e)?;
                cx.builder.position_at_end(drop);
                if cx.tys.needs_drop(held) {
                    cx.call_glue(v, held)?;
                }
                cx.free(b)?;
                cx.builder.build_unconditional_branch(end).map_err(e)?;
                // Torn down, it left no result.
                cx.builder.position_at_end(cancelled);
                let mark = cx.llcx.append_basic_block(f, "cancelled.mark");
                cx.builder
                    .build_conditional_branch(detached, gone, mark)
                    .map_err(e)?;
                cx.builder.position_at_end(mark);
                cx.store(b, cx.i64(CANCELLED).into())?;
                cx.builder.build_unconditional_branch(end).map_err(e)?;
                cx.builder.position_at_end(gone);
                cx.free(b)?;
                cx.builder.build_unconditional_branch(end).map_err(e)?;
                cx.builder.position_at_end(end);
                // A returned frame owns nothing.
                cx.free(a)
            })?;
        }
        let r = self
            .builder
            .build_int_z_extend(ready, self.llcx.i8_type(), "")
            .map_err(e)?;
        self.builder.build_return(Some(&r)).map_err(e)?;
        Ok(())
    }
}
