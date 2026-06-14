//! Browser-side wasm-bindgen wrapper for `karac::run_playground`.
//!
//! Exposes a single entry point — [`run`] — that the playground page's
//! JS shell calls with the editor source. Returns a JSON envelope:
//!
//! ```text
//! { ok: bool,
//!   stdout: string[],
//!   diagnostics: [
//!     { phase: "parse"|"resolve"|"typecheck"|"effect"|"ownership"|"runtime",
//!       message: string, line: number, column: number,
//!       offset: number, length: number }
//!   ] }
//! ```
//!
//! The shape matches `karac::PlaygroundResult` verbatim via
//! `serde-wasm-bindgen`. The JS shell renders `stdout` into the output
//! pane and decorates the editor with `diagnostics`.
//!
//! See `karac::run_playground` (src/lib.rs) for the pipeline contract
//! and slice 1 unit tests. Tracker line 703.

use serde::Serialize;
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
struct JsDiagnostic {
    phase: &'static str,
    message: String,
    line: usize,
    column: usize,
    offset: usize,
    length: usize,
}

#[derive(Serialize)]
struct JsResult {
    ok: bool,
    stdout: Vec<String>,
    diagnostics: Vec<JsDiagnostic>,
}

/// Install a panic hook that routes Rust panics to `console.error` in
/// the browser devtools. Without this, panics in the wasm module
/// become opaque `RuntimeError: unreachable executed` messages with no
/// payload. Idempotent — safe to call from every `run`.
fn install_panic_hook() {
    console_error_panic_hook::set_once();
}

/// Run a Kāra source string through the full check pipeline +
/// interpreter, returning a structured result for the JS playground
/// shell to render.
///
/// Errors in any phase are returned in `diagnostics`; the function
/// itself never throws (modulo a host-level panic, which the panic hook
/// routes to `console.error`).
#[wasm_bindgen]
pub fn run(source: &str) -> Result<JsValue, JsValue> {
    install_panic_hook();
    let result = karac::run_playground(source);
    let envelope = JsResult {
        ok: result.ok,
        stdout: result.stdout,
        diagnostics: result
            .diagnostics
            .into_iter()
            .map(|d| JsDiagnostic {
                phase: d.phase,
                message: d.message,
                line: d.line,
                column: d.column,
                offset: d.offset,
                length: d.length,
            })
            .collect(),
    };
    serde_wasm_bindgen::to_value(&envelope).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[derive(Serialize)]
struct JsCartographDiagnostic {
    phase: &'static str,
    message: String,
    line: usize,
    column: usize,
    offset: usize,
    length: usize,
}

#[derive(Serialize)]
struct JsCartograph {
    ok: bool,
    /// Whole-program effect-graph JSON envelope (nodes + call edges),
    /// byte-identical to `karac query effects <file>`. Empty string on a
    /// fatal parse/resolve error. The JS shell `JSON.parse`s it.
    effects: String,
    /// Whole-program concurrency JSON envelope (parallel bands),
    /// byte-identical to `karac query concurrency <file>`.
    concurrency: String,
    diagnostics: Vec<JsCartographDiagnostic>,
}

/// Build the whole-program effect graph for `source` in the browser —
/// the Cartographer studio's live re-query entry point. Returns the two
/// JSON envelopes (effects + concurrency) the CLI `query` commands emit,
/// plus diagnostics for editor decoration. Never throws (modulo a host
/// panic, routed to `console.error` by the panic hook); a fatal
/// parse/resolve error comes back as `ok:false` with empty envelopes and
/// populated `diagnostics`.
#[wasm_bindgen]
pub fn cartograph(source: &str) -> Result<JsValue, JsValue> {
    install_panic_hook();
    let result = karac::effect_graph::cartograph_json(source, "studio.kara");
    let envelope = JsCartograph {
        ok: result.ok,
        effects: result.effects_json,
        concurrency: result.concurrency_json,
        diagnostics: result
            .diagnostics
            .into_iter()
            .map(|d| JsCartographDiagnostic {
                phase: d.phase,
                message: d.message,
                line: d.line,
                column: d.column,
                offset: d.offset,
                length: d.length,
            })
            .collect(),
    };
    serde_wasm_bindgen::to_value(&envelope).map_err(|e| JsValue::from_str(&e.to_string()))
}
