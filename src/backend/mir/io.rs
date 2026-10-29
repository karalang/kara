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

use super::{Cx, R};
use crate::mir::ty::IntrinsicTy;
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

/// The `IoError` variants by the runtime's code for them.
pub(super) const IO_ERRORS: [&str; 10] = [
    "NotFound",
    "PermissionDenied",
    "AlreadyExists",
    "UnexpectedEof",
    "InvalidUtf8",
    "Interrupted",
    "Other",
    "InvalidInput",
    "Unsupported",
    "ProviderUnavailable",
];

/// The `NetError` variants by the runtime's code for them.
const NET_ERRORS: [&str; 10] = [
    "Interrupted",
    "Other",
    "AddrInUse",
    "ConnectionRefused",
    "PermissionDenied",
    "TimedOut",
    "WouldBlock",
    "InvalidInput",
    "Tls",
    "Protocol",
];

/// `TlsFailure`'s variants, in the order `src/tls_conn.rs` numbers them.
const TLS_FAILURES: [&str; 4] = ["Handshake", "BadCertificate", "Protocol", "UnexpectedClose"];

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
            (
                "OsEnv" | "OsClock" | "OsRandom" | "OsStdout" | "OsStderr" | "OsStdin"
                | "OsFileSystem",
                _,
            ) if !args.is_empty() => {
                let (resource, method) = crate::mir::providers::os_default(base, method)
                    .expect("a library default provider");
                self.native(&format!("{resource}.{method}"), &args[1..], dest)
                    .map(Some)
            }
            ("FileSystem", "enter" | "exit") => self.fs_extent_native(method, args, dest).map(Some),
            ("File", "adopt") => self.adopt_native(args, dest).map(Some),
            ("File", "options") => self.options_native(method, args, dest),
            ("File", _) => self.file_native(method, args, dest),
            ("OpenOptions", "open") => self.file_native("options_open", args, dest),
            ("OpenOptions", _) => self.options_native(method, args, dest),
            ("BufReader" | "BufWriter" | "LinesIter", _) => {
                self.buf_native(base, method, args, dest)
            }
            ("Stdio", _) => self.stdio_native(method, args, dest),
            ("Env", "var") if args.len() == 1 => self.env_var_native(args, dest).map(Some),
            ("Stdin" | "StdinLines" | "Stdout" | "Stderr", _) => {
                self.std_stream_native(base, method, args, dest)
            }
            ("Command", "spawn" | "output") => self.command_native(method, args, dest).map(Some),
            ("Child", _) => self.child_native(method, args, dest),
            ("ChildStdout" | "ChildStderr" | "ChildStdin", _) => {
                self.pipe_native(method, args, dest)
            }
            ("TcpListener" | "TcpStream" | "UdpSocket", _) => {
                self.tcp_native(base, method, args, dest)
            }
            ("TlsListener" | "TlsStream", _) => self.tls_native(base, method, args, dest),
            ("WebSocket", "__accept_key") => self.ws_accept_native(args, dest).map(Some),
            ("Client" | "RequestBuilder" | "Response" | "HttpError", _) => {
                self.http_native(base, method, args, dest)
            }
            ("RegexEngine", _) => self.regex_native(method, args, dest),
            ("StableHash", "siphash24") => self.siphash_native(args, dest).map(Some),
            ("String" | "str", "normalize") => self.normalize_native(args, dest).map(Some),
            ("CryptoSha2" | "CryptoHmac" | "CryptoLegacy" | "CryptoCt", _) => {
                self.crypto_native(base, method, args, dest)
            }
            ("Server", "__install") => self.http_install_native(args, dest).map(Some),
            ("Server", "__wire") => self.http_wire_native(args, dest).map(Some),
            ("Server", "__wire_fixed") => self.http_wire_fixed_native(args, dest).map(Some),
            ("Server", "__keep_alive") => {
                let keep = self.rt_named("karac_mir_http_keep_alive", &[], 8)?;
                let d = self.place_ptr(dest)?;
                self.store(d, keep.into()).map(Some)
            }
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
            "options_open" | "open_flags" => 10,
            _ => return Ok(None),
        };
        let null = self.ptr_ty().const_null();
        let (handle, ptr, len, n) = if op <= 2 {
            let (p, n) = self.str_parts(&args[0])?;
            (null, p, n, self.i64(0))
        } else if method == "open_flags" {
            // The path, then the options' bits, in a program that overrides
            // `FileSystem` somewhere (`providers::route_files`).
            let (p, n) = self.str_parts(&args[0])?;
            let flags = self.operand_ptr(&args[1])?;
            let flags = self.load_i64(flags)?;
            (null, p, n, flags)
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

    /// `FileSystem.enter(&mut p, read, write, close, raw_read, raw_write)`
    /// and `FileSystem.exit(id)` around a `with_provider[FileSystem]` call
    /// (core semantics §14.11): the runtime's table of the providers whose
    /// calls run, which a `File` one of them opened is routed through. The
    /// runtime calls the provider's `close`, `__read` and `__write` by
    /// their bodies' pointer convention (a result slot, then a slot per
    /// argument), so none of them may suspend.
    fn fs_extent_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<()> {
        if method == "exit" {
            let id = self.operand_ptr(&args[0])?;
            let id = self.load_i64(id)?;
            return self.rt("fs_exit", &[id.into()]);
        }
        let at = self.operand_ptr(&args[0])?;
        let provider = self.load_ptr(at)?;
        let mut call = vec![provider.into()];
        for a in &args[3..6] {
            let Operand::Const(crate::mir::Const {
                kind: crate::mir::ConstKind::FnDef(inst),
                ..
            }) = a
            else {
                return Err("FileSystem.enter takes the provider's methods".into());
            };
            if self
                .program
                .bodies
                .contains_key(&crate::mir::coroutine::dstart_name(&inst.name))
            {
                return Err(format!(
                    "`{}` may suspend, and a file's provider method must run to completion \
                     (core semantics §14.11)",
                    inst.name
                ));
            }
            let f = self.function(&inst.name)?;
            call.push(f.as_global_value().as_pointer_value().into());
        }
        let id = self.rt_int("fs_enter", &call, 64)?;
        let d = self.place_ptr(dest)?;
        self.store(d, id.into())
    }

    /// `File.adopt(&mut p, r)`: the `Result` of the provider `p`'s `open`
    /// made into a `File` of `p`'s running call, by the runtime's id for it
    /// in place of the provider's own. `Result[i64, IoError]` and
    /// `Result[File, IoError]` have one layout, the `File` a word.
    fn adopt_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let at = self.operand_ptr(&args[0])?;
        let provider = self.load_ptr(at)?;
        let (src, st) = (self.operand_ptr(&args[1])?, self.operand_type(&args[1])?);
        let (d, dt) = (self.place_ptr(dest)?, self.place_type(dest)?);
        let (sl, dl) = (self.layout(st)?, self.layout(dt)?);
        let (sok, dok) = (self.variant_of(st, "Ok")?, self.variant_of(dt, "Ok")?);
        let (so, dof) = (
            self.field_offset(st, Some(sok), 0)?,
            self.field_offset(dt, Some(dok), 0)?,
        );
        if sl.size != dl.size || so != dof || sok != dok {
            return Err("File.adopt of a `Result` laid out apart from its `File`'s".into());
        }
        self.copy(d, src, st)?;
        let tag = self.load_tag(src, st)?;
        let is_ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                tag.get_type().const_int(u64::from(sok), false),
                "",
            )
            .map_err(e)?;
        self.if_then(is_ok, |cx| {
            let h = cx.offset(src, so)?;
            let h = cx.load_i64(h)?;
            let word = cx.rt_int("fs_adopt", &[provider.into(), h.into()], 64)?;
            let at = cx.offset(d, dof)?;
            cx.store(at, word.into())
        })
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
        self.store_next_line(dest, code, value, msg)?;
        Ok(Some(()))
    }

    /// A line iterator's `next` at `dest` from the entry's report: `None`
    /// at the end (`-2`), else `Some` of the line's `Result`.
    fn store_next_line(
        &mut self,
        dest: &Place,
        code: IntValue<'ctx>,
        value: PointerValue<'ctx>,
        msg: PointerValue<'ctx>,
    ) -> R<()> {
        let i32t = self.llcx.i32_type();
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
        })
    }

    /// `Env.var(name)`: `Ok(value)`, or `Err` of `NotPresent` for an unset
    /// variable and `NotUnicode` for a value that is not UTF-8, through
    /// `karac_mir_env_var(out, ptr, len)`.
    fn env_var_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let (p, n) = self.str_parts(&args[0])?;
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        let (ok, er) = (self.variant_of(dty, "Ok")?, self.variant_of(dty, "Err")?);
        let q = self.offset(d, self.field_offset(dty, Some(ok), 0)?)?;
        let et = self
            .tys
            .field_ty(dty, Some(er), 0)
            .ok_or("Result.Err has no payload")?;
        let ep = self.offset(d, self.field_offset(dty, Some(er), 0)?)?;
        let code = self.rt_int("env_var", &[q.into(), p.into(), n.into()], 32)?;
        let i32t = self.llcx.i32_type();
        self.store_tag(d, dty, er)?;
        for (k, name) in [(1, "NotPresent"), (2, "NotUnicode")] {
            let v = self.variant_of(et, name)?;
            let is = self
                .builder
                .build_int_compare(IntPredicate::EQ, code, i32t.const_int(k, false), "")
                .map_err(e)?;
            self.if_then(is, |cx| cx.store_tag(ep, et, v))?;
        }
        let found = self
            .builder
            .build_int_compare(IntPredicate::EQ, code, i32t.const_zero(), "")
            .map_err(e)?;
        self.if_then(found, |cx| cx.store_tag(d, dty, ok))?;
        self.drop_moved(args)
    }

    /// The standard streams' reads and flushes, through
    /// `karac_mir_std_stream(op, value, msg)`; `stdin.lines()` is an empty
    /// marker, so making one does nothing.
    fn std_stream_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let op: u64 = match (base, method) {
            ("Stdin", "lines") => return self.drop_moved(args).map(Some),
            ("Stdin", "read_line") => 0,
            ("Stdin", "read_to_string") => 1,
            ("StdinLines", "next") => 2,
            ("Stdout", "flush") if args.is_empty() => 3,
            ("Stderr", "flush") if args.is_empty() => 4,
            _ => return Ok(None),
        };
        let opv = self.llcx.i32_type().const_int(op, false);
        if op >= 3 {
            let value = self.entry_slot(8, "io.value")?;
            let msg = self.temp(
                self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String)),
                "io.msg",
            )?;
            self.rt_int("std_stream", &[opv.into(), value.into(), msg.into()], 32)?;
            return Ok(Some(()));
        }
        if op <= 1 {
            self.io_call("std_stream", &[opv.into()], args, dest)?;
            return Ok(Some(()));
        }
        let value = self.entry_slot(8, "io.value")?;
        let msg = self.temp(
            self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String)),
            "io.msg",
        )?;
        let code = self.rt_int("std_stream", &[opv.into(), value.into(), msg.into()], 32)?;
        self.drop_moved(args)?;
        self.store_next_line(dest, code, value, msg)?;
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
        self.bytes_call("command", &vals, dest, false)
    }

    /// Calls the fallible `karac_mir_{name}(args.., value, out, err, msg)`
    /// and writes its `Result` at `dest`, an `Output` built from the bytes
    /// it left at `out` and `err` when that is the payload; `retried` when
    /// a readiness loop calls it again after `IO_NOT_READY`.
    fn bytes_call(
        &mut self,
        name: &str,
        call_args: &[BasicMetadataValueEnum<'ctx>],
        dest: &Place,
        retried: bool,
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
        if !retried {
            return self.store_io_result_at(d, dty, code, value, msg, Some((out, err)));
        }
        // A wait in a readiness loop that found the child still running
        // stored nothing.
        let done = self.io_done(code)?;
        self.if_then(done, |cx| {
            cx.store_io_result_at(d, dty, code, value, msg, Some((out, err)))
        })
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
            self.bytes_call("child", &call, dest, op == 0)?;
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
        // A call that found its descriptor not ready after all did nothing:
        // the readiness loop calls it again with the same arguments.
        let done = self.io_done(code)?;
        self.if_then(done, |cx| {
            cx.drop_moved(args)?;
            cx.store_net_result(dest, code, value, None)
        })?;
        Ok(Some(()))
    }

    /// Whether a socket call's `code` is its result, not the reactor
    /// contract's `IO_NOT_READY` (100) or `IO_IN_PROGRESS` (101), after which
    /// the call stored nothing and the readiness loop retries it.
    fn io_done(&mut self, code: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        let i32t = self.llcx.i32_type();
        let off = self
            .builder
            .build_int_sub(code, i32t.const_int(100, false), "")
            .map_err(e)?;
        self.builder
            .build_int_compare(IntPredicate::UGT, off, i32t.const_int(1, false), "")
            .map_err(e)
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
            1 | 9 => (len, addr_len) = self.timeout_ms(&args[1])?,
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
        let done = self.io_done(code)?;
        self.if_then(done, |cx| {
            cx.drop_moved(args)?;
            cx.store_net_result(dest, code, value, Some(out))
        })?;
        Ok(Some(()))
    }

    /// A read timeout's `Option[i64]` as the runtime takes it: the
    /// milliseconds, and 1 when it is `Some` (0 for `None`).
    fn timeout_ms(&mut self, arg: &Operand) -> R<(IntValue<'ctx>, IntValue<'ctx>)> {
        let (at, t) = self.behind_refs(arg)?;
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
        let present = self.builder.build_int_z_extend(is, i64t, "").map_err(e)?;
        let ms = self
            .builder
            .build_select(is, ms, self.i64(0), "")
            .map_err(e)?
            .into_int_value();
        Ok((ms, present))
    }

    /// `TlsListener` / `TlsStream` through `karac_mir_tls(op, fd, ptr, len,
    /// a, a_len, b, b_len, value)` (`runtime/src/mir_tls.rs`), its results
    /// as `karac_mir_tcp`'s, with `Tls`'s `TlsFailure` in `value`.
    /// `TlsListener.local_addr` is a `TcpListener`'s.
    fn tls_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let op: u64 = match (base, method) {
            ("TlsListener", "bind_tls") => 0,
            ("TlsListener", "accept") => 1,
            ("TlsStream", "read") => 2,
            ("TlsStream", "write") => 3,
            ("TlsStream", "shutdown_write") => 4,
            ("TlsStream", "set_read_timeout") => 5,
            ("TlsListener", "local_addr") => {
                return self.net_native("TcpListener", method, args, dest)
            }
            _ => return Ok(None),
        };
        let null = self.ptr_ty().const_null();
        let zero = self.i64(0);
        let (mut fd, mut ptr, mut len, mut a, mut a_len, mut b, mut b_len) =
            (zero, null, zero, null, zero, null, zero);
        if op == 0 {
            (ptr, len) = self.str_parts(&args[0])?;
            (a, a_len) = self.str_parts(&args[1])?;
            (b, b_len) = self.str_parts(&args[2])?;
        } else {
            let (at, _) = self.behind_refs(&args[0])?;
            fd = self.load_i64(at)?;
        }
        match op {
            2 | 3 => (ptr, len) = self.view_of(&args[1])?,
            5 => (len, a_len) = self.timeout_ms(&args[1])?,
            _ => {}
        }
        let value = self.entry_slot(8, "tls.value")?;
        let i32t = self.llcx.i32_type();
        let code = self.rt_int(
            "tls",
            &[
                i32t.const_int(op, false).into(),
                fd.into(),
                ptr.into(),
                len.into(),
                a.into(),
                a_len.into(),
                b.into(),
                b_len.into(),
                value.into(),
            ],
            32,
        )?;
        self.drop_moved(args)?;
        self.store_net_result(dest, code, value, None)?;
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
            let tls = *name == "Tls";
            let off = if other || tls {
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
                if tls {
                    // The failure, by its code in `value`.
                    let ft = cx
                        .tys
                        .field_ty(et, Some(var), 0)
                        .ok_or("NetError.Tls has no payload")?;
                    let fp = cx.offset(ep, off)?;
                    for (k, f) in TLS_FAILURES.iter().enumerate() {
                        let fv = cx.variant_of(ft, f)?;
                        let is = cx
                            .builder
                            .build_int_compare(IntPredicate::EQ, v, cx.i64(k as u64), "")
                            .map_err(e)?;
                        cx.if_then(is, |cx| cx.store_tag(fp, ft, fv))?;
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// The `#[compiler_builtin]` `Drop` of the I/O type `name`, whose
    /// `mut ref` is in `slot`: `true` when it is one. A socket closes its
    /// descriptor, leaving the reactor first (`karac_mir_sock_close`).
    pub(super) fn io_drop(&mut self, name: &str, slot: PointerValue<'ctx>) -> R<bool> {
        let close = match name {
            "TcpListener" | "TcpStream" | "UdpSocket" => "karac_mir_sock_close",
            // A stream sends `close_notify` first (`mir_tls.rs`).
            "TlsListener" | "TlsStream" => "karac_mir_tls_close",
            _ => return Ok(false),
        };
        let v = self.load_ptr(slot)?;
        let fd = self.load_i64(v)?;
        let i64t = self.llcx.i64_type();
        let f = self.extern_fn(close, self.llcx.void_type().fn_type(&[i64t.into()], false));
        self.builder.build_call(f, &[fd.into()], "").map_err(e)?;
        Ok(true)
    }

    /// The request a `Client.get` / `post` or `RequestBuilder.send` with
    /// `args` makes, as `karac_runtime_http_client_mir_send` takes it: its
    /// op, the URL and the body, and the builder's handle. `__ready` of the
    /// call reads it the same way (`task.rs`).
    #[allow(clippy::type_complexity)]
    pub(super) fn http_request(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
    ) -> R<(
        u64,
        (PointerValue<'ctx>, IntValue<'ctx>),
        (PointerValue<'ctx>, IntValue<'ctx>),
        IntValue<'ctx>,
    )> {
        let null = self.ptr_ty().const_null();
        Ok(match (base, method) {
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
        })
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
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        match (base, method) {
            ("Client", "new") => {}
            ("Client", "get" | "post") | ("RequestBuilder", "send") => {
                let (op, (a, an), (b, bn), h) = self.http_request(base, method, args)?;
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

    /// `Server.__install(buf, eof)` (runtime/stdlib/source/http_server.kara):
    /// the request at the start of `buf` made the current one, through
    /// `karac_mir_http_install`, as the interpreter's.
    fn http_install_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let (p, n) = self.view_of(&args[0])?;
        let eof = self.operand_scalar(&args[1])?.into_int_value();
        let eof = self
            .builder
            .build_int_z_extend_or_bit_cast(eof, self.llcx.i32_type(), "eof")
            .map_err(e)?;
        let used = self.rt_named(
            "karac_mir_http_install",
            &[p.into(), n.into(), eof.into()],
            64,
        )?;
        let d = self.place_ptr(dest)?;
        self.store(d, used.into())
    }

    /// `Server.__wire(resp)`: the response's bytes, as a `Vec[u8]`, reading
    /// `resp`'s fields by name, so a program's own `Response` with
    /// `headers: Vec[(String, String)]` sets headers.
    fn http_wire_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let ptr = self.ptr_ty();
        let i64t = self.llcx.i64_type();
        let aty = self.operand_type(&args[0])?;
        let mut at = self.operand_ptr(&args[0])?;
        let rt = match self.tys.kind(aty) {
            TyKind::Ref(i) | TyKind::MutRef(i) => {
                at = self.load_ptr(at)?;
                i
            }
            _ => aty,
        };
        let TyKind::Adt(adt) = self.tys.kind(rt) else {
            return Err("Server.__wire of no Response".into());
        };
        let names: Vec<String> = self.tys.adt(adt).variants[0]
            .fields
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        let field = |n: &str| names.iter().position(|f| f == n).map(|k| k as u32);
        let status_k = field("status").ok_or("a Response without a `status`")?;
        let status = self.load_i64(self.offset(at, self.field_offset(rt, None, status_k)?)?)?;
        let parts = |k: Option<u32>| -> R<(PointerValue<'ctx>, IntValue<'ctx>)> {
            match k {
                Some(k) => {
                    let p = self.offset(at, self.field_offset(rt, None, k)?)?;
                    Ok((self.load_ptr(p)?, self.load_i64(self.offset(p, 8)?)?))
                }
                None => Ok((ptr.const_null(), self.i64(0))),
            }
        };
        let (b, bn) = parts(field("body"))?;
        let (h, hn) = parts(field("headers"))?;
        let d = self.place_ptr(dest)?;
        let wire = self.extern_fn(
            "karac_mir_http_wire",
            self.llcx.void_type().fn_type(
                &[
                    i64t.into(),
                    ptr.into(),
                    i64t.into(),
                    ptr.into(),
                    i64t.into(),
                    ptr.into(),
                ],
                false,
            ),
        );
        self.builder
            .build_call(
                wire,
                &[
                    status.into(),
                    b.into(),
                    bn.into(),
                    h.into(),
                    hn.into(),
                    d.into(),
                ],
                "",
            )
            .map_err(e)?;
        self.drop_moved(args)
    }

    /// `Server.__wire_fixed(body)`: a 200 with `body`'s bytes, through
    /// `karac_mir_http_wire` as [`Self::http_wire_native`]'s.
    fn http_wire_fixed_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let ptr = self.ptr_ty();
        let (b, bn) = self.str_parts(&args[0])?;
        let d = self.place_ptr(dest)?;
        let i64t = self.llcx.i64_type();
        let wire = self.extern_fn(
            "karac_mir_http_wire",
            self.llcx.void_type().fn_type(
                &[
                    i64t.into(),
                    ptr.into(),
                    i64t.into(),
                    ptr.into(),
                    i64t.into(),
                    ptr.into(),
                ],
                false,
            ),
        );
        self.builder
            .build_call(
                wire,
                &[
                    self.i64(200).into(),
                    b.into(),
                    bn.into(),
                    ptr.const_null().into(),
                    self.i64(0).into(),
                    d.into(),
                ],
                "",
            )
            .map_err(e)?;
        self.drop_moved(args)
    }

    /// `Request`'s methods, on the request the running task is answering,
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

    /// The WebSocket handshake's accept value through
    /// `karac_mir_ws_accept_key(p, n, out)`, which the interpreter's
    /// `src/ws_frame.rs` backs too.
    fn ws_accept_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let (p, n) = self.str_parts(&args[0])?;
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let out = self.temp(string, "ws.accept")?;
        let f = self.extern_fn(
            "karac_mir_ws_accept_key",
            self.llcx.void_type().fn_type(
                &[
                    self.ptr_ty().into(),
                    self.llcx.i64_type().into(),
                    self.ptr_ty().into(),
                ],
                false,
            ),
        );
        self.builder
            .build_call(f, &[p.into(), n.into(), out.into()], "")
            .map_err(e)?;
        self.drop_moved(args)?;
        let d = self.place_ptr(dest)?;
        self.copy(d, out, string)
    }

    /// `StableHash.siphash24(bytes, k0, k1)` through the
    /// `karac_stable_siphash24` the `karac-hash` crate backs, which the
    /// interpreter calls directly.
    fn siphash_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let (p, n) = self.view_of(&args[0])?;
        let k0 = self.operand_scalar(&args[1])?.into_int_value();
        let k1 = self.operand_scalar(&args[2])?.into_int_value();
        let h = self.rt_named(
            "karac_stable_siphash24",
            &[p.into(), n.into(), k0.into(), k1.into()],
            64,
        )?;
        self.drop_moved(args)?;
        let d = self.place_ptr(dest)?;
        self.store(d, h.into())
    }

    /// `String.normalize(form)` through `karac_unicode_normalize_mir(form,
    /// p, n, out)`, which lives in the opt-in unicode archive. The form's
    /// tag is its variant index, the order runtime/src/unicode.rs numbers
    /// the forms in; the interpreter calls the same `icu_normalizer`.
    fn normalize_native(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let (p, n) = self.str_parts(&args[0])?;
        let fty = self.operand_type(&args[1])?;
        let at = self.operand_ptr(&args[1])?;
        let tag = self.load_tag(at, fty)?;
        let form = self
            .builder
            .build_int_z_extend_or_bit_cast(tag, self.llcx.i32_type(), "form")
            .map_err(e)?;
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let out = self.temp(string, "normalize.out")?;
        let f = self.extern_fn(
            "karac_unicode_normalize_mir",
            self.llcx.void_type().fn_type(
                &[
                    self.llcx.i32_type().into(),
                    self.ptr_ty().into(),
                    self.llcx.i64_type().into(),
                    self.ptr_ty().into(),
                ],
                false,
            ),
        );
        self.builder
            .build_call(f, &[form.into(), p.into(), n.into(), out.into()], "")
            .map_err(e)?;
        self.drop_moved(args)?;
        let d = self.place_ptr(dest)?;
        self.copy(d, out, string)
    }

    /// `std.crypto`'s engine natives (runtime/stdlib/std_crypto*.kara,
    /// B-2026-10-15-17) through `karac_crypto_mir(op, alg, a, a_len, b,
    /// b_len, h, out, out_len)`, which lives in the full archive; the
    /// interpreter's `crypto_method` makes the same calls.
    fn crypto_native(
        &mut self,
        engine: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let hmac = engine == "CryptoHmac";
        let op: u64 = match method {
            "digest" => 0,
            "mac" => 1,
            "start" if hmac => 3,
            "start" => 2,
            "update" => 4,
            "finish" => 5,
            "free" => 6,
            "eq" => 7,
            _ => return Ok(None),
        };
        let null = self.ptr_ty().const_null();
        let zero = self.i64(0);
        let (mut alg, mut h) = (zero, zero);
        let (mut a, mut a_len, mut b, mut b_len) = (null, zero, null, zero);
        let (mut out, mut out_len) = (null, zero);
        let int = |me: &mut Self, i: usize| -> R<IntValue<'ctx>> {
            Ok(me.operand_scalar(&args[i])?.into_int_value())
        };
        match op {
            0 => {
                alg = int(self, 0)?;
                (a, a_len) = self.view_of(&args[1])?;
                (out, out_len) = self.view_of(&args[2])?;
            }
            1 => {
                alg = int(self, 0)?;
                (a, a_len) = self.view_of(&args[1])?;
                (b, b_len) = self.view_of(&args[2])?;
                (out, out_len) = self.view_of(&args[3])?;
            }
            2 => alg = int(self, 0)?,
            3 => {
                alg = int(self, 0)?;
                (a, a_len) = self.view_of(&args[1])?;
            }
            4 => {
                h = int(self, 0)?;
                (a, a_len) = self.view_of(&args[1])?;
            }
            5 => {
                h = int(self, 0)?;
                (out, out_len) = self.view_of(&args[1])?;
            }
            6 => h = int(self, 0)?,
            _ => {
                (a, a_len) = self.view_of(&args[0])?;
                (b, b_len) = self.view_of(&args[1])?;
            }
        }
        let i32t = self.llcx.i32_type();
        let v = self.rt_named(
            "karac_crypto_mir",
            &[
                i32t.const_int(op, false).into(),
                alg.into(),
                a.into(),
                a_len.into(),
                b.into(),
                b_len.into(),
                h.into(),
                out.into(),
                out_len.into(),
            ],
            64,
        )?;
        self.drop_moved(args)?;
        let d = self.place_ptr(dest)?;
        match op {
            2 | 3 => self.store(d, v.into())?,
            7 => {
                let eq = self
                    .builder
                    .build_int_truncate(v, self.llcx.i8_type(), "")
                    .map_err(e)?;
                self.store(d, eq.into())?;
            }
            _ => {}
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
