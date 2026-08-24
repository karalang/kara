//! `dbg()` runtime support — design.md § `dbg()`.
//!
//! Four entry points backing the compiled backends' `dbg` lowering
//! (B-2026-08-23-18). Until that lowering existed, `karac build` / `karac run`
//! REFUSED any program containing `dbg` and only the tree-walk interpreter ran
//! it, so this module is what makes the three backends agree.
//!
//! The two `quote` helpers exist so the compiled backends and the interpreter
//! produce byte-identical `Debug` text **by construction rather than by
//! convention** — both sides end up inside Rust's own `{:?}`, which is exactly
//! what the interpreter's `Value::debug_fmt` calls. This is the same rule the
//! Arrow IPC twin and `String.normalize` follow: when two backends must agree
//! on a text format, they link the same Rust code rather than reimplementing it
//! on each side. Codegen quotes ONLY at the `String` / `char` leaves — every
//! compound shape (struct, tuple, Vec, enum, Map, …) renders identically in
//! `Debug` and `Display`, so the existing Display walker serves both modes.
//!
//! `karac_dbg_emit`'s write deliberately does NOT go through
//! `karac_runtime_write_console`. design.md § `dbg()` ("Per-line atomicity, not
//! ordering") makes `dbg` exempt from the console chokepoint: transparent-verb
//! output is not captured per-branch and replayed at a join, because doing so
//! would serialize at join points and defeat the transparent designation. Each
//! call is one `write(2)` of the complete line, which POSIX makes atomic up to
//! `PIPE_BUF`, so sibling tasks' lines never tear even though they may
//! interleave. The interpreter's `eval_builtin_dbg` writes its line the same
//! way, with one `write_all` under a held `stderr` lock.

use core::sync::atomic::{AtomicU64, Ordering};

/// Monotonic source-order task-id allocator, shared by every `par` region in
/// the process. Starts at 1 — id 0 is the "not inside a `par`" sentinel and is
/// never reported as a tag, matching the interpreter's counter exactly (see
/// `eval_par_block`, "Counter starts at 1 (id 0 is the 'no par' sentinel)").
static TASK_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The task id of the `par` branch running on this thread, or 0 at the top
    /// level. Installed per branch by `TaskIdGuard`, which restores the
    /// enclosing value on drop so a NESTED `par` returns to its outer branch's
    /// id rather than to 0 (the `OutputRedirectGuard` discipline).
    static CURRENT_TASK_ID: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

/// Reserve `count` consecutive task ids for one `par` region, returning the
/// first. Called once at region entry so ids are handed out in SOURCE order
/// (branch `i` gets `base + i`) regardless of the order the OS actually
/// schedules the branches — the same pre-assignment rule the interpreter uses,
/// which is what makes a given branch report a stable id across runs.
pub(crate) fn reserve_task_ids(count: u64) -> u64 {
    TASK_ID_COUNTER.fetch_add(count, Ordering::Relaxed)
}

/// RAII install of a branch's task id, restoring the enclosing one on drop so
/// the nesting unwinds correctly even if the branch panics.
pub(crate) struct TaskIdGuard {
    prev: u64,
}

impl TaskIdGuard {
    pub(crate) fn new(id: u64) -> Self {
        let prev = CURRENT_TASK_ID.try_with(|c| c.replace(id)).unwrap_or(0);
        TaskIdGuard { prev }
    }
}

impl Drop for TaskIdGuard {
    fn drop(&mut self) {
        let _ = CURRENT_TASK_ID.try_with(|c| c.set(self.prev));
    }
}

/// The task id of the currently running `par` branch, or 0 outside any `par`.
/// `karac_dbg_emit` prefixes `[task:N …]` (terminal) / sets `"task_id":N`
/// (structured) when this is non-zero — the interpreter's
/// `current_task_id: Option<u64>` in a different spelling. Internal: codegen
/// never reads it directly, since the whole envelope is assembled here.
fn current_task_id() -> u64 {
    CURRENT_TASK_ID.try_with(|c| c.get()).unwrap_or(0)
}

/// `format!("{:?}", s)` for a Kāra `String` / `str` — the quoted, escaped
/// `Debug` spelling of a string leaf. Returns a malloc'd NUL-terminated buffer
/// (caller frees) with the byte length in `out_len`, the `alloc_string_result`
/// convention every other string-returning runtime entry point uses.
///
/// # Safety
/// `data` must point to `len` valid UTF-8 bytes; `out_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn karac_dbg_quote_str(
    data: *const u8,
    len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    unsafe {
        let s = crate::clone::str_from_raw(data, len);
        let quoted = format!("{:?}", s);
        crate::clone::alloc_string_result(quoted.as_bytes(), out_len)
    }
}

/// `format!("{:?}", c)` for a Kāra `char` — the quoted, escaped `Debug`
/// spelling of a char leaf (`'x'`, `'\n'`, `'\u{1f600}'`). Same buffer
/// convention as `karac_dbg_quote_str`.
///
/// An out-of-range code point renders as U+FFFD rather than aborting: this is a
/// diagnostic path, and a corrupt value should still print something rather
/// than take the program down.
///
/// # Safety
/// `out_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn karac_dbg_quote_char(cp: u32, out_len: *mut i64) -> *mut u8 {
    unsafe {
        let c = char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER);
        let quoted = format!("{:?}", c);
        crate::clone::alloc_string_result(quoted.as_bytes(), out_len)
    }
}

/// Terminal (default) or structured JSON `dbg` output, chosen once per process.
///
/// The interpreter takes this from the CLI's `--output` flag
/// (`OutputMode::Json | Jsonl => DbgOutputMode::Json`). A compiled binary has
/// no such flag — it runs standalone — so the compiled backends read
/// `KARAC_DBG_OUTPUT` instead, and `karac run --output=json` sets it for the
/// JIT'd program. Same two formats either way; only the way the choice arrives
/// differs, because the two surfaces genuinely differ in what is available.
fn json_mode() -> bool {
    use std::sync::OnceLock;
    static MODE: OnceLock<bool> = OnceLock::new();
    *MODE.get_or_init(|| {
        matches!(
            std::env::var("KARAC_DBG_OUTPUT").as_deref(),
            Ok("json") | Ok("jsonl")
        )
    })
}

/// JSON string escape — byte-for-byte the interpreter's `dbg_json_escape`
/// (`src/interpreter.rs`), including the `\u{:04x}` form for the other C0
/// control characters. The two must agree: a `dbg` line is compared across
/// backends by the A/B rule, and the escape is the only part of the JSON
/// envelope with any freedom in it.
fn json_escape(s: &str, out: &mut String) {
    use std::fmt::Write;
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Format and emit one `dbg()` line — the compiled backends' twin of the
/// interpreter's `eval_builtin_dbg` tail (B-2026-08-23-18).
///
/// Codegen renders the VALUE (through the synthesized `Debug` function) and
/// hands the four text pieces here; the envelope — terminal vs JSON, the
/// `[task:N …]` tag, the trailing newline, and the single atomic `write(2)` —
/// is assembled in one place rather than open-coded in LLVM IR. That keeps the
/// two output formats and the task tagging as ordinary Rust, next to the
/// escape function they share.
///
/// # Safety
/// Each `(ptr, len)` pair must describe valid UTF-8 bytes, or be `(null, 0)`.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn karac_dbg_emit(
    file: *const u8,
    file_len: i64,
    line: i64,
    expr: *const u8,
    expr_len: i64,
    ty: *const u8,
    ty_len: i64,
    value: *const u8,
    value_len: i64,
) {
    unsafe {
        let file = crate::clone::str_from_raw(file, file_len);
        let expr = crate::clone::str_from_raw(expr, expr_len);
        let ty = crate::clone::str_from_raw(ty, ty_len);
        let value = crate::clone::str_from_raw(value, value_len);
        let task = current_task_id();

        let mut out = String::with_capacity(file.len() + expr.len() + ty.len() + value.len() + 48);
        if json_mode() {
            out.push_str("{\"kind\":\"dbg\",\"task_id\":");
            if task == 0 {
                out.push_str("null");
            } else {
                out.push_str(&task.to_string());
            }
            out.push_str(",\"file\":");
            json_escape(file, &mut out);
            out.push_str(",\"line\":");
            out.push_str(&line.to_string());
            out.push_str(",\"expr\":");
            json_escape(expr, &mut out);
            out.push_str(",\"type\":");
            json_escape(ty, &mut out);
            out.push_str(",\"value\":");
            json_escape(value, &mut out);
            out.push_str("}\n");
        } else {
            out.push('[');
            if task != 0 {
                out.push_str("task:");
                out.push_str(&task.to_string());
                out.push(' ');
            }
            out.push_str(file);
            out.push(':');
            out.push_str(&line.to_string());
            out.push_str("] ");
            out.push_str(expr);
            out.push_str(" = ");
            out.push_str(value);
            out.push('\n');
        }
        crate::fatal::write_stderr(out.as_bytes());
    }
}
