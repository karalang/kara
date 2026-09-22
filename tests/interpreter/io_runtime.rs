//! files, processes, CLI, environment, sockets, time -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter io_runtime::
//!
//! New fixtures about files, processes, CLI, environment, sockets, time belong in this file.

use super::*;

#[test]
fn test_unknown_primitive_method_is_runtime_error_not_ice() {
    // `karac run` bypasses typecheck enforcement, so an unknown method on a
    // primitive reaches the interpreter. It used to hit `unreachable!` and
    // panic (ICE); it now records a structured runtime error.
    let errors = runtime_errors("fn main() { let x = 5i64; let _ = x.bogus(); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("bogus") && e.message.contains("i64")),
        "expected a structured runtime error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_mmio_intrinsics_clean_error_not_panic() {
    // B-2026-07-12-7 — the tree-walk interpreter has no raw-pointer model, so
    // `ptr.mut`/`ptr.const` and `volatile_read`/`volatile_write` are codegen-
    // only. Reaching them used to panic with `unreachable!("variable 'ptr' not
    // found")` (the `ptr` receiver evaluated as an unbound identifier); they now
    // record a clean structured runtime error naming the intrinsic + the
    // `karac build` / non-`--interp` workaround.
    let errors = runtime_errors(
        "fn main() { let cell: i32 = 0; unsafe { let p: *const i32 = ptr.const(cell); } }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("ptr.const") && e.message.contains("codegen")),
        "expected a clean ptr-intrinsic runtime error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    // The `volatile_write` free-call peer.
    let errors = runtime_errors(
        "fn main() { let cell: i32 = 0; unsafe { volatile_write(ptr.mut(cell), 5); } }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("codegen")
            && (e.message.contains("volatile_write") || e.message.contains("ptr.mut"))),
        "expected a clean MMIO-intrinsic runtime error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_illtyped_binop_records_runtime_error_not_panic() {
    // B-2026-06-12-4: `String * Int` is a hard typecheck error, but `karac run`
    // executes despite typecheck errors (it demotes them to warnings). The
    // interpreter's binary-op fallthrough used to `unreachable!()`-panic on the
    // illegal operand that slipped through; it must now surface a graceful
    // runtime error instead. (Reaching this test at all — rather than aborting
    // the process — is the regression guard.)
    let errors = runtime_errors(r#"fn main() { let x = "ab" * 3; }"#);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not defined for operands")
                && e.message.contains("String")),
        "expected a graceful 'operator not defined' runtime error for String * Int, got {:?}",
        errors
    );
}

#[test]
fn test_illtyped_unary_records_runtime_error_not_panic() {
    // Sibling of the binop case (B-2026-06-12-4): unary `-` on a String is a
    // hard typecheck error; under `karac run` it reaches the interpreter, whose
    // unary fallthrough must record a runtime error rather than `unreachable!()`.
    let errors = runtime_errors(r#"fn main() { let x = -"ab"; }"#);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not defined for an operand")
                && e.message.contains("String")),
        "expected a graceful 'operator not defined' runtime error for -String, got {:?}",
        errors
    );
}

#[test]
fn test_division_by_zero_records_runtime_error() {
    let errors = runtime_errors("fn main() { let x = 10; let y = 0; let z = x / y; }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("division by zero")),
        "expected a division-by-zero runtime error, got {:?}",
        errors
    );
}

#[test]
fn test_modulo_by_zero_records_runtime_error() {
    let errors = runtime_errors("fn main() { let x = 10; let y = 0; let z = x % y; }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("division by zero")),
        "expected a division-by-zero runtime error for %, got {:?}",
        errors
    );
}

#[test]
fn test_todo_records_runtime_error() {
    let errors = runtime_errors(r#"fn main() { todo("finish this"); }"#);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not yet implemented") && e.message.contains("finish this")),
        "expected todo() to surface a runtime error, got {:?}",
        errors
    );
}

#[test]
fn test_unwrap_none_records_runtime_error() {
    let errors = runtime_errors("fn main() { let x = None; x.unwrap(); }");
    assert!(
        errors.iter().any(|e| e.message.contains("unwrap")),
        "expected an unwrap runtime error, got {:?}",
        errors
    );
}

// ── env.args / env.var ────────────────────────────────────────────

#[test]
fn test_env_var_missing_key_returns_err() {
    let output = run("fn main() {
         match env.var(\"__KARAC_NO_SUCH_VAR_XYZ__\") {
             Ok(v) => println(v),
             Err(e) => println(\"not found\"),
         }
     }");
    assert_eq!(output, "not found\n");
}

#[test]
fn test_env_args_returns_array() {
    // env.args() returns a Vec[String]; len() ≥ 1 (includes binary path)
    let output = run("fn main() {
         let args = env.args();
         println(args.len() > 0);
     }");
    assert_eq!(output, "true\n");
}

// ── impl From[VarError] for IoError — variant mapping ────────────────────

#[test]
fn test_var_error_not_present_maps_to_io_error_not_found() {
    // VarError.NotPresent → IoError.NotFound via the baked stdlib impl.
    let output = run("fn main() {
         let io: IoError = IoError.from(VarError.NotPresent);
         match io {
             IoError.NotFound => println(\"not_found\"),
             IoError.PermissionDenied => println(\"perm_denied\"),
             IoError.AlreadyExists => println(\"already_exists\"),
             IoError.UnexpectedEof => println(\"eof\"),
             IoError.InvalidUtf8 => println(\"invalid_utf8\"),
             IoError.Interrupted => println(\"interrupted\"),
             IoError.Other(_) => println(\"other\"),
         }
     }");
    assert_eq!(output, "not_found\n");
}

#[test]
fn test_resource_method_without_provider_raises_runtime_error() {
    let errors = runtime_errors(
        "effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             println(UserDB.id());
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one runtime error, got {:?}",
        errors
    );
    assert!(
        errors[0]
            .message
            .contains("no provider bound for resource 'UserDB'"),
        "message did not mention missing provider: {:?}",
        errors[0].message
    );
}

#[test]
fn test_filesystem_read_lines_missing_file_returns_err() {
    let src = "fn main() with reads(FileSystem) {
                   match FileSystem.read_lines(\"/nonexistent_karac_rl_xyz.txt\") {
                       Ok(_) => println(\"ok\"),
                       Err(_) => println(\"err\"),
                   }
               }";
    assert_eq!(run_no_errors(src), "err\n");
}

#[test]
fn test_filesystem_read_nonexistent_file_returns_err() {
    let src = "fn main() {
                   let r = FileSystem.read_to_string(\"/nonexistent_karac_test_xyz.txt\");
                   match r {
                       Ok(_) => println(\"ok\"),
                       Err(e) => match e {
                           IoError.NotFound => println(\"not found\"),
                           _ => println(\"other error\"),
                       },
                   }
               }";
    let out = run_no_errors(src);
    assert_eq!(out, "not found\n");
}

/// B-2026-08-10-3 — `File.seek(whence: SeekFrom, offset: i64) ->
/// Result[i64, IoError]`, returning the NEW absolute position.
///
/// The runtime entry point `karac_runtime_file_seek` had shipped long before
/// the surface did, deliberately, so that adding `seek` would need no runtime
/// rebuild. This is the surface half arriving: stdlib stub, interpreter arm,
/// codegen lowering.
///
/// Random access is what the file holds: after seeking to 2 in "ABCD", the
/// next byte read must be 'C' (67) — a position check alone would pass even
/// if the cursor never moved.
///
/// `Start` with a negative offset is an error rather than a wrap-around: the
/// runtime takes a `u64` there, so casting -5 would seek to ~1.8e19 instead of
/// failing. The interpreter rejects it before the cast for that reason.
#[test]
fn test_file_seek_positions_and_reads() {
    let tmp = std::env::temp_dir().join("karac_test_file_seek.bin");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let data = [65u8, 66u8, 67u8, 68u8];
                     match f.write(data[0..4]) {{ Ok(_) => {{}} Err(_) => println(\"write err\") }}
                     match f.flush() {{ Ok(_) => {{}} Err(_) => println(\"flush err\") }}
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match File.open(\"{path}\") {{
                 Ok(g) => {{
                     match g.seek(SeekFrom.Start, 2i64) {{
                         Ok(p) => println(\"pos \" + p.to_string()),
                         Err(_) => println(\"seek err\"),
                     }}
                     let mut buf = [0u8, 0u8];
                     match g.read(mut buf) {{
                         Ok(n) => println(\"read \" + n.to_string() + \" b0 \" + buf[0].to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                     match g.seek(SeekFrom.End, -1i64) {{
                         Ok(e) => println(\"end \" + e.to_string()),
                         Err(_) => println(\"seek err\"),
                     }}
                     match g.seek(SeekFrom.Current, 0i64) {{
                         Ok(c) => println(\"cur \" + c.to_string()),
                         Err(_) => println(\"seek err\"),
                     }}
                     match g.seek(SeekFrom.Start, -5i64) {{
                         Ok(_) => println(\"unexpected ok\"),
                         Err(_) => println(\"neg rejected\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    // seek(Start,2) → 2; the byte there is 'C' (67), so the cursor really
    // moved; seek(End,-1) → 3 on a 4-byte file; seek(Current,0) → 3 (the
    // "where am I" query, which is why no separate `tell` is needed).
    assert_eq!(out, "pos 2\nread 2 b0 67\nend 3\ncur 3\nneg rejected\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_file_open_nonexistent_returns_io_error_not_found() {
    // Same NotFound variant as FileSystem.read_to_string — the
    // `io_error_from_std` helper maps ErrorKind::NotFound to
    // IoError.NotFound regardless of which surface fired it.
    let src = "fn main() {
                   match File.open(\"/nonexistent_karac_test_F1.txt\") {
                       Ok(_) => println(\"unexpected ok\"),
                       Err(e) => match e {
                           IoError.NotFound => println(\"not found\"),
                           _ => println(\"other\"),
                       },
                   }
               }";
    let out = run_no_errors(src);
    assert_eq!(out, "not found\n");
}

#[test]
fn test_file_flush_on_writable_handle_returns_ok() {
    // Flush on a freshly opened writable file returns Ok(Unit) even
    // when nothing was written — the std::fs::File flush is a no-op
    // for un-buffered handles, never an error in this case.
    let tmp = std::env::temp_dir().join("karac_test_file_flush_ok.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     match f.flush() {{
                         Ok(_) => println(\"flushed\"),
                         Err(_) => println(\"err\"),
                     }}
                 }}
                 Err(_) => println(\"create err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "flushed\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_file_sync_all_and_sync_data_return_ok_and_persist() {
    // B-2026-07-30-16 — interpreter half of the durability pair. Both
    // typechecked but had no dispatch arm here, so a program that
    // passed `karac check` died at runtime with "method 'sync_all' not
    // found on type 'unknown'". Asserts the surface (Ok on both) and
    // that the written bytes are readable afterwards; the codegen twin
    // is `test_e2e_file_sync_all_and_sync_data_persist_contents`.
    let tmp = std::env::temp_dir().join("karac_test_file_sync_pair.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let mut data: Vec[u8] = Vec.new();
                     data.push(79u8); data.push(75u8);
                     match f.write(data) {{
                         Ok(_) => println(\"wrote\"),
                         Err(_) => println(\"write err\"),
                     }}
                     match f.sync_all() {{
                         Ok(_) => println(\"sync_all\"),
                         Err(_) => println(\"sync_all err\"),
                     }}
                     match f.sync_data() {{
                         Ok(_) => println(\"sync_data\"),
                         Err(_) => println(\"sync_data err\"),
                     }}
                 }}
                 Err(_) => println(\"create err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "wrote\nsync_all\nsync_data\n");
    let contents = std::fs::read(&tmp).expect("read tempfile");
    assert_eq!(contents, b"OK");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_read_line_at_eof_returns_zero() {
    // read_line on an empty file returns Ok(0) (EOF), leaving the
    // destination String untouched.
    let tmp = std::env::temp_dir().join("karac_test_bufreader_eof.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut line = String.new();
                     match br.read_line(line) {{
                         Ok(n) => println(\"n=\" + n.to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "n=0\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_lines_iterates_and_strips_newlines() {
    // `for line in br.lines()` yields one Ok(line) per line with the
    // trailing newline stripped, terminating at EOF.
    let tmp = std::env::temp_dir().join("karac_test_bufreader_lines.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"alpha\nbeta\ngamma\n").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut count = 0;
                     for line in br.lines() {{
                         match line {{
                             Ok(s) => {{ println(\"[\" + s + \"]\"); count = count + 1; }}
                             Err(_) => println(\"read err\"),
                         }}
                     }}
                     println(\"count=\" + count.to_string());
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "[alpha]\n[beta]\n[gamma]\ncount=3\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_lines_crlf_and_no_trailing_newline() {
    // CRLF line endings are stripped (\r\n, matching std::io::Lines), and a
    // final line with no trailing newline is still yielded.
    let tmp = std::env::temp_dir().join("karac_test_bufreader_lines_crlf.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"one\r\ntwo\r\nthree").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     for line in br.lines() {{
                         match line {{
                             Ok(s) => println(\"[\" + s + \"]\"),
                             Err(_) => println(\"read err\"),
                         }}
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "[one]\n[two]\n[three]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_lines_empty_file_yields_nothing() {
    // An empty file produces zero lines (the for-loop body never runs).
    let tmp = std::env::temp_dir().join("karac_test_bufreader_lines_empty.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut count = 0;
                     for line in br.lines() {{
                         match line {{
                             Ok(_) => {{ count = count + 1; }}
                             Err(_) => {{}}
                         }}
                     }}
                     println(\"count=\" + count.to_string());
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "count=0\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_fill_buf_at_eof_returns_empty_slice() {
    // fill_buf on an empty file returns Ok with a zero-length slice (EOF).
    let tmp = std::env::temp_dir().join("karac_test_bufreader_fillbuf_eof.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     match br.fill_buf() {{
                         Ok(buf) => println(\"len=\" + buf.len().to_string()),
                         Err(_) => println(\"fill err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "len=0\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_consume_clamps_past_buffer() {
    // consume(n) with n past the buffered length is clamped (no panic); after
    // consuming all 3 buffered bytes, a read returns 0 (EOF).
    let tmp = std::env::temp_dir().join("karac_test_bufreader_consume_clamp.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"abc").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let _ = br.fill_buf();
                     br.consume(100);
                     let rest = [0u8, 0u8, 0u8];
                     match br.read(rest[0..3]) {{
                         Ok(n) => println(\"n=\" + n.to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "n=0\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── std.cli — builder-style argument parser ────────────────────────

#[test]
fn test_cli_builder_records_state() {
    let output = run(r#"fn main() {
         let p = Parser.new("greet")
             .about("Greets a name")
             .arg("--name", Arg.string().required())
             .flag("--verbose", short: 'v', help: "verbose");
         println(p.program_name);
         println(p.about_text);
         println(p.args.len());
         println(p.flags.len());
     }"#);
    assert_eq!(output, "greet\nGreets a name\n1\n1\n");
}

#[test]
fn test_cli_parse_named_arg_returns_value() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--name", "alice"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet")
                     .arg("--name", Arg.string().required());
                 match p.parse() {
                     Ok(args) => {
                         match args.get_string("--name") {
                             Ok(n) => println(n),
                             Err(e) => println(e.message),
                         }
                     }
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "alice\n");
}

#[test]
fn test_cli_parse_unknown_token_becomes_positional() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "extra1", "extra2"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet");
                 match p.parse() {
                     Ok(args) => println(args.positional.len()),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "2\n");
}

#[test]
fn test_cli_arg_builder_chains() {
    let output = run(r#"fn main() {
         let a = Arg.string().required().help("a help");
         println(a.is_required);
         println(a.help_text);
     }"#);
    assert_eq!(output, "true\na help\n");
}

#[test]
fn test_cli_subcommand_name_none_when_no_subcommand_invoked() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .subcommand("upper", Parser.new("upper"));
                 match parser.parse() {
                     Ok(args) => {
                         match args.subcommand_name() {
                             Some(name) => println(name),
                             None => println("none"),
                         }
                     }
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "none\n");
}

#[test]
fn test_cli_subcommand_consumes_remaining_tokens_as_its_own() {
    // Tokens AFTER the subcommand name match against the sub-parser,
    // not the parent. Here `--name` is declared on the SUBCOMMAND, so
    // the parent never sees it.
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "upper", "--name", "bob"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .subcommand("upper", Parser.new("upper").arg("--name", Arg.string()));
                 match parser.parse() {
                     Ok(args) => {
                         match args.sub {
                             Some(s) => {
                                 match s.get_string("--name") {
                                     Ok(n) => println(n),
                                     Err(e) => println(e.message),
                                 }
                             }
                             None => println("no_sub"),
                         }
                     }
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "bob\n");
}

#[test]
fn test_cli_parse_help_short_circuits_with_rendered_text() {
    // `--help` fires before normal arg checking — `--name` is declared
    // required, but the help short-circuit wins. The error message
    // carries the rendered help text.
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--help"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .about("Greets a name")
                     .arg("--name", Arg.string().required());
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert!(output.contains("greet - Greets a name"), "output: {output}");
    assert!(output.contains("USAGE:"), "output: {output}");
    assert!(output.contains("--name <VALUE>"), "output: {output}");
    assert!(output.contains("[required]"), "output: {output}");
    assert!(output.contains("-h, --help"), "output: {output}");
}

#[test]
fn test_cli_parse_short_help_h_short_circuits() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "-h"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("p").about("about");
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert!(output.contains("p - about"), "output: {output}");
    assert!(output.contains("USAGE:"), "output: {output}");
}

#[test]
fn test_cli_parse_version_short_circuits() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--version"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet").version("1.2.3");
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "greet 1.2.3\n");
}

#[test]
fn test_cli_parse_short_version_v_short_circuits() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "-V"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet").version("0.1.0");
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "greet 0.1.0\n");
}

#[test]
fn test_cli_version_line_falls_back_to_program_name_when_unset() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "-V"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet");
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    // No version() declared — fall back to bare program name.
    assert_eq!(output, "greet\n");
}

#[test]
fn test_cli_help_text_renders_subcommands_section() {
    let output = run(r#"fn main() {
         let parser = Parser.new("greet")
             .subcommand("upper", Parser.new("upper").about("uppercase"))
             .subcommand("lower", Parser.new("lower").about("lowercase"));
         let h = parser.help_text();
         println(h);
     }"#);
    assert!(output.contains("SUBCOMMANDS:"), "output: {output}");
    assert!(output.contains("    upper  uppercase"), "output: {output}");
    assert!(output.contains("    lower  lowercase"), "output: {output}");
}

// ── std.process — Command / Child surface ──────────────────────────

#[test]
fn test_process_command_builder_records_state() {
    let output = run(r#"fn main() {
         let cmd = Command.new("ls").arg("-la").arg("/tmp").env("PATH", "/usr/bin");
         println(cmd.program);
         println(cmd.cmd_args.len());
         println(cmd.cmd_env.len());
     }"#);
    assert_eq!(output, "ls\n2\n1\n");
}

#[test]
fn test_process_command_env_records_kv() {
    let output = run(r#"fn main() {
         let cmd = Command.new("printenv").env("FOO", "bar");
         match cmd.cmd_env.get(0) {
             Some(e) => {
                 println(e.key);
                 println(e.value);
             }
             None => println("?"),
         }
     }"#);
    assert_eq!(output, "FOO\nbar\n");
}

#[cfg(unix)]
#[test]
fn test_process_kill_terminates_child_and_wait_reports_failure() {
    // Spawn /bin/sleep 60 (way longer than test runtime), kill it,
    // then wait. kill returns Ok(Unit); wait returns Ok(status) with
    // success=false (terminated by signal). The child is reaped by
    // the wait call after the kill (`kill` itself leaves the table
    // entry in place per the spec — the caller still needs wait()).
    // Gated on `unix` because the hard-coded path doesn't resolve
    // on Windows.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/sleep").arg("60");
         match cmd.spawn() {
             Ok(child) => {
                 match child.kill() {
                     Ok(_) => println("killed"),
                     Err(_) => println("kill_err"),
                 }
                 match child.wait() {
                     Ok(status) => println(status.success),
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "killed\nfalse\n");
}

#[cfg(unix)]
#[test]
fn test_process_env_vars_propagate_to_child() {
    // The .env() builder method propagates to the spawned process.
    // Child stdout inherits the parent fd, so it doesn't show up in
    // Kāra's `captured_output` — instead, verify env var propagation
    // by having the child's shell `test` the var against the expected
    // value and exit 0 / 1 accordingly. The wait-status's `success`
    // field is the signal. Gated on `unix` because `/bin/sh` doesn't
    // resolve on Windows.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/sh")
             .arg("-c")
             .arg("test \"$MY_VAR\" = \"kara-env-witness\"")
             .env("MY_VAR", "kara-env-witness");
         match cmd.spawn() {
             Ok(child) => {
                 match child.wait() {
                     Ok(s) => println(s.success),
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(
        output, "true\n",
        "expected env var to propagate (exit 0); got: {output}"
    );
}

#[test]
fn test_process_wait_on_unknown_child_returns_not_found() {
    // If a Child handle's pid isn't in the interpreter's side-table
    // (e.g., the user fabricated a Child struct manually), wait()
    // returns IoError.NotFound rather than panic. Defensive against
    // user code that hand-constructs Child values to side-step the
    // builder.
    let output = run(r#"fn main() {
         let fake = Child { pid: 999999999 };
         match fake.wait() {
             Ok(_) => println("ok??"),
             Err(IoError.NotFound) => println("not_found"),
             Err(_) => println("other_err"),
         }
     }"#);
    assert_eq!(output, "not_found\n");
}

#[test]
fn test_process_stdio_builder_records_redirection() {
    // The stdin/stdout/stderr builders thread the `Stdio` setting onto
    // the Command (default `Inherit`). Read the fields back to confirm
    // the chain records what was set without dropping the others.
    let output = run(r#"fn main() {
         let cmd = Command.new("ls").stdout(Stdio.Null).stderr(Stdio.Null);
         match cmd.cmd_stdin { Stdio.Inherit => println("in:inherit"), Stdio.Null => println("in:null") }
         match cmd.cmd_stdout { Stdio.Inherit => println("out:inherit"), Stdio.Null => println("out:null") }
         match cmd.cmd_stderr { Stdio.Inherit => println("err:inherit"), Stdio.Null => println("err:null") }
     }"#);
    assert_eq!(output, "in:inherit\nout:null\nerr:null\n");
}

#[cfg(unix)]
#[test]
fn test_process_stdout_not_piped_yields_none() {
    // A stream left at the default `Stdio.Inherit` (here stdout is
    // redirected to `Null`, also not piped) has no captured handle, so
    // `Child.stdout()` is `None` — mirroring `std::process::Child::stdout`.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/echo").arg("x").stdout(Stdio.Null);
         match cmd.spawn() {
             Ok(child) => {
                 match child.stdout() {
                     Some(_) => println("some"),
                     None => println("none"),
                 }
                 match child.wait() {
                     Ok(_) => {}
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "none\n");
}

#[test]
fn test_unwired_path_reports_runtime_error_not_panic() {
    // The eval_expr Path fallback used to be `unreachable!` — a
    // typechecker-accepted-but-uninterpreted path killed the process
    // with a Rust panic instead of a span-carrying diagnostic.
    // `String.bogus()` survives resolve (run_program_full tolerates the
    // typecheck rejection) and exercises the degraded path: a recorded
    // RuntimeError naming the path, no panic.
    let errors = runtime_errors(r#"fn main() { let _x = String.bogus(); }"#);
    assert_eq!(errors.len(), 1, "expected exactly one runtime error");
    assert!(
        errors[0].message.contains("no interpreter evaluation rule"),
        "unexpected message: {}",
        errors[0].message
    );
    assert!(
        errors[0].message.contains("String.bogus"),
        "message should name the unwired path: {}",
        errors[0].message
    );
}

// ── std.http ──────────────────────────────────────────────────────────────────

#[test]
fn test_http_client_new() {
    // Client.new() should return a Client struct — no network needed.
    let output = run(r#"
fn main() {
    let c = Client.new();
    println("ok");
}
"#);
    assert_eq!(output, "ok\n");
}

#[test]
#[ignore = "requires network access"]
fn test_http_client_get_ok() {
    let output = run(r#"
fn main() {
    let c = Client.new();
    match c.get("http://httpbin.org/status/200") {
        Ok(resp) => println(resp.status()),
        Err(e) => println(e.message()),
    }
}
"#);
    assert_eq!(output, "200\n");
}

#[test]
fn test_http_client_get_invalid_url() {
    // A clearly invalid URL should produce Err, not panic.
    let output = run(r#"
fn main() {
    let c = Client.new();
    match c.get("not-a-url") {
        Ok(_) => println("ok"),
        Err(_) => println("err"),
    }
}
"#);
    assert_eq!(output, "err\n");
}

#[test]
#[ignore = "requires network access"]
fn test_http_client_post_ok() {
    let output = run(r#"
fn main() {
    let c = Client.new();
    match c.post("http://httpbin.org/post", "hello") {
        Ok(resp) => println(resp.status()),
        Err(e) => println(e.message()),
    }
}
"#);
    assert_eq!(output, "200\n");
}

#[test]
fn test_http_response_methods() {
    // Invalid URL → Err — verify the error path and HttpError.message() work.
    let output = run(r#"
fn make_client() -> Client { Client.new() }
fn main() {
    let c = make_client();
    match c.get("not-a-url") {
        Ok(resp) => println(resp.status()),
        Err(e) => println("error"),
    }
}
"#);
    assert_eq!(output, "error\n");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_http_request_builder_chain_applies_method_headers_and_body() {
    // B-2026-07-31 follow-up — `Client.request(m, url)` and the whole
    // `RequestBuilder` chain (`header` / `body` / `timeout` / `send`) had NO
    // interpreter dispatch arm: codegen backed it since phase-8 line 24 and
    // `karac check` passed, so the chain built and ran under AOT and JIT but
    // died on `karac run --interp` with "method 'request' not found on type
    // 'Client'". Same check-green/run-red split as the `File.sync_all`
    // durability gap. This pins that all four builder steps take effect —
    // a chain that dispatched but dropped its configuration would still
    // return 200 and hide the bug.
    let port = spawn_echo_origin();
    let output = run(&format!(
        r#"
fn main() with sends(Network) receives(Network) {{
    let c = Client.new();
    match c.request("PUT", "http://127.0.0.1:{port}/p")
           .header("X-A", "one")
           .header("X-B", "two")
           .body("payload42")
           .timeout(5000)
           .send() {{
        Ok(r) => {{
            println(r.status());
            println(r.body());
        }}
        Err(e) => println("err " + e.message()),
    }}
}}
"#
    ));
    assert_eq!(output, "200\nm=PUT;xa=one;xb=two;body=payload42\n");
}

#[test]
fn size_of_unsupported_type_arg_shape_is_runtime_error_not_panic() {
    // `size_of[Vec[i64]]()` is rejected at typecheck
    // (E_LAYOUT_QUERY_TYPE_ARG_REQUIRED — the nested-generic bracket
    // operand parses as indexing, not a type). `karac run` tolerates
    // typecheck errors, so evaluation must degrade to a runtime error
    // rather than fall through to variable lookup and panic.
    let errors = runtime_errors("fn main() { println(size_of[Vec[i64]]()); }");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]
            .message
            .contains("size_of requires a plain type argument"),
        "unexpected message: {}",
        errors[0].message
    );
}

#[test]
fn test_stdlib_wrapper_runtime_error_attributed_to_call_site() {
    // B-2026-07-21-17: a runtime error raised inside a spliced gated-stdlib
    // wrapper body (std.lazy's `lit`) carried the MODULE source's own
    // line/col — the wrapper items are cloned from the module's separate
    // parse — which the CLI then renders against the USER file's path
    // (`lit(vec![1i64])` reported "at <user file>:19:5", line 19 of
    // lazy.kara, in a 5-line program). The interpreter now attributes such
    // errors to the OUTERMOST wrapper call site: a real user-file location,
    // and where the faulting argument lives. Line 4 below is the `lit(v)`
    // call; pre-fix the span was the wrapper body's module line (18+). A
    // plain user-code error and an error AFTER a successful wrapper call
    // keep their raise-site spans (the wrapper stack is popped on return).
    let errors = runtime_errors(
        "import std.lazy.{lit};\n\
         fn main() {\n\
             let v: Vec[i64] = vec![1i64];\n\
             let _ = lit(v);\n\
         }",
    );
    let e = errors
        .iter()
        .find(|e| e.message.contains("LazyExpr.lit expects a scalar literal"))
        .expect("lit rejection error missing");
    assert_eq!(
        e.span.line, 4,
        "error must point at the user call site, got line {} (span {:?})",
        e.span.line, e.span
    );
    // Post-wrapper user error: attribution untouched (line 5, the v2[3]).
    let errors = runtime_errors(
        "import std.lazy.{lit};\n\
         fn main() {\n\
             let _ok = lit(5i64);\n\
             let v2: Vec[i64] = vec![];\n\
             let _ = v2[3];\n\
         }",
    );
    let e = errors
        .iter()
        .find(|e| e.message.contains("out of bounds"))
        .expect("oob error missing");
    assert_eq!(
        e.span.line, 5,
        "post-wrapper user error must keep its raise-site line, got {} ({:?})",
        e.span.line, e.span
    );
}

/// B-2026-07-30-5 (ineligible leg) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_deque_ineligible_shapes_slow_path`, same source and expected string.
#[test]
fn test_deque_ineligible_shapes_slow_path() {
    assert_eq!(
        run("fn make() -> VecDeque[i64] {\n\
                 let mut q: VecDeque[i64] = VecDeque.new();\n\
                 q.push_back(5);\n\
                 q.push_back(6);\n\
                 q\n\
             }\n\
             fn total(d: VecDeque[i64]) -> i64 {\n\
                 let mut acc = 0;\n\
                 let mut d2 = d;\n\
                 while not d2.is_empty() {\n\
                     match d2.pop_front() { Some(x) => { acc = acc + x; } None => {} }\n\
                 }\n\
                 acc\n\
             }\n\
             fn main() {\n\
                 let got = make();\n\
                 println(total(got));\n\
                 let mut r: VecDeque[i64] = VecDeque.new();\n\
                 r.push_back(7);\n\
                 r.push_back(8);\n\
                 println(r[0]);\n\
                 match r.pop_front() { Some(x) => { println(x); } None => {} }\n\
             }\n"),
        "11\n7\n7\n"
    );
}

/// B-2026-09-02-4 — the INTERPRETER twin of
/// `e2e_param_wrapped_in_returned_aggregate_on_some_paths_has_one_owner`, same
/// program and the same expected string: the row's two spellings diverged in
/// opposite directions between the backends, and the one predicate both read
/// now settles every cell to one body per path.
#[test]
fn test_param_wrapped_in_returned_aggregate_on_some_paths_has_one_owner() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct Box2 { r: R }
impl Drop for Box2 { fn drop(mut ref self) { println(f"B{self.r.id}") } }
struct P2 { r: R, n: i64 }
enum Slot { Held(R), Empty }
struct H { n: i64 }
fn mk(i: i64) -> String { return f"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }
fn mr(i: i64) -> R { return R { id: i, s: mk(i) }; }
fn fmake(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(77) }; } return Box2 { r: r }; }
fn fplain(r: R, k: bool) -> P2 { if k { return P2 { r: mr(78), n: 1 }; } return P2 { r: r, n: 2 }; }
fn ftup(r: R, k: bool) -> (R, i64) { if k { return (mr(79), 1); } return (r, 2); }
fn fslot(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Held(r); }
fn ftail(r: R, k: bool) -> P2 { if k { P2 { r: mr(80), n: 1 } } else { P2 { r: r, n: 2 } } }
fn fnest(r: R, k: bool, j: bool) -> P2 { if k { if j { return P2 { r: r, n: 3 }; } } return P2 { r: mr(81), n: 1 }; }
fn ftwo(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(82) }; } let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }; }
impl H {
    fn amake(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(87) }; } return Box2 { r: r }; }
    fn aplain(r: R, k: bool) -> P2 { if k { return P2 { r: mr(88), n: 1 }; } return P2 { r: r, n: 2 }; }
    fn mmake(ref self, r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(89) }; } return Box2 { r: r }; }
}
fn main() {
    let h = H { n: 0 };
    println("ft"); { let x = fmake(mr(55), true); println(f"C{x.r.id}"); }
    println("ff"); { let x = fmake(mr(53), false); println(f"C{x.r.id}"); }
    println("at"); { let x = H.amake(mr(56), true); println(f"C{x.r.id}"); }
    println("af"); { let x = H.amake(mr(57), false); println(f"C{x.r.id}"); }
    println("pt"); { let x = fplain(mr(58), true); println(f"C{x.r.id}"); }
    println("pf"); { let x = fplain(mr(59), false); println(f"C{x.r.id}"); }
    println("apt"); { let x = H.aplain(mr(60), true); println(f"C{x.r.id}"); }
    println("apf"); { let x = H.aplain(mr(61), false); println(f"C{x.r.id}"); }
    println("tt"); { let x = ftup(mr(62), true); println(f"C{x.0.id}"); }
    println("tf"); { let x = ftup(mr(63), false); println(f"C{x.0.id}"); }
    println("st"); { let x = fslot(mr(64), true); match x { Slot.Held(v) => println(f"C{v.id}"), Slot.Empty => println("CE") } }
    println("tlt"); { let x = ftail(mr(66), true); println(f"C{x.r.id}"); }
    println("tlf"); { let x = ftail(mr(67), false); println(f"C{x.r.id}"); }
    println("ntt"); { let x = fnest(mr(68), true, true); println(f"C{x.r.id}"); }
    println("ntf"); { let x = fnest(mr(69), true, false); println(f"C{x.r.id}"); }
    println("nff"); { let x = fnest(mr(70), false, false); println(f"C{x.r.id}"); }
    println("twt"); { let x = ftwo(mr(71), true); println(f"C{x.r.id}"); }
    println("mt"); { let x = h.mmake(mr(73), true); println(f"C{x.r.id}"); }
    println("mf"); { let x = h.mmake(mr(74), false); println(f"C{x.r.id}"); }
    println("nt"); { let a = mr(75); let x = fmake(a, true); println(f"C{x.r.id}"); }
    println("nf"); { let b = mr(76); let x = fmake(b, false); println(f"C{x.r.id}"); }
    println("end");
}"#),
        "ft\nd55\nC77\nB77\nd77\nff\nC53\nB53\nd53\nat\nd56\nC87\nB87\nd87\naf\nC57\nB57\nd57\npt\nd58\nC78\nd78\npf\nC59\nd59\napt\nd60\nC88\nd88\napf\nC61\nd61\ntt\nd62\nC79\nd79\ntf\nC63\nd63\nst\nd64\nCE\ntlt\nd66\nC80\nd80\ntlf\nC67\nd67\nntt\nC68\nd68\nntf\nd69\nC81\nd81\nnff\nd70\nC81\nd81\ntwt\nd71\nC82\nB82\nd82\nmt\nd73\nC89\nB89\nd89\nmf\nC74\nB74\nd74\nnt\nd75\nC77\nB77\nd77\nnf\nC76\nB76\nd76\nend\n",
        "a param wrapped in a returned aggregate on some paths has one owner per path"
    );
}

/// B-2026-09-07-22 — the interpreter was correct on this row's cells throughout;
/// the twin holds the compiled string to it.
///
/// Twin of `tests/codegen.rs`'s `e2e_mixed_path_hop_on_a_method`, pinned to the same string.
#[test]
fn test_mixed_path_hop_on_a_method() {
    assert_eq!(
        run(r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn fwd(r: R) -> R { return r; }
struct Hold { n: i64 }
impl Hold { fn pick2(ref self, r: R, k: bool) -> R { if k { return mk(90); } return fwd(r); } }
impl R { fn picka2(r: R, k: bool) -> R { if k { return mk(91); } return fwd(r); } }
impl Hold { fn pick(ref self, r: R, k: bool) -> R { if k { return mk(92); } return r; } }
fn main() {
  let h = Hold { n: 0 };
  println("method_hop_handback"); let a = h.pick2(mk(1), false); println(f"  v={a.inner.v}");
  println("method_hop_dies"); let b = h.pick2(mk(2), true); println(f"  v={b.id}");
  println("assoc_hop_handback"); let c = R.picka2(mk(3), false); println(f"  v={c.inner.v}");
  println("assoc_hop_dies"); let d = R.picka2(mk(4), true); println(f"  v={d.id}");
  println("method_nohop_handback"); let e = h.pick(mk(5), false); println(f"  v={e.inner.v}");
  println("named_into_method_hop"); let g = mk(6); let n = h.pick2(g, false); println(f"  v={n.inner.v}");
  println("end");
}
"#),
        r#"method_hop_handback
  v=1
  dR1
method_hop_dies
  dR2
  v=90
  dR90
assoc_hop_handback
  v=3
  dR3
assoc_hop_dies
  dR4
  v=91
  dR91
method_nohop_handback
  v=5
  dR5
named_into_method_hop
  v=6
  dR6
end
"#
    );
}

/// B-2026-09-07-15 — the interpreter ran the body twice for the mixed-path hop's
/// hand-back leg (it reads the same predicate the compiled backends do), so this
/// is a fix pin on both sides.
///
/// Twin of `tests/codegen.rs`'s `e2e_mixed_path_hand_back_through_a_hop`, pinned to the same string.
#[test]
fn test_mixed_path_hand_back_through_a_hop() {
    assert_eq!(
        run(r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
fn f(r: R) -> R { return r; }
fn pf(p: P) -> P { return p; }
fn mvia(r: R, c: bool) -> R { if c { return f(r); } return mk(90); }
fn cvia(r: R, c: bool) -> R { if c { return r; } return mk(91); }
fn pvia(p: P, c: bool) -> P { if c { return pf(p); } return mkp(92); }
fn main() {
  println("hop_handback"); let a = mk(1); let z = mvia(a, true); println(f"  v={z.inner.v}");
  println("hop_dies_inside"); let b = mk(2); let y = mvia(b, false); println(f"  v={y.id}");
  println("hop_handback_fresh"); let w = mvia(mk(3), true); println(f"  v={w.inner.v}");
  println("hop_dies_fresh"); let x = mvia(mk(4), false); println(f"  v={x.id}");
  println("nohop_handback"); let c = mk(5); let u = cvia(c, true); println(f"  v={u.inner.v}");
  println("nohop_dies_inside"); let d = mk(6); let t = cvia(d, false); println(f"  v={t.id}");
  println("copyable_hop_handback"); let e = mkp(7); let s = pvia(e, true); println(f"  v={s.id}");
  println("copyable_hop_dies"); let g = mkp(8); let r = pvia(g, false); println(f"  v={r.id}");
  println("end");
}
"#),
        r#"hop_handback
  v=1
  dR1
hop_dies_inside
  dR2
  v=90
  dR90
hop_handback_fresh
  v=3
  dR3
hop_dies_fresh
  dR4
  v=90
  dR90
nohop_handback
  v=5
  dR5
nohop_dies_inside
  dR6
  v=91
  dR91
copyable_hop_handback
  v=7
  dP7
copyable_hop_dies
  dP8
  v=92
  dP92
end
"#
    );
}
