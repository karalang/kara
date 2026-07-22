//! `std.process` codegen — phase-8 P1, the run-vs-build gap closer.
//!
//! Lowers the `#[compiler_builtin]` methods of `runtime/stdlib/
//! process.kara` — `Command.spawn`, `Child.wait` / `try_wait` / `kill`
//! / `stdout` / `stderr` / `stdin`, `ChildStdout.read_to_string` /
//! `ChildStderr.read_to_string`, `ChildStdin.write` / `close` — to
//! `karac_runtime_process_*` extern calls (`runtime/src/process.rs`).
//! The pure builder methods (`Command.new` / `.arg` / `.env` /
//! `.stdin` / `.stdout` / `.stderr`) have real Kāra bodies and compile
//! through the normal stdlib-body pass (`process_stdlib_program` in
//! `compiled_stdlib_programs`); only the OS-touching stubs land here.
//!
//! Fallible methods speak the `KaracIoResult` out-param ABI shared
//! with the file family and unpack through the shared
//! [`super::Codegen::lower_kara_io_result`] helper (`ExitStatus` /
//! `Option[ExitStatus]` Ok payloads ride bit-packed `value` words —
//! see `FileOkKind::{ExitStatusPacked, OptionExitStatusPacked}`).
//! `spawn` hands the runtime the *raw buffers* of the Command's
//! `Vec[String]` / `Vec[EnvVar]` fields (data pointer + element
//! count) so the runtime strides them natively — no codegen-side
//! loops — and the program String as a descriptor pointer (SSO-safe).
//!
//! Dispatch is keyed on the receiver's STATIC type name (the same
//! `type_name_of_expr` gate as the regex arms), so a user method that
//! happens to be called `wait` or `close` on an unrelated type never
//! routes here. Method-chain receivers are safe despite the
//! span-collision class: every `Command` builder link returns
//! `Command`, so whichever chain link a collided span resolves to,
//! the receiver's name is still `Command`.

use crate::ast::*;

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::IntPredicate;

use super::file::FileOkKind;

/// The `Stdio` stream selector values shared with
/// `karac_runtime_process_take_stream` / `_read_to_string`.
const STREAM_STDOUT: u64 = 0;
const STREAM_STDERR: u64 = 1;
const STREAM_STDIN: u64 = 2;

impl<'ctx> super::Codegen<'ctx> {
    /// Try to lower a `std.process` builtin method call. Returns
    /// `Ok(None)` (fall through to the rest of dispatch) unless the
    /// receiver's static type + method + arity match one of the
    /// process builtins.
    pub(super) fn try_compile_process_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Cheap method-name pre-filter before the type lookup.
        if !matches!(
            method,
            "spawn"
                | "wait"
                | "try_wait"
                | "kill"
                | "stdout"
                | "stderr"
                | "stdin"
                | "read_to_string"
                | "write"
                | "close"
        ) {
            return Ok(None);
        }
        let Some(type_name) = self.type_name_of_expr(object) else {
            return Ok(None);
        };
        match (type_name.as_str(), method, args.len()) {
            ("Command", "spawn", 0) => self.compile_command_spawn(object).map(Some),
            ("Child", "wait", 0) => self
                .compile_child_io_call(
                    object,
                    "karac_runtime_process_wait",
                    FileOkKind::ExitStatusPacked,
                )
                .map(Some),
            ("Child", "try_wait", 0) => self
                .compile_child_io_call(
                    object,
                    "karac_runtime_process_try_wait",
                    FileOkKind::OptionExitStatusPacked,
                )
                .map(Some),
            ("Child", "kill", 0) => self
                .compile_child_io_call(object, "karac_runtime_process_kill", FileOkKind::Unit)
                .map(Some),
            ("Child", "stdout", 0) => self
                .compile_child_take_stream(object, STREAM_STDOUT)
                .map(Some),
            ("Child", "stderr", 0) => self
                .compile_child_take_stream(object, STREAM_STDERR)
                .map(Some),
            ("Child", "stdin", 0) => self
                .compile_child_take_stream(object, STREAM_STDIN)
                .map(Some),
            ("ChildStdout", "read_to_string", 0) => self
                .compile_child_stream_read(object, STREAM_STDOUT)
                .map(Some),
            ("ChildStderr", "read_to_string", 0) => self
                .compile_child_stream_read(object, STREAM_STDERR)
                .map(Some),
            ("ChildStdin", "write", 1) => self
                .compile_child_stdin_write(object, &args[0].value)
                .map(Some),
            ("ChildStdin", "close", 0) => self.compile_child_stdin_close(object).map(Some),
            _ => Ok(None),
        }
    }

    /// Field index of `name` in struct `type_name`, per the
    /// declaration-order `struct_field_names` side table.
    fn process_field_index(&self, type_name: &str, name: &str) -> Result<u32, String> {
        self.struct_field_names
            .get(type_name)
            .and_then(|fields| fields.iter().position(|f| f == name))
            .map(|i| i as u32)
            .ok_or_else(|| {
                format!(
                    "codegen: std.process struct `{type_name}` missing field `{name}` \
                     (process.kara not registered in compiled_stdlib_programs?)"
                )
            })
    }

    /// Extract the `pid` field from a compiled `Child` / `ChildStdout`
    /// / `ChildStderr` / `ChildStdin` receiver value.
    fn compile_process_pid(
        &mut self,
        object: &Expr,
        type_name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let idx = self.process_field_index(type_name, "pid")?;
        let recv = self.compile_expr(object)?;
        let sv = recv.into_struct_value();
        Ok(self
            .builder
            .build_extract_value(sv, idx, "proc.pid")
            .unwrap()
            .into_int_value())
    }

    /// Read a `Stdio` enum tag out of a Command field value. A
    /// payload-less enum lowers to a `{ i64 tag }` aggregate; be
    /// tolerant of a bare-i64 lowering too.
    fn stdio_tag_of(
        &mut self,
        field_val: BasicValueEnum<'ctx>,
        name: &str,
    ) -> inkwell::values::IntValue<'ctx> {
        match field_val {
            BasicValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(sv, 0, name)
                .unwrap()
                .into_int_value(),
            BasicValueEnum::IntValue(iv) => iv,
            other => panic!("std.process: unexpected Stdio field lowering {other:?}"),
        }
    }

    /// Compile `cmd.spawn() -> Result[Child, IoError]`.
    fn compile_command_spawn(&mut self, object: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        let f_prog = self.process_field_index("Command", "program")?;
        let f_args = self.process_field_index("Command", "cmd_args")?;
        let f_env = self.process_field_index("Command", "cmd_env")?;
        let f_in = self.process_field_index("Command", "cmd_stdin")?;
        let f_out = self.process_field_index("Command", "cmd_stdout")?;
        let f_err = self.process_field_index("Command", "cmd_stderr")?;

        let recv = self.compile_expr(object)?;
        let sv = recv.into_struct_value();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "process codegen called outside fn".to_string())?;

        // Program String → descriptor pointer (store the 24-byte
        // aggregate to a slot; the runtime decodes it SSO-aware).
        let prog_val = self
            .builder
            .build_extract_value(sv, f_prog, "proc.prog")
            .unwrap();
        let str_ty = self.vec_struct_type();
        let prog_slot = self.create_entry_alloca(fn_val, "proc.prog.slot", str_ty.into());
        self.builder.build_store(prog_slot, prog_val).unwrap();

        // Vec fields → (data ptr, element count).
        let vec_parts = |field: u32, tag: &str| {
            let v = self
                .builder
                .build_extract_value(sv, field, &format!("proc.{tag}"))
                .unwrap()
                .into_struct_value();
            let data = self
                .builder
                .build_extract_value(v, 0, &format!("proc.{tag}.data"))
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(v, 1, &format!("proc.{tag}.len"))
                .unwrap()
                .into_int_value();
            (data, len)
        };
        let (args_data, args_len) = vec_parts(f_args, "args");
        let (env_data, env_len) = vec_parts(f_env, "env");

        // Stdio enum tags.
        let stdin_val = self
            .builder
            .build_extract_value(sv, f_in, "proc.cfg.in")
            .unwrap();
        let stdout_val = self
            .builder
            .build_extract_value(sv, f_out, "proc.cfg.out")
            .unwrap();
        let stderr_val = self
            .builder
            .build_extract_value(sv, f_err, "proc.cfg.err")
            .unwrap();
        let stdin_tag = self.stdio_tag_of(stdin_val, "proc.cfg.in.tag");
        let stdout_tag = self.stdio_tag_of(stdout_val, "proc.cfg.out.tag");
        let stderr_tag = self.stdio_tag_of(stderr_val, "proc.cfg.err.tag");

        let slot = self.alloca_io_result_slot()?;
        let spawn_fn = self
            .module
            .get_function("karac_runtime_process_spawn")
            .expect("karac_runtime_process_spawn declared in Codegen::new");
        self.builder
            .build_call(
                spawn_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(slot),
                    BasicMetadataValueEnum::PointerValue(prog_slot),
                    BasicMetadataValueEnum::PointerValue(args_data),
                    BasicMetadataValueEnum::IntValue(args_len),
                    BasicMetadataValueEnum::PointerValue(env_data),
                    BasicMetadataValueEnum::IntValue(env_len),
                    BasicMetadataValueEnum::IntValue(stdin_tag),
                    BasicMetadataValueEnum::IntValue(stdout_tag),
                    BasicMetadataValueEnum::IntValue(stderr_tag),
                ],
                "proc.spawn.call",
            )
            .unwrap();
        // Ok payload = Child { pid } — a single-i64-field struct, so the
        // pid rides Result payload word 0 exactly like a byte count.
        self.lower_kara_io_result(slot, FileOkKind::ByteCount)
    }

    /// Shared shape of `Child.wait` / `try_wait` / `kill`: extract the
    /// pid, call `(out, pid)`, unpack per `ok_kind`.
    fn compile_child_io_call(
        &mut self,
        object: &Expr,
        runtime_sym: &str,
        ok_kind: FileOkKind,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let pid = self.compile_process_pid(object, "Child")?;
        let slot = self.alloca_io_result_slot()?;
        let f = self
            .module
            .get_function(runtime_sym)
            .unwrap_or_else(|| panic!("{runtime_sym} declared in Codegen::new"));
        self.builder
            .build_call(
                f,
                &[
                    BasicMetadataValueEnum::PointerValue(slot),
                    BasicMetadataValueEnum::IntValue(pid),
                ],
                "proc.io.call",
            )
            .unwrap();
        self.lower_kara_io_result(slot, ok_kind)
    }

    /// Compile `child.stdout() / .stderr() / .stdin() ->
    /// Option[ChildStdout / ChildStderr / ChildStdin]`. The runtime
    /// returns the pid when the handle was taken, 0 otherwise; the
    /// Option is built branch-free (tag = ret > 0; payload word 0 =
    /// ret, which is already 0 in the None case).
    fn compile_child_take_stream(
        &mut self,
        object: &Expr,
        which: u64,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let pid = self.compile_process_pid(object, "Child")?;
        let i64_ty = self.context.i64_type();
        let take_fn = self
            .module
            .get_function("karac_runtime_process_take_stream")
            .expect("karac_runtime_process_take_stream declared in Codegen::new");
        let ret = self
            .builder
            .build_call(
                take_fn,
                &[
                    BasicMetadataValueEnum::IntValue(pid),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(which, false)),
                ],
                "proc.take.ret",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::SGT,
                ret,
                i64_ty.const_zero(),
                "proc.take.some",
            )
            .unwrap();
        let tag = self
            .builder
            .build_int_z_extend(is_some, i64_ty, "proc.take.tag")
            .unwrap();
        let opt_layout = self
            .enum_layouts
            .get("Option")
            .ok_or_else(|| "Option layout not registered before process codegen".to_string())?;
        let opt_ty = opt_layout.llvm_type;
        let mut agg = opt_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, tag, 0, "proc.take.opt.tag")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, ret, 1, "proc.take.opt.w0")
            .unwrap()
            .into_struct_value();
        for w in 2..opt_ty.count_fields() {
            agg = self
                .builder
                .build_insert_value(agg, i64_ty.const_zero(), w, &format!("proc.take.opt.w{w}"))
                .unwrap()
                .into_struct_value();
        }
        Ok(agg.into())
    }

    /// Compile `ChildStdout.read_to_string()` / `ChildStderr.…` —
    /// StringPayload Ok through the shared unpacker.
    fn compile_child_stream_read(
        &mut self,
        object: &Expr,
        which: u64,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let type_name = if which == STREAM_STDOUT {
            "ChildStdout"
        } else {
            "ChildStderr"
        };
        let pid = self.compile_process_pid(object, type_name)?;
        let i64_ty = self.context.i64_type();
        let slot = self.alloca_io_result_slot()?;
        let read_fn = self
            .module
            .get_function("karac_runtime_process_read_to_string")
            .expect("karac_runtime_process_read_to_string declared in Codegen::new");
        self.builder
            .build_call(
                read_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(slot),
                    BasicMetadataValueEnum::IntValue(pid),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(which, false)),
                ],
                "proc.read.call",
            )
            .unwrap();
        self.lower_kara_io_result(slot, FileOkKind::StringPayload)
    }

    /// Compile `ChildStdin.write(data) -> Result[Unit, IoError]`. The
    /// String argument is passed as a descriptor pointer (SSO-safe);
    /// the runtime borrows the bytes read-only.
    fn compile_child_stdin_write(
        &mut self,
        object: &Expr,
        data_arg: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let pid = self.compile_process_pid(object, "ChildStdin")?;
        let data_val = self.compile_expr(data_arg)?;
        let fn_val = self
            .current_fn
            .ok_or_else(|| "process codegen called outside fn".to_string())?;
        let str_ty = self.vec_struct_type();
        let data_slot = self.create_entry_alloca(fn_val, "proc.write.data", str_ty.into());
        self.builder.build_store(data_slot, data_val).unwrap();
        let slot = self.alloca_io_result_slot()?;
        let write_fn = self
            .module
            .get_function("karac_runtime_process_stdin_write")
            .expect("karac_runtime_process_stdin_write declared in Codegen::new");
        self.builder
            .build_call(
                write_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(slot),
                    BasicMetadataValueEnum::IntValue(pid),
                    BasicMetadataValueEnum::PointerValue(data_slot),
                ],
                "proc.write.call",
            )
            .unwrap();
        self.lower_kara_io_result(slot, FileOkKind::Unit)
    }

    /// Compile `ChildStdin.close() -> Result[Unit, IoError]`. The
    /// runtime close is void + idempotent (mirroring the interpreter's
    /// always-`Ok` semantics), so the Result is a constant `Ok(Unit)`.
    fn compile_child_stdin_close(&mut self, object: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        let pid = self.compile_process_pid(object, "ChildStdin")?;
        let close_fn = self
            .module
            .get_function("karac_runtime_process_stdin_close")
            .expect("karac_runtime_process_stdin_close declared in Codegen::new");
        self.builder
            .build_call(
                close_fn,
                &[BasicMetadataValueEnum::IntValue(pid)],
                "proc.close.call",
            )
            .unwrap();
        let i64_ty = self.context.i64_type();
        let result_layout = self
            .enum_layouts
            .get("Result")
            .ok_or_else(|| "Result layout not registered before process codegen".to_string())?;
        let result_ty = result_layout.llvm_type;
        let mut agg = result_ty.get_undef();
        // Ok tag = 1 (seeded layout); Unit payload = all-zero words.
        agg = self
            .builder
            .build_insert_value(agg, i64_ty.const_int(1, false), 0, "proc.close.ok.tag")
            .unwrap()
            .into_struct_value();
        for w in 1..result_ty.count_fields() {
            agg = self
                .builder
                .build_insert_value(agg, i64_ty.const_zero(), w, &format!("proc.close.w{w}"))
                .unwrap()
                .into_struct_value();
        }
        Ok(agg.into())
    }
}
