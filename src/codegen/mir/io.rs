//! The I/O natives: `File`, processes, sockets, the HTTP client and server
//! and `Clock`, each a call into the runtime's blocking implementation
//! through its `karac_mir_*` entry (`runtime/src/mir_io.rs`). Their
//! reference semantics are the interpreter's `file_method`,
//! `process_method`, `tcp_method`, `http_method`, `http_server_method` and
//! its `Clock.now` arm (`src/mir/interp.rs`).
//!
//! A fallible entry reports as `karac_mir_fs` does: a negative return is
//! success, with the payload in an `i64` out slot; `0..=6` is the `IoError`
//! variant, in the order [`IO_ERRORS`] lists, and `Other` comes with its
//! message as a `String` in a second out slot.
use inkwell::values::{BasicMetadataValueEnum, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::callable::Callee;
use super::{Cx, R};
use crate::mir::ty::IntrinsicTy;
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

/// The `IoError` variants by the runtime's code for them.
pub(super) const IO_ERRORS: [&str; 9] = [
    "NotFound",
    "PermissionDenied",
    "AlreadyExists",
    "UnexpectedEof",
    "InvalidUtf8",
    "Interrupted",
    "Other",
    "InvalidInput",
    "Unsupported",
];

/// The `NetError` variants by the runtime's code for them.
const NET_ERRORS: [&str; 8] = [
    "Interrupted",
    "Other",
    "AddrInUse",
    "ConnectionRefused",
    "PermissionDenied",
    "TimedOut",
    "WouldBlock",
    "InvalidInput",
];

impl<'ctx> Cx<'ctx, '_> {
    /// Lowers the I/O native `base.method`, or `None` when it is not one.
    pub(super) fn io_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        match (base, method) {
            // The default providers (§14.7) are the ambient resources'
            // natives with a receiver, which carries nothing.
            ("OsEnv" | "OsClock" | "OsRandom", _) if !args.is_empty() => {
                let resource = match base {
                    "OsEnv" => "Env",
                    "OsClock" => "Clock",
                    _ => "RandomSource",
                };
                self.native(&format!("{resource}.{method}"), &args[1..], dest)
                    .map(Some)
            }
            ("File", "options") => self.options_native(method, args, dest),
            ("File", _) => self.file_native(method, args, dest),
            ("OpenOptions", "open") => self.file_native("options_open", args, dest),
            ("OpenOptions", _) => self.options_native(method, args, dest),
            ("BufReader" | "BufWriter" | "LinesIter", _) => {
                self.buf_native(base, method, args, dest)
            }
            ("Stdio", _) => self.stdio_native(method, args, dest),
            ("Command", "spawn" | "output") => self.command_native(method, args, dest).map(Some),
            ("Child", _) => self.child_native(method, args, dest),
            ("ChildStdout" | "ChildStderr" | "ChildStdin", _) => {
                self.pipe_native(method, args, dest)
            }
            ("TcpListener" | "TcpStream" | "UdpSocket", _) => {
                self.tcp_native(base, method, args, dest)
            }
            ("Client" | "RequestBuilder" | "Response" | "HttpError", _) => {
                self.http_native(base, method, args, dest)
            }
            ("RegexEngine", _) => self.regex_native(method, args, dest),
            ("Server", "serve") => self.serve_native(args, dest).map(Some),
            ("Server", "serve_static") => self.serve_static_native(args, dest).map(Some),
            ("Request", _) => self.request_native(method, args, dest),
            ("Clock", "now" | "monotonic_ns" | "wall_ns") => {
                let i64t = self.llcx.i64_type();
                let name = match method {
                    "now" => "karac_runtime_clock_now",
                    "monotonic_ns" => "karac_runtime_clock_monotonic_ns",
                    _ => "karac_runtime_clock_wall_ns",
                };
                let f = self.extern_fn(name, i64t.fn_type(&[], false));
                let v = self
                    .builder
                    .build_call(f, &[], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic();
                let d = self.place_ptr(dest)?;
                self.store(d, v)?;
                Ok(Some(()))
            }
            _ => Ok(None),
        }
    }

    /// `File.options()` and the `OpenOptions` builder (B-2026-10-15-28),
    /// inline: `OpenOptions { flags }` with the bits read 1, write 2,
    /// append 4, truncate 8, create 16, create_new 32, as the interpreter
    /// sets them.
    fn options_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let bit: u64 = match method {
            "options" => 0,
            "read" => 1,
            "write" => 2,
            "append" => 4,
            "truncate" => 8,
            "create" => 16,
            "create_new" => 32,
            _ => return Ok(None),
        };
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        let d = self.offset(d, self.field_offset(dty, None, 0)?)?;
        if bit == 0 {
            self.store(d, self.i64(0).into())?;
            return Ok(Some(()));
        }
        let (at, ot) = self.behind_refs(&args[0])?;
        let at = self.offset(at, self.field_offset(ot, None, 0)?)?;
        let flags = self.load_i64(at)?;
        let bt = self.operand_type(&args[1])?;
        let on = self.operand_ptr(&args[1])?;
        let on = self.load(on, bt)?.into_int_value();
        let i1 = self.llcx.bool_type();
        let on = if on.get_type() == i1 {
            on
        } else {
            self.builder
                .build_int_compare(IntPredicate::NE, on, on.get_type().const_zero(), "")
                .map_err(e)?
        };
        let set = self.builder.build_or(flags, self.i64(bit), "").map_err(e)?;
        let clear = self
            .builder
            .build_and(flags, self.i64(!bit), "")
            .map_err(e)?;
        let v = self.builder.build_select(on, set, clear, "").map_err(e)?;
        self.store(d, v)?;
        Ok(Some(()))
    }

    /// `File.open` / `create` / `append` and the handle methods, through
    /// `karac_mir_file(op, handle, ptr, len, n, value, msg)`.
    fn file_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let op: u64 = match method {
            "open" => 0,
            "create" => 1,
            "append" => 2,
            "read" => 3,
            "write" => 4,
            "flush" => 5,
            "sync_all" => 6,
            "sync_data" => 7,
            "seek" => 8,
            "set_len" => 9,
            "options_open" => 10,
            _ => return Ok(None),
        };
        let null = self.ptr_ty().const_null();
        let (handle, ptr, len, n) = if op <= 2 {
            let (p, n) = self.str_parts(&args[0])?;
            (null, p, n, self.i64(0))
        } else if op == 10 {
            // `OpenOptions { flags }`, by value or behind a reference, then
            // the path (B-2026-10-15-28).
            let (at, ot) = self.behind_refs(&args[0])?;
            let at = self.offset(at, self.field_offset(ot, None, 0)?)?;
            let flags = self.load_i64(at)?;
            let (p, n) = self.str_parts(&args[1])?;
            (null, p, n, flags)
        } else {
            let (at, _) = self.behind_refs(&args[0])?;
            let handle = self.load_ptr(at)?;
            match method {
                "read" | "write" => {
                    let (p, n) = self.view_of(&args[1])?;
                    (handle, p, n, self.i64(0))
                }
                "seek" => {
                    let wt = self.operand_type(&args[1])?;
                    // The runtime's `whence` byte is the variant's index.
                    for (k, name) in ["Start", "Current", "End"].iter().enumerate() {
                        if self.variant_of(wt, name)? != k as u32 {
                            return Err(format!("SeekFrom.{name} is not variant {k}"));
                        }
                    }
                    let w = self.operand_ptr(&args[1])?;
                    let tag = self.load_tag(w, wt)?;
                    let i64t = self.llcx.i64_type();
                    let whence = self
                        .builder
                        .build_int_z_extend_or_bit_cast(tag, i64t, "")
                        .map_err(e)?;
                    let off = self.operand_ptr(&args[2])?;
                    let off = self.load_i64(off)?;
                    (handle, null, off, whence)
                }
                "set_len" => {
                    let l = self.operand_ptr(&args[1])?;
                    let l = self.load_i64(l)?;
                    (handle, null, l, self.i64(0))
                }
                _ => (handle, null, self.i64(0), self.i64(0)),
            }
        };
        let i32t = self.llcx.i32_type();
        self.io_call(
            "file",
            &[
                i32t.const_int(op, false).into(),
                handle.into(),
                ptr.into(),
                len.into(),
                n.into(),
            ],
            args,
            dest,
        )?;
        Ok(Some(()))
    }

    /// Whether `t` is one of a child's pipes.
    fn is_pipe(&self, t: Ty) -> bool {
        self.tys
            .tcx()
            .adt_of(t)
            .is_some_and(|(a, _)| matches!(&*a.name, "ChildStdout" | "ChildStderr" | "ChildStdin"))
    }

    /// `BufReader` / `BufWriter` and a `LinesIter`'s `next`, through
    /// `karac_mir_buf(op, handle, ptr, len, value, msg)`, as the
    /// interpreter's `buf_method`. A `LinesIter` is its reader's handle,
    /// which it borrows.
    fn buf_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let op: u64 = match (base, method) {
            ("BufReader", "new" | "with_capacity") => 0,
            ("BufWriter", "new" | "with_capacity") => 1,
            ("BufReader", "read_line") => 2,
            ("BufReader", "read_to_string") => 3,
            ("BufReader", "read") => 4,
            ("LinesIter", "next") => 5,
            ("BufWriter", "write") => 6,
            ("BufWriter", "write_all") => 7,
            ("BufWriter", "flush") => 8,
            ("BufReader", "consume") => 9,
            ("BufReader", "fill_buf") => 12,
            ("BufReader", "lines") => {
                let (at, _) = self.behind_refs(&args[0])?;
                let h = self.load_ptr(at)?;
                let d = self.place_ptr(dest)?;
                self.store(d, h.into())?;
                return Ok(Some(()));
            }
            _ => return Ok(None),
        };
        // Over a child's pipe rather than a `File` (B-2026-10-15-44).
        let op = if op <= 1 && self.is_pipe(self.operand_type(&args[0])?) {
            10 + op
        } else {
            op
        };
        let null = self.ptr_ty().const_null();
        let zero = self.i64(0);
        let i32t = self.llcx.i32_type();
        let opv = i32t.const_int(op, false);
        if op <= 1 || op == 10 || op == 11 {
            // The `File` moves in and the runtime takes it, so nothing
            // drops it here.
            let f = self.operand_ptr(&args[0])?;
            let file = self.load_ptr(f)?;
            let cap = match args.get(1) {
                Some(a) => {
                    let p = self.operand_ptr(a)?;
                    self.load_i64(p)?
                }
                None => zero,
            };
            let value = self.entry_slot(8, "buf.handle")?;
            self.rt_int(
                "buf",
                &[
                    opv.into(),
                    file.into(),
                    null.into(),
                    cap.into(),
                    value.into(),
                    null.into(),
                ],
                32,
            )?;
            let h = self.load_i64(value)?;
            let d = self.place_ptr(dest)?;
            self.store(d, h.into())?;
            return Ok(Some(()));
        }
        let (at, _) = self.behind_refs(&args[0])?;
        let handle = self.load_ptr(at)?;
        let (ptr, len) = match op {
            2 | 3 => {
                let (s, _) = self.behind_refs(&args[1])?;
                (s, zero)
            }
            4 | 6 | 7 => self.view_of(&args[1])?,
            9 => {
                let p = self.operand_ptr(&args[1])?;
                (null, self.load_i64(p)?)
            }
            _ => (null, zero),
        };
        if op == 9 {
            let value = self.entry_slot(8, "buf.value")?;
            self.rt_int(
                "buf",
                &[
                    opv.into(),
                    handle.into(),
                    ptr.into(),
                    len.into(),
                    value.into(),
                    null.into(),
                ],
                32,
            )?;
            return Ok(Some(()));
        }
        let call = [opv.into(), handle.into(), ptr.into(), len.into()];
        if op == 12 {
            // `fill_buf`: a view of the runtime reader's buffer, its pointer
            // and length in `value[0..2]` (B-2026-10-14-72).
            let value = self.entry_slot(16, "buf.view")?;
            let msg = self.temp(
                self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String)),
                "io.msg",
            )?;
            let mut all = call.to_vec();
            all.push(value.into());
            all.push(msg.into());
            let code = self.rt_int("buf", &all, 32)?;
            self.drop_moved(args)?;
            self.store_io_result(dest, code, value, msg)?;
            return Ok(Some(()));
        }
        if op != 5 {
            self.io_call("buf", &call, args, dest)?;
            return Ok(Some(()));
        }
        // `next`: `None` at the end (`-2`), else `Some` of the line's
        // `Result`.
        let value = self.entry_slot(8, "io.value")?;
        let msg = self.temp(
            self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String)),
            "io.msg",
        )?;
        let mut all = call.to_vec();
        all.push(value.into());
        all.push(msg.into());
        let code = self.rt_int("buf", &all, 32)?;
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        let (some, none) = self.option_variants(dty)?;
        let rt = self
            .tys
            .field_ty(dty, Some(some), 0)
            .ok_or("an option without a payload")?;
        let off = self.field_offset(dty, Some(some), 0)?;
        let more = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                code,
                i32t.const_int((-2i64) as u64, true),
                "",
            )
            .map_err(e)?;
        self.store_tag(d, dty, none)?;
        self.if_then(more, |cx| {
            let q = cx.offset(d, off)?;
            cx.store_io_result_at(q, rt, code, value, msg, None)?;
            cx.store_tag(d, dty, some)
        })?;
        Ok(Some(()))
    }

    /// The `Stdio` constructors through `karac_mir_stdio(kind, handle)`,
    /// which takes a `File` or pipe handed in by move.
    fn stdio_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let kind: u64 = match method {
            "inherit" => 0,
            "null" => 1,
            "piped" => 2,
            "from_file" => 3,
            "from_stdout" | "from_stderr" => 4,
            _ => return Ok(None),
        };
        let handle = if kind >= 3 {
            // The handle moves in and the runtime takes it, so nothing
            // drops it here.
            let a = self.operand_ptr(&args[0])?;
            self.load_ptr(a)?
        } else {
            self.ptr_ty().const_null()
        };
        let i32t = self.llcx.i32_type();
        let f = self.extern_fn(
            "karac_mir_stdio",
            self.ptr_ty()
                .fn_type(&[i32t.into(), self.ptr_ty().into()], false),
        );
        let h = self
            .builder
            .build_call(f, &[i32t.const_int(kind, false).into(), handle.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic();
        let d = self.place_ptr(dest)?;
        self.store(d, h)?;
        Ok(Some(()))
    }

    /// `Command.spawn` and `output` through `karac_mir_command`, handing it
    /// the `Command`'s fields where they lie.
    fn command_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<()> {
        let (cmd, ty) = self.behind_refs(&args[0])?;
        let fields: Vec<String> = self
            .tys
            .tcx()
            .adt_of(ty)
            .map(|(adt, _)| {
                adt.variants[0]
                    .fields
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect()
            })
            .ok_or("Command.spawn of a non-struct")?;
        let field = |cx: &Self, n: &str| -> R<(PointerValue<'ctx>, Ty)> {
            let k = fields
                .iter()
                .position(|f| f == n)
                .ok_or_else(|| format!("a Command without `{n}`"))? as u32;
            let t = cx
                .tys
                .field_ty(ty, None, k)
                .ok_or_else(|| format!("`{n}` has no type"))?;
            Ok((cx.offset(cmd, cx.field_offset(ty, None, k)?)?, t))
        };
        let (prog, _) = field(self, "program")?;
        let (a, _) = field(self, "cmd_args")?;
        let (env, _) = field(self, "cmd_env")?;
        let (dir, dt) = field(self, "cmd_dir")?;
        let dir = self.some_ptr(dir, dt)?;
        let stdio = self.entry_slot(24, "cmd.stdio")?;
        for (i, n) in ["cmd_stdin", "cmd_stdout", "cmd_stderr"].iter().enumerate() {
            let (p, t) = field(self, n)?;
            let at = self.some_ptr(p, t)?;
            // A `Stdio` is a non-null handle: `None` is null.
            let null = self.ptr_ty().const_null();
            let is = self.builder.build_is_null(at, "").map_err(e)?;
            let h = self.load_ptr_or(at, is, null)?;
            self.store(self.offset(stdio, 8 * i as u64)?, h.into())?;
        }
        let output = method == "output";
        let i32t = self.llcx.i32_type();
        let vals: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            prog.into(),
            self.load_ptr(a)?.into(),
            self.load_i64(self.offset(a, 8)?)?.into(),
            self.load_ptr(env)?.into(),
            self.load_i64(self.offset(env, 8)?)?.into(),
            dir.into(),
            stdio.into(),
            i32t.const_int(u64::from(output), false).into(),
        ];
        self.bytes_call("command", &vals, dest)
    }

    /// Calls the fallible `karac_mir_{name}(args.., value, out, err, msg)`
    /// and writes its `Result` at `dest`, an `Output` built from the bytes
    /// it left at `out` and `err` when that is the payload.
    fn bytes_call(
        &mut self,
        name: &str,
        call_args: &[BasicMetadataValueEnum<'ctx>],
        dest: &Place,
    ) -> R<()> {
        let s = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let value = self.entry_slot(8, "io.value")?;
        let out = self.temp(s, "io.out")?;
        let err = self.temp(s, "io.err")?;
        let msg = self.temp(s, "io.msg")?;
        let mut all = call_args.to_vec();
        all.extend::<[BasicMetadataValueEnum<'ctx>; 4]>([
            value.into(),
            out.into(),
            err.into(),
            msg.into(),
        ]);
        let code = self.rt_int(name, &all, 32)?;
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        self.store_io_result_at(d, dty, code, value, msg, Some((out, err)))
    }

    /// The methods of a `Child` through `karac_mir_child`; the receiver is
    /// a struct around its `ChildProcess` handle, by reference.
    fn child_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let op: u64 = match method {
            "wait" => 0,
            "try_wait" => 1,
            "kill" => 2,
            "wait_with_output" => 3,
            "id" => 4,
            "stdin" => 5,
            "stdout" => 6,
            "stderr" => 7,
            _ => return Ok(None),
        };
        let (at, _) = self.behind_refs(&args[0])?;
        let handle = self.load_ptr(at)?;
        let i32t = self.llcx.i32_type();
        let call: Vec<BasicMetadataValueEnum<'ctx>> =
            vec![i32t.const_int(op, false).into(), handle.into()];
        if op <= 3 {
            self.bytes_call("child", &call, dest)?;
            return Ok(Some(()));
        }
        // `id` cannot fail; a pipe is `Some` of its handle, or `None` (0).
        let s = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let value = self.entry_slot(8, "io.value")?;
        let out = self.temp(s, "io.out")?;
        let msg = self.temp(s, "io.msg")?;
        let mut all = call;
        all.extend::<[BasicMetadataValueEnum<'ctx>; 4]>([
            value.into(),
            out.into(),
            out.into(),
            msg.into(),
        ]);
        self.rt_int("child", &all, 32)?;
        let got = self.load_i64(value)?;
        let d = self.place_ptr(dest)?;
        if op == 4 {
            self.store(d, got.into())?;
            return Ok(Some(()));
        }
        let dty = self.place_type(dest)?;
        let (some, none) = self.option_variants(dty)?;
        let off = self.field_offset(dty, Some(some), 0)?;
        let taken = self
            .builder
            .build_int_compare(IntPredicate::NE, got, self.i64(0), "")
            .map_err(e)?;
        self.store_tag(d, dty, none)?;
        self.if_then(taken, |cx| {
            cx.store_tag(d, dty, some)?;
            cx.store(cx.offset(d, off)?, got.into())
        })?;
        Ok(Some(()))
    }

    /// The methods of a child's pipes through `karac_mir_pipe(op, handle,
    /// ptr, len, value, msg)`.
    fn pipe_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let op: u64 = match method {
            "read" => 0,
            "read_to_end" => 1,
            "read_to_string" => 2,
            "write" => 3,
            "write_all" => 4,
            "flush" => 5,
            "close" => 6,
            _ => return Ok(None),
        };
        let (at, _) = self.behind_refs(&args[0])?;
        let handle = self.load_ptr(at)?;
        let (ptr, len) = match op {
            0 | 3 | 4 => self.view_of(&args[1])?,
            1 | 2 => (self.behind_refs(&args[1])?.0, self.i64(0)),
            _ => (self.ptr_ty().const_null(), self.i64(0)),
        };
        let i32t = self.llcx.i32_type();
        self.io_call(
            "pipe",
            &[
                i32t.const_int(op, false).into(),
                handle.into(),
                ptr.into(),
                len.into(),
            ],
            args,
            dest,
        )?;
        Ok(Some(()))
    }

    /// The payload of the `Option` of type `ty` at `p`, or null for `None`.
    fn some_ptr(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<PointerValue<'ctx>> {
        let (some, _) = self.option_variants(ty)?;
        let tag = self.load_tag(p, ty)?;
        let is = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                tag.get_type().const_int(u64::from(some), false),
                "",
            )
            .map_err(e)?;
        let payload = self.offset(p, self.field_offset(ty, Some(some), 0)?)?;
        let null = self.ptr_ty().const_null();
        Ok(self
            .builder
            .build_select(is, payload, null, "")
            .map_err(e)?
            .into_pointer_value())
    }

    /// The pointer at `p`, or `or` when `skip` (and `p` is not to be read).
    fn load_ptr_or(
        &mut self,
        p: PointerValue<'ctx>,
        skip: IntValue<'ctx>,
        or: PointerValue<'ctx>,
    ) -> R<PointerValue<'ctx>> {
        let slot = self.entry_slot(8, "io.ptr")?;
        self.store(slot, or.into())?;
        let read = self.builder.build_not(skip, "").map_err(e)?;
        self.if_then(read, |cx| {
            let v = cx.load_ptr(p)?;
            cx.store(slot, v.into())
        })?;
        self.load_ptr(slot)
    }

    /// Calls the fallible `karac_mir_{name}(args.., value, msg)` and writes
    /// its `Result` at `dest`: `Ok` of the `i64` it left in `value` (none
    /// for a `Unit` payload), or `Err` of the `IoError` its code names.
    /// Drops the arguments the native was handed by move.
    fn io_call(
        &mut self,
        name: &str,
        call_args: &[BasicMetadataValueEnum<'ctx>],
        args: &[Operand],
        dest: &Place,
    ) -> R<()> {
        let value = self.entry_slot(8, "io.value")?;
        let msg = self.temp(
            self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String)),
            "io.msg",
        )?;
        let mut all = call_args.to_vec();
        all.push(value.into());
        all.push(msg.into());
        let code = self.rt_int(name, &all, 32)?;
        self.drop_moved(args)?;
        self.store_io_result(dest, code, value, msg)
    }

    /// Writes the `Result` a `karac_mir_*` I/O entry reported at `dest`.
    fn store_io_result(
        &mut self,
        dest: &Place,
        code: IntValue<'ctx>,
        value: PointerValue<'ctx>,
        msg: PointerValue<'ctx>,
    ) -> R<()> {
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        self.store_io_result_at(d, dty, code, value, msg, None)
    }

    /// [`Self::store_io_result`] at `d`, a `Result` of type `dty`; `bytes`
    /// are where an `Output`'s stdout and stderr were left.
    fn store_io_result_at(
        &mut self,
        d: PointerValue<'ctx>,
        dty: Ty,
        code: IntValue<'ctx>,
        value: PointerValue<'ctx>,
        msg: PointerValue<'ctx>,
        bytes: Option<(PointerValue<'ctx>, PointerValue<'ctx>)>,
    ) -> R<()> {
        let (ok, er) = (self.variant_of(dty, "Ok")?, self.variant_of(dty, "Err")?);
        let okt = self
            .tys
            .field_ty(dty, Some(ok), 0)
            .ok_or("Result.Ok has no payload")?;
        let et = self
            .tys
            .field_ty(dty, Some(er), 0)
            .ok_or("Result.Err has no payload")?;
        let ok_off = self.field_offset(dty, Some(ok), 0)?;
        let ep = self.offset(d, self.field_offset(dty, Some(er), 0)?)?;
        let i32t = self.llcx.i32_type();
        let done = self
            .builder
            .build_int_compare(IntPredicate::SLT, code, i32t.const_zero(), "")
            .map_err(e)?;
        self.store_tag(d, dty, er)?;
        self.if_then(done, |cx| {
            cx.store_tag(d, dty, ok)?;
            let q = cx.offset(d, ok_off)?;
            cx.store_io_ok(q, okt, value, msg, bytes)
        })?;
        for (k, name) in IO_ERRORS.iter().enumerate() {
            let v = self.variant_of(et, name)?;
            let is = self
                .builder
                .build_int_compare(IntPredicate::EQ, code, i32t.const_int(k as u64, false), "")
                .map_err(e)?;
            let other = *name == "Other";
            let off = if other {
                self.field_offset(et, Some(v), 0)?
            } else {
                0
            };
            self.if_then(is, |cx| {
                cx.store_tag(ep, et, v)?;
                if other {
                    let s = cx.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
                    cx.copy(cx.offset(ep, off)?, msg, s)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Writes the `Ok` payload of type `okt` at `q` from what the runtime
    /// left: a `String` in `msg`, or a word in `value`, which for an
    /// `ExitStatus` (in an `Option`, or an `Output` with the bytes at
    /// `bytes`) is packed as `mir_process.rs` packs it.
    fn store_io_ok(
        &mut self,
        q: PointerValue<'ctx>,
        okt: Ty,
        value: PointerValue<'ctx>,
        msg: PointerValue<'ctx>,
        bytes: Option<(PointerValue<'ctx>, PointerValue<'ctx>)>,
    ) -> R<()> {
        if matches!(self.tys.kind(okt), TyKind::Intrinsic(IntrinsicTy::String)) {
            return self.copy(q, msg, okt);
        }
        // A view the entry left as a pointer and a length in `value[0..2]`.
        if matches!(self.tys.kind(okt), TyKind::Slice(_)) {
            return self.copy(q, value, okt);
        }
        let name = self.tys.tcx().adt_of(okt).map(|(adt, _)| adt.name.clone());
        let v = self.load_i64(value)?;
        match name.as_deref() {
            Some("ExitStatus") => self.store_exit(q, okt, v),
            Some("Output") => {
                let (out, err) = bytes.ok_or("an Output without its bytes")?;
                for (n, from) in [("stdout", out), ("stderr", err)] {
                    let (p, t) = self.named_field(q, okt, n)?;
                    self.copy(p, from, t)?;
                }
                let (p, t) = self.named_field(q, okt, "status")?;
                self.store_exit(p, t, v)
            }
            // The status with its low bit set when present.
            Some("Option") => {
                let (some, none) = self.option_variants(okt)?;
                let st = self
                    .tys
                    .field_ty(okt, Some(some), 0)
                    .ok_or("an option without a payload")?;
                let off = self.field_offset(okt, Some(some), 0)?;
                let present = self.builder.build_and(v, self.i64(1), "").map_err(e)?;
                let present = self
                    .builder
                    .build_int_compare(IntPredicate::NE, present, self.i64(0), "")
                    .map_err(e)?;
                self.store_tag(q, okt, none)?;
                self.if_then(present, |cx| {
                    cx.store_tag(q, okt, some)?;
                    let p = cx.offset(q, off)?;
                    cx.store_exit(p, st, v)
                })
            }
            _ => match self.layout(okt)?.size {
                0 => Ok(()),
                8 => self.store(q, v.into()),
                _ => Err(format!("an I/O result of `{}`", self.tys.display(okt))),
            },
        }
    }

    /// The field `n` of the struct of type `ty` at `p`, and its type.
    fn named_field(&self, p: PointerValue<'ctx>, ty: Ty, n: &str) -> R<(PointerValue<'ctx>, Ty)> {
        let k = self
            .tys
            .tcx()
            .adt_of(ty)
            .and_then(|(adt, _)| adt.variants[0].fields.iter().position(|(f, _)| f == n))
            .ok_or_else(|| format!("`{}` has no field `{n}`", self.tys.display(ty)))?
            as u32;
        let t = self
            .tys
            .field_ty(ty, None, k)
            .ok_or_else(|| format!("`{n}` has no type"))?;
        Ok((self.offset(p, self.field_offset(ty, None, k)?)?, t))
    }

    /// An `ExitStatus { exited, exit_code, exit_signal }` at `p` from `v`,
    /// packed `(code << 16) | (signal << 2) | (exited << 1)`.
    fn store_exit(&mut self, p: PointerValue<'ctx>, ty: Ty, v: IntValue<'ctx>) -> R<()> {
        let code = self
            .builder
            .build_right_shift(v, self.i64(16), true, "")
            .map_err(e)?;
        let signal = self
            .builder
            .build_right_shift(v, self.i64(2), false, "")
            .map_err(e)?;
        let signal = self
            .builder
            .build_and(signal, self.i64(0x3fff), "")
            .map_err(e)?;
        let exited = self
            .builder
            .build_right_shift(v, self.i64(1), false, "")
            .map_err(e)?;
        let exited = self.builder.build_and(exited, self.i64(1), "").map_err(e)?;
        let exited = self
            .builder
            .build_int_truncate(exited, self.llcx.i8_type(), "")
            .map_err(e)?;
        let (q, _) = self.named_field(p, ty, "exited")?;
        self.store(q, exited.into())?;
        let (q, _) = self.named_field(p, ty, "exit_code")?;
        self.store(q, code.into())?;
        let (q, _) = self.named_field(p, ty, "exit_signal")?;
        self.store(q, signal.into())
    }

    /// `TcpListener` / `TcpStream`, through `karac_mir_tcp(op, fd, ptr, len,
    /// value)`, and their other methods and `UdpSocket`'s through
    /// `karac_mir_net(op, fd, ptr, len, addr, addr_len, value, out)`; the
    /// `NetError` cause comes back as its code, in the order [`NET_ERRORS`]
    /// lists, `Other`'s `i32` in `value`.
    fn tcp_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let op: u64 = match (base, method) {
            ("TcpListener", "bind") => 0,
            ("TcpStream", "connect") => 1,
            ("TcpListener", "accept") => 2,
            ("TcpStream", "try_clone") => 3,
            ("TcpStream", "shutdown_write") => 4,
            ("TcpStream", "read") => 5,
            ("TcpStream", "write") => 6,
            ("TcpStream", "write_all") => 7,
            _ => return self.net_native(base, method, args, dest),
        };
        let null = self.ptr_ty().const_null();
        let (fd, ptr, len) = if op <= 1 {
            let (p, n) = self.str_parts(&args[0])?;
            (self.i64(0), p, n)
        } else {
            let (at, _) = self.behind_refs(&args[0])?;
            let fd = self.load_i64(at)?;
            if op >= 5 {
                let (p, n) = self.view_of(&args[1])?;
                (fd, p, n)
            } else {
                (fd, null, self.i64(0))
            }
        };
        let value = self.entry_slot(8, "tcp.value")?;
        let i32t = self.llcx.i32_type();
        let code = self.rt_int(
            "tcp",
            &[
                i32t.const_int(op, false).into(),
                fd.into(),
                ptr.into(),
                len.into(),
                value.into(),
            ],
            32,
        )?;
        self.drop_moved(args)?;
        self.store_net_result(dest, code, value, None)?;
        Ok(Some(()))
    }

    /// The socket methods `karac_mir_net` runs (`tcp_native`).
    fn net_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let op: u64 = match (base, method) {
            ("TcpListener", "local_addr") => 0,
            ("TcpStream", "set_read_timeout") => 1,
            ("UdpSocket", "bind") => 2,
            ("UdpSocket", "send_to") => 3,
            ("UdpSocket", "recv_from") => 4,
            ("UdpSocket", "connect") => 5,
            ("UdpSocket", "send") => 6,
            ("UdpSocket", "recv") => 7,
            ("UdpSocket", "local_addr") => 8,
            ("UdpSocket", "set_read_timeout") => 9,
            ("UdpSocket", "try_clone") => 10,
            _ => return Ok(None),
        };
        let null = self.ptr_ty().const_null();
        let zero = self.i64(0);
        let (mut fd, mut ptr, mut len, mut addr, mut addr_len) = (zero, null, zero, null, zero);
        if op == 2 {
            (ptr, len) = self.str_parts(&args[0])?;
        } else {
            let (at, _) = self.behind_refs(&args[0])?;
            fd = self.load_i64(at)?;
        }
        match op {
            3 | 4 | 6 | 7 => (ptr, len) = self.view_of(&args[1])?,
            5 => (addr, addr_len) = self.str_parts(&args[1])?,
            1 | 9 => {
                // `Some(ms)` is `len` ms with `addr_len` 1; `None` is 0.
                let (at, t) = self.behind_refs(&args[1])?;
                let (some, _) = self.option_variants(t)?;
                let tag = self.load_tag(at, t)?;
                let is = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        tag,
                        tag.get_type().const_int(u64::from(some), false),
                        "",
                    )
                    .map_err(e)?;
                let ms = self.load_i64(self.offset(at, self.field_offset(t, Some(some), 0)?)?)?;
                let i64t = self.llcx.i64_type();
                addr_len = self.builder.build_int_z_extend(is, i64t, "").map_err(e)?;
                len = self
                    .builder
                    .build_select(is, ms, zero, "")
                    .map_err(e)?
                    .into_int_value();
            }
            _ => {}
        }
        if op == 3 {
            (addr, addr_len) = self.str_parts(&args[2])?;
        }
        let s = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let value = self.entry_slot(8, "net.value")?;
        let out = self.temp(s, "net.out")?;
        let i32t = self.llcx.i32_type();
        let code = self.rt_int(
            "net",
            &[
                i32t.const_int(op, false).into(),
                fd.into(),
                ptr.into(),
                len.into(),
                addr.into(),
                addr_len.into(),
                value.into(),
                out.into(),
            ],
            32,
        )?;
        self.drop_moved(args)?;
        self.store_net_result(dest, code, value, Some(out))?;
        Ok(Some(()))
    }

    /// Writes a socket call's `Result` at `dest` from the runtime's `code`
    /// (negative for `Ok`) and `value`: a socket is its descriptor, a count
    /// an `i64`, `Unit` nothing, and an address the `String` at `out`
    /// (`recv_from`'s `(i64, String)` takes both).
    fn store_net_result(
        &mut self,
        dest: &Place,
        code: IntValue<'ctx>,
        value: PointerValue<'ctx>,
        out: Option<PointerValue<'ctx>>,
    ) -> R<()> {
        let i32t = self.llcx.i32_type();
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        let (ok, er) = (self.variant_of(dty, "Ok")?, self.variant_of(dty, "Err")?);
        let okt = self
            .tys
            .field_ty(dty, Some(ok), 0)
            .ok_or("Result.Ok has no payload")?;
        let et = self
            .tys
            .field_ty(dty, Some(er), 0)
            .ok_or("Result.Err has no payload")?;
        let ok_off = self.field_offset(dty, Some(ok), 0)?;
        let ep = self.offset(d, self.field_offset(dty, Some(er), 0)?)?;
        let v = self.load_i64(value)?;
        let done = self
            .builder
            .build_int_compare(IntPredicate::SLT, code, i32t.const_zero(), "")
            .map_err(e)?;
        self.store_tag(d, dty, er)?;
        self.if_then(done, |cx| {
            cx.store_tag(d, dty, ok)?;
            let p = cx.offset(d, ok_off)?;
            let s = cx.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
            match (cx.tys.kind(okt), out) {
                (TyKind::Intrinsic(IntrinsicTy::String), Some(out)) => cx.copy(p, out, s),
                (TyKind::Tuple(ts), Some(out)) if ts.len() == 2 => {
                    cx.store(cx.offset(p, cx.field_offset(okt, None, 0)?)?, v.into())?;
                    cx.copy(cx.offset(p, cx.field_offset(okt, None, 1)?)?, out, s)
                }
                _ => match cx.layout(okt)?.size {
                    0 => Ok(()),
                    8 => cx.store(p, v.into()),
                    _ => Err(format!("a socket result of `{}`", cx.tys.display(okt))),
                },
            }
        })?;
        for (k, name) in NET_ERRORS.iter().enumerate() {
            let var = self.variant_of(et, name)?;
            let is = self
                .builder
                .build_int_compare(IntPredicate::EQ, code, i32t.const_int(k as u64, false), "")
                .map_err(e)?;
            let other = *name == "Other";
            let off = if other {
                self.field_offset(et, Some(var), 0)?
            } else {
                0
            };
            self.if_then(is, |cx| {
                cx.store_tag(ep, et, var)?;
                if other {
                    let c = cx
                        .builder
                        .build_int_truncate(v, cx.llcx.i32_type(), "")
                        .map_err(e)?;
                    cx.store(cx.offset(ep, off)?, c.into())?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// The `#[compiler_builtin]` `Drop` of the I/O type `name`, whose
    /// `mut ref` is in `slot`: `true` when it is one. A socket closes its
    /// descriptor.
    pub(super) fn io_drop(&mut self, name: &str, slot: PointerValue<'ctx>) -> R<bool> {
        if !matches!(name, "TcpListener" | "TcpStream" | "UdpSocket") {
            return Ok(false);
        }
        let v = self.load_ptr(slot)?;
        let fd = self.load_i64(v)?;
        let i64t = self.llcx.i64_type();
        let f = self.extern_fn(
            "karac_runtime_tcp_close",
            self.llcx.i32_type().fn_type(&[i64t.into()], false),
        );
        self.builder.build_call(f, &[fd.into()], "").map_err(e)?;
        Ok(true)
    }

    /// `std.http`'s client: `Client.get` / `post` / `request`, the
    /// `RequestBuilder` chain, and `Response` / `HttpError`'s readers, as the
    /// interpreter's `http_method`. Requests go through the runtime's
    /// client (`karac_runtime_http_client_mir_*`, in the full archive).
    fn http_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let i64t = self.llcx.i64_type();
        let ptr = self.ptr_ty();
        let null = ptr.const_null();
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        match (base, method) {
            ("Client", "new") => {}
            ("Client", "get" | "post") | ("RequestBuilder", "send") => {
                let (op, (a, an), (b, bn), h) = match (base, method) {
                    ("Client", "get") => (
                        0,
                        self.str_parts(&args[1])?,
                        (null, self.i64(0)),
                        self.i64(0),
                    ),
                    ("Client", _) => {
                        let url = self.str_parts(&args[1])?;
                        (1, url, self.str_parts(&args[2])?, self.i64(0))
                    }
                    _ => {
                        let (at, _) = self.behind_refs(&args[0])?;
                        let h = self.load_i64(at)?;
                        (2, (null, self.i64(0)), (null, self.i64(0)), h)
                    }
                };
                let status = self.entry_slot(8, "http.status")?;
                let out = self.temp(string, "http.out")?;
                let i32t = self.llcx.i32_type();
                let ok = self.rt_named(
                    "karac_runtime_http_client_mir_send",
                    &[
                        i32t.const_int(op, false).into(),
                        a.into(),
                        an.into(),
                        b.into(),
                        bn.into(),
                        h.into(),
                        status.into(),
                        out.into(),
                    ],
                    32,
                )?;
                self.drop_moved(args)?;
                let dty = self.place_type(dest)?;
                let d = self.place_ptr(dest)?;
                let (okv, erv) = (self.variant_of(dty, "Ok")?, self.variant_of(dty, "Err")?);
                let rt = self
                    .tys
                    .field_ty(dty, Some(okv), 0)
                    .ok_or("Result.Ok has no payload")?;
                let et = self
                    .tys
                    .field_ty(dty, Some(erv), 0)
                    .ok_or("Result.Err has no payload")?;
                let r = self.offset(d, self.field_offset(dty, Some(okv), 0)?)?;
                let er = self.offset(d, self.field_offset(dty, Some(erv), 0)?)?;
                let is_ok = self
                    .builder
                    .build_int_compare(IntPredicate::NE, ok, i32t.const_zero(), "")
                    .map_err(e)?;
                self.store_tag(d, dty, erv)?;
                self.copy(
                    self.offset(er, self.field_offset(et, None, 0)?)?,
                    out,
                    string,
                )?;
                self.if_then(is_ok, |cx| {
                    cx.store_tag(d, dty, okv)?;
                    let st = cx.load_i64(status)?;
                    cx.store(cx.offset(r, cx.field_offset(rt, None, 0)?)?, st.into())?;
                    cx.copy(cx.offset(r, cx.field_offset(rt, None, 1)?)?, out, string)
                })?;
            }
            ("Client", "request") => {
                let (m, mn) = self.str_parts(&args[1])?;
                let (u, un) = self.str_parts(&args[2])?;
                let h = self.rt_named(
                    "karac_runtime_http_builder_new",
                    &[m.into(), mn.into(), u.into(), un.into()],
                    64,
                )?;
                self.drop_moved(args)?;
                let d = self.place_ptr(dest)?;
                self.store(d, h.into())?;
            }
            ("RequestBuilder", "header" | "body" | "timeout") => {
                let (at, ty) = self.behind_refs(&args[0])?;
                let h = self.load_i64(at)?;
                let void = self.llcx.void_type();
                match method {
                    "header" => {
                        let (k, kn) = self.str_parts(&args[1])?;
                        let (v, vn) = self.str_parts(&args[2])?;
                        let f = self.extern_fn(
                            "karac_runtime_http_builder_add_header",
                            void.fn_type(
                                &[
                                    i64t.into(),
                                    ptr.into(),
                                    i64t.into(),
                                    ptr.into(),
                                    i64t.into(),
                                ],
                                false,
                            ),
                        );
                        self.builder
                            .build_call(
                                f,
                                &[h.into(), k.into(), kn.into(), v.into(), vn.into()],
                                "",
                            )
                            .map_err(e)?;
                    }
                    "body" => {
                        let (b, bn) = self.str_parts(&args[1])?;
                        let f = self.extern_fn(
                            "karac_runtime_http_builder_set_body",
                            void.fn_type(&[i64t.into(), ptr.into(), i64t.into()], false),
                        );
                        self.builder
                            .build_call(f, &[h.into(), b.into(), bn.into()], "")
                            .map_err(e)?;
                    }
                    _ => {
                        let ms = self.operand_ptr(&args[1])?;
                        let ms = self.load_i64(ms)?;
                        let f = self.extern_fn(
                            "karac_runtime_http_builder_set_timeout",
                            void.fn_type(&[i64t.into(), i64t.into()], false),
                        );
                        self.builder
                            .build_call(f, &[h.into(), ms.into()], "")
                            .map_err(e)?;
                    }
                }
                // The builder goes on as the same request.
                self.drop_moved(&args[1..])?;
                let d = self.place_ptr(dest)?;
                self.copy(d, at, ty)?;
            }
            ("Response", "status") => {
                let (at, ty) = self.behind_refs(&args[0])?;
                let v = self.load_i64(self.offset(at, self.field_offset(ty, None, 0)?)?)?;
                let d = self.place_ptr(dest)?;
                self.store(d, v.into())?;
            }
            // `bytes` is the body's bytes as a `Vec[u8]`, which a `String`
            // is laid out as.
            ("Response", "body" | "bytes") | ("HttpError", "message") => {
                let (at, ty) = self.behind_refs(&args[0])?;
                let k = u32::from(base == "Response");
                let src = self.offset(at, self.field_offset(ty, None, k)?)?;
                let d = self.place_ptr(dest)?;
                self.clone_into(d, src, string)?;
            }
            ("Response", "header" | "headers") => {
                let (at, ty) = self.behind_refs(&args[0])?;
                let body = self.load_ptr(self.offset(at, self.field_offset(ty, None, 1)?)?)?;
                let d = self.place_ptr(dest)?;
                if method == "headers" {
                    let f = self.extern_fn(
                        "karac_runtime_http_client_mir_headers",
                        self.llcx
                            .void_type()
                            .fn_type(&[ptr.into(), ptr.into()], false),
                    );
                    self.builder
                        .build_call(f, &[body.into(), d.into()], "")
                        .map_err(e)?;
                } else {
                    let (n, nn) = self.str_parts(&args[1])?;
                    let out = self.temp(string, "http.header")?;
                    let found = self.rt_named(
                        "karac_runtime_http_client_mir_header",
                        &[body.into(), n.into(), nn.into(), out.into()],
                        32,
                    )?;
                    self.drop_moved(&args[1..])?;
                    let dty = self.place_type(dest)?;
                    let (some, none) = self.option_variants(dty)?;
                    let off = self.field_offset(dty, Some(some), 0)?;
                    let hit = self
                        .builder
                        .build_int_compare(
                            IntPredicate::NE,
                            found,
                            self.llcx.i32_type().const_zero(),
                            "",
                        )
                        .map_err(e)?;
                    self.store_tag(d, dty, none)?;
                    self.if_then(hit, |cx| {
                        cx.store_tag(d, dty, some)?;
                        cx.copy(cx.offset(d, off)?, out, string)
                    })?;
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// `Server.serve(addr, handler)`: binds through `karac_mir_http_bind`,
    /// then answers each request `karac_mir_http_next` waits for with the
    /// handler's `Response` until the server can accept no more, which is
    /// the `Err` it returns (as is a failed bind). The `Response` is read by
    /// field name, so a program's own `Response` with `headers: Vec[(String,
    /// String)]` sets headers.
    fn serve_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let ptr = self.ptr_ty();
        let i64t = self.llcx.i64_type();
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let (a, an) = self.str_parts(&args[0])?;
        let msg = self.temp(string, "serve.msg")?;
        let bind = self.extern_fn(
            "karac_mir_http_bind",
            ptr.fn_type(&[ptr.into(), i64t.into(), ptr.into()], false),
        );
        let server = self
            .builder
            .build_call(bind, &[a.into(), an.into(), msg.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.drop_moved(&args[..1])?;

        let c = self.callee_by_ref(&args[1])?;
        let want = match &c {
            Callee::Body { name, env } => {
                self.program.bodies[name].locals[1 + usize::from(env.is_some())].ty
            }
            Callee::Erased { ty, .. } => match self.tys.kind(*ty) {
                TyKind::Fn { params, .. } if !params.is_empty() => params[0],
                _ => return Err("Server.serve with a handler taking no Request".into()),
            },
        };
        let req_ty = match self.tys.kind(want) {
            TyKind::Ref(i) | TyKind::MutRef(i) => i,
            _ => want,
        };
        let req = self.temp(req_ty, "serve.req")?;
        let rt = self.callee_ret(&c)?;
        let resp = self.temp(rt, "serve.resp")?;
        let TyKind::Adt(adt) = self.tys.kind(rt) else {
            return Err("Server.serve with a handler returning no Response".into());
        };
        let names: Vec<String> = self.tys.adt(adt).variants[0]
            .fields
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        let at = |n: &str| names.iter().position(|f| f == n).map(|k| k as u32);
        let status_k = at("status").ok_or("a Response without a `status`")?;
        let (body_k, headers_k) = (at("body"), at("headers"));

        let f = self.cur_fn()?;
        let head = self.llcx.append_basic_block(f, "serve.next");
        let each = self.llcx.append_basic_block(f, "serve.answer");
        let done = self.llcx.append_basic_block(f, "serve.done");
        let failed = self.builder.build_is_null(server, "").map_err(e)?;
        self.builder
            .build_conditional_branch(failed, done, head)
            .map_err(e)?;
        self.builder.position_at_end(head);
        let got = self.rt_named("karac_mir_http_next", &[server.into(), msg.into()], 32)?;
        let more = self
            .builder
            .build_int_compare(IntPredicate::NE, got, self.llcx.i32_type().const_zero(), "")
            .map_err(e)?;
        self.builder
            .build_conditional_branch(more, each, done)
            .map_err(e)?;

        self.builder.position_at_end(each);
        self.call_callee(&c, resp, &[(req, req_ty)])?;
        let status = self.load_i64(self.offset(resp, self.field_offset(rt, None, status_k)?)?)?;
        // A `String` and a `Vec` both start with their buffer and length.
        let parts = |k: Option<u32>| -> R<(PointerValue<'ctx>, IntValue<'ctx>)> {
            match k {
                Some(k) => {
                    let p = self.offset(resp, self.field_offset(rt, None, k)?)?;
                    Ok((self.load_ptr(p)?, self.load_i64(self.offset(p, 8)?)?))
                }
                None => Ok((ptr.const_null(), self.i64(0))),
            }
        };
        let (b, bn) = parts(body_k)?;
        let (h, hn) = parts(headers_k)?;
        let respond = self.extern_fn(
            "karac_mir_http_respond",
            self.llcx.void_type().fn_type(
                &[
                    ptr.into(),
                    i64t.into(),
                    ptr.into(),
                    i64t.into(),
                    ptr.into(),
                    i64t.into(),
                ],
                false,
            ),
        );
        self.builder
            .build_call(
                respond,
                &[
                    server.into(),
                    status.into(),
                    b.into(),
                    bn.into(),
                    h.into(),
                    hn.into(),
                ],
                "",
            )
            .map_err(e)?;
        self.call_glue(resp, rt)?;
        self.builder.build_unconditional_branch(head).map_err(e)?;

        self.builder.position_at_end(done);
        if !Self::consumes(&c) {
            self.drop_moved(&args[1..])?;
        }
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        let erv = self.variant_of(dty, "Err")?;
        let et = self
            .tys
            .field_ty(dty, Some(erv), 0)
            .ok_or("Result.Err has no payload")?;
        self.store_tag(d, dty, erv)?;
        let er = self.offset(d, self.field_offset(dty, Some(erv), 0)?)?;
        self.copy(
            self.offset(er, self.field_offset(et, None, 0)?)?,
            msg,
            string,
        )
    }

    /// `Server.serve_static(addr, body)`: `serve` with a 200 and `body` as
    /// every answer, as the interpreter's.
    fn serve_static_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let ptr = self.ptr_ty();
        let i64t = self.llcx.i64_type();
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let (a, an) = self.str_parts(&args[0])?;
        let (b, bn) = self.str_parts(&args[1])?;
        let msg = self.temp(string, "serve.msg")?;
        let bind = self.extern_fn(
            "karac_mir_http_bind",
            ptr.fn_type(&[ptr.into(), i64t.into(), ptr.into()], false),
        );
        let server = self
            .builder
            .build_call(bind, &[a.into(), an.into(), msg.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let f = self.cur_fn()?;
        let head = self.llcx.append_basic_block(f, "serve.next");
        let each = self.llcx.append_basic_block(f, "serve.answer");
        let done = self.llcx.append_basic_block(f, "serve.done");
        let failed = self.builder.build_is_null(server, "").map_err(e)?;
        self.builder
            .build_conditional_branch(failed, done, head)
            .map_err(e)?;
        self.builder.position_at_end(head);
        let got = self.rt_named("karac_mir_http_next", &[server.into(), msg.into()], 32)?;
        let more = self
            .builder
            .build_int_compare(IntPredicate::NE, got, self.llcx.i32_type().const_zero(), "")
            .map_err(e)?;
        self.builder
            .build_conditional_branch(more, each, done)
            .map_err(e)?;
        self.builder.position_at_end(each);
        let respond = self.extern_fn(
            "karac_mir_http_respond",
            self.llcx.void_type().fn_type(
                &[
                    ptr.into(),
                    i64t.into(),
                    ptr.into(),
                    i64t.into(),
                    ptr.into(),
                    i64t.into(),
                ],
                false,
            ),
        );
        self.builder
            .build_call(
                respond,
                &[
                    server.into(),
                    self.i64(200).into(),
                    b.into(),
                    bn.into(),
                    ptr.const_null().into(),
                    self.i64(0).into(),
                ],
                "",
            )
            .map_err(e)?;
        self.builder.build_unconditional_branch(head).map_err(e)?;
        self.builder.position_at_end(done);
        self.drop_moved(args)?;
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        let erv = self.variant_of(dty, "Err")?;
        let et = self
            .tys
            .field_ty(dty, Some(erv), 0)
            .ok_or("Result.Err has no payload")?;
        self.store_tag(d, dty, erv)?;
        let er = self.offset(d, self.field_offset(dty, Some(erv), 0)?)?;
        self.copy(
            self.offset(er, self.field_offset(et, None, 0)?)?,
            msg,
            string,
        )
    }

    /// `Request`'s methods, on the request `Server.serve` is answering,
    /// through `karac_mir_http_request(op, name, len, out)`.
    fn request_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let op: u64 = match method {
            "path" => 0,
            "method" => 1,
            "body" => 2,
            "header" => 3,
            "headers" => 4,
            "query" => 5,
            _ => return Ok(None),
        };
        let i32t = self.llcx.i32_type();
        let op = i32t.const_int(op, false);
        let d = self.place_ptr(dest)?;
        if method != "header" {
            let null = self.ptr_ty().const_null();
            self.rt_named(
                "karac_mir_http_request",
                &[op.into(), null.into(), self.i64(0).into(), d.into()],
                32,
            )?;
            return Ok(Some(()));
        }
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let (n, nn) = self.str_parts(&args[1])?;
        let out = self.temp(string, "request.header")?;
        let found = self.rt_named(
            "karac_mir_http_request",
            &[op.into(), n.into(), nn.into(), out.into()],
            32,
        )?;
        self.drop_moved(&args[1..])?;
        let dty = self.place_type(dest)?;
        let (some, none) = self.option_variants(dty)?;
        let off = self.field_offset(dty, Some(some), 0)?;
        let hit = self
            .builder
            .build_int_compare(IntPredicate::NE, found, i32t.const_zero(), "")
            .map_err(e)?;
        self.store_tag(d, dty, none)?;
        self.if_then(hit, |cx| {
            cx.store_tag(d, dty, some)?;
            cx.copy(cx.offset(d, off)?, out, string)
        })?;
        Ok(Some(()))
    }

    /// `std.regex`'s engine natives (runtime/stdlib/source/regex.kara,
    /// B-2026-10-14-63) through `karac_regex_mir(op, pat, pat_len, s, s_len,
    /// rep, rep_len, a, b, slots, n, out)`, which lives in the opt-in regex
    /// archive; the interpreter's `regex_method` makes the same calls.
    fn regex_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let op: u64 = match method {
            "check" => 0,
            "groups" => 1,
            "group_name" => 2,
            "find_from" => 3,
            "replace" => 4,
            "escape" => 5,
            _ => return Ok(None),
        };
        let null = self.ptr_ty().const_null();
        let zero = self.i64(0);
        let (pat, pat_len) = self.str_parts(&args[0])?;
        let (mut s, mut s_len, mut rep, mut rep_len) = (null, zero, null, zero);
        let (mut a, mut b, mut slots, mut n) = (zero, zero, null, zero);
        match op {
            2 => a = self.operand_scalar(&args[1])?.into_int_value(),
            3 => {
                (s, s_len) = self.str_parts(&args[1])?;
                a = self.operand_scalar(&args[2])?.into_int_value();
                b = self.operand_scalar(&args[3])?.into_int_value();
                (slots, n) = self.view_of(&args[4])?;
            }
            4 => {
                (s, s_len) = self.str_parts(&args[1])?;
                (rep, rep_len) = self.str_parts(&args[2])?;
                a = self.operand_scalar(&args[3])?.into_int_value();
            }
            _ => {}
        }
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let out = self.temp(string, "regex.out")?;
        let i32t = self.llcx.i32_type();
        let v = self.rt_named(
            "karac_regex_mir",
            &[
                i32t.const_int(op, false).into(),
                pat.into(),
                pat_len.into(),
                s.into(),
                s_len.into(),
                rep.into(),
                rep_len.into(),
                a.into(),
                b.into(),
                slots.into(),
                n.into(),
                out.into(),
            ],
            64,
        )?;
        self.drop_moved(args)?;
        let d = self.place_ptr(dest)?;
        match op {
            1 => self.store(d, v.into())?,
            3 => {
                let found = self
                    .builder
                    .build_int_truncate(v, self.llcx.i8_type(), "")
                    .map_err(e)?;
                self.store(d, found.into())?;
            }
            _ => self.copy(d, out, string)?,
        }
        Ok(Some(()))
    }

    /// Calls the runtime's `name(args)`, which returns an int of `bits`.
    fn rt_named(
        &mut self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
        bits: u32,
    ) -> R<IntValue<'ctx>> {
        let tys = self.arg_tys(args);
        let f = self.extern_fn(name, self.int_ty(bits).fn_type(&tys, false));
        let v = self.builder.build_call(f, args, "").map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
    }
}
