//! Line-based REPL over the tree-walk interpreter.
//!
//! Implements `karac repl` (P0 delivery item per `roadmap.md § Interactive
//! Development`). Each cell is wrapped as the body of an implicit
//! `fn main()` per `design.md § Interactive Evaluation Model > Cell Scope`,
//! with top-level items (fn / struct / enum / trait / impl / const)
//! accumulating across the session.
//!
//! Surface (v1 MVP):
//! - Read-eval-print loop with multi-line continuation (brace-balance probe).
//! - Persistent top-level items across cells.
//! - Meta-commands: `:help`, `:quit`, `:type <expr>`, `:effects`, `:save <file>`.
//! - Last expression in a cell auto-prints if it's not `Unit`.
//!
//! Persistent `let` bindings (v1 source-replay model):
//! - Top-level `let` / `let mut` statements from a successfully-evaluated
//!   statement cell are extracted (verbatim source) and prepended to every
//!   subsequent synthetic `fn main()` body. A later cell that references
//!   `x` resolves it against the replayed `let x = ...;` from cell N.
//! - Caveat: side effects on the RHS re-run on every cell (the value is
//!   recomputed, not snapshotted). Pure literal / arithmetic bindings
//!   behave like a notebook; `let x = read_file(...)` re-reads the file.
//!   Full value-snapshot semantics ship with the Jupyter kernel CR
//!   (interpreter surgery scheduled separately).
//! - `:reset` clears the persistent-let buffer for users who want to
//!   recover from an expensive replay.
//!
//! Other v1 limitations (called out in `:help`):
//! - Mutation in a statement cell (`x += 1;`) does not survive to the
//!   next cell — only `let` source replays. Re-binding (`let x = x + 1;`)
//!   is the supported idiom for now.
//! - `--auto-clone`, notebook-aware use-after-move hints, and rich Jupyter
//!   display all ship as separate items in the Interactive Development
//!   chapter of `roadmap.md`.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::interpreter::Value;

mod display;
#[cfg(feature = "lljit_prototype")]
mod jit_runner_client;
mod util;

/// Slice c-repl.B.B: split a runner-captured stdout byte slice into
/// the `Vec<String>` shape `WrapperOutcome` expects. The interpreter
/// populates `captured_output` one line at a time; the JIT runner
/// returns the raw bytes from a tempfile, so we split on `\n` and
/// drop the trailing empty string a `\n`-terminated buffer would
/// produce.
#[cfg(feature = "lljit_prototype")]
fn bytes_to_stdout_lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    if lines.last().map(|s| s.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}
pub use display::{render_display, render_text_plain, DisplayBundle};
use util::*;

/// Caller-supplied options for `run_with_options` — mirrors the flags
/// surfaced on the `karac repl` subcommand. `Default` matches the
/// historical bare-`karac repl` behavior so existing callers keep working.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReplOptions {
    /// `--auto-clone`: when on, the REPL auto-inserts `.clone()` at
    /// cross-cell consume sites flagged by `UseAfterMove`, rather than
    /// surfacing the diagnostic. Each insertion emits a
    /// `perf[auto-clone-in-repl]` note (never silent — design.md §
    /// Interactive Evaluation Model > Ownership Across Cells).
    pub auto_clone: bool,
}

/// Run the interactive REPL until EOF or `:quit`. Returns once the user
/// exits; never reaches the rest of the CLI dispatch.
pub fn run() {
    run_with_options(ReplOptions::default());
}

/// Slice c-repl.B.B: produce the `(JIT)` banner tag when JIT mode is
/// on, `None` otherwise. Reads the session's `jit_enabled` snapshot
/// taken at construction so the banner can't disagree with the
/// dispatch path the session will actually take.
#[cfg(feature = "lljit_prototype")]
fn jit_banner_tag(session: &Session) -> Option<String> {
    if session.jit_enabled() {
        Some("JIT".to_string())
    } else {
        None
    }
}

#[cfg(not(feature = "lljit_prototype"))]
fn jit_banner_tag(_session: &Session) -> Option<String> {
    None
}

/// Surface for the binary entry point: launch the REPL with caller-supplied
/// options. Used by `karac repl` flag wiring (`--auto-clone`).
pub fn run_with_options(opts: ReplOptions) {
    let mut session = Session::with_options(opts);
    // Slice c-repl.B.B: surface JIT mode in the banner so users know
    // KARAC_REPL_JIT=1 actually took effect — silent activation is a
    // worse failure mode than a missing flag because the cells run
    // either way and the divergence only shows up on side-effect
    // semantics.
    let jit_tag = jit_banner_tag(&session);
    let mut tags: Vec<&str> = Vec::new();
    if opts.auto_clone {
        tags.push("auto-clone on");
    }
    if let Some(tag) = jit_tag.as_deref() {
        tags.push(tag);
    }
    let banner = if tags.is_empty() {
        "Kāra REPL  (type :help for commands, :quit to exit)".to_string()
    } else {
        format!(
            "Kāra REPL  ({}; type :help for commands, :quit to exit)",
            tags.join("; ")
        )
    };
    println!("{banner}");

    let mut editor = match DefaultEditor::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: failed to start line editor: {e}");
            return;
        }
    };

    loop {
        let primary = "> ";
        let cont = ".. ";
        let mut buffer = String::new();
        let mut prompt = primary;

        let line = match editor.readline(prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                eprintln!("error: {e}");
                continue;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        // Meta-command short-circuit (only on the first line of a cell).
        if line.starts_with(':') {
            let _ = editor.add_history_entry(&line);
            if !session.dispatch_meta(line.trim()) {
                break;
            }
            continue;
        }

        buffer.push_str(&line);
        buffer.push('\n');
        prompt = cont;

        // Brace/paren balance probe — fall into multi-line mode while the
        // input is structurally incomplete. Strings and comments are coarse-
        // tracked so braces inside them don't trip the probe.
        while !is_balanced(&buffer) {
            let next = match editor.readline(prompt) {
                Ok(l) => l,
                Err(ReadlineError::Interrupted) => {
                    buffer.clear();
                    break;
                }
                Err(ReadlineError::Eof) => break,
                Err(e) => {
                    eprintln!("error: {e}");
                    buffer.clear();
                    break;
                }
            };
            buffer.push_str(&next);
            buffer.push('\n');
        }

        if buffer.trim().is_empty() {
            continue;
        }

        let _ = editor.add_history_entry(buffer.trim_end());
        session.evaluate_cell(&buffer);
    }
}

/// Result of `Session::dispatch_magic` — the rendered display text
/// plus a flag indicating whether the kernel should treat this as an
/// error reply. The Jupyter kernel (phase-5 tracker § "Jupyter kernel
/// MVP", `[x]` shipped 2026-05-19) routes `ok` true results into
/// `display_data` / `execute_result` and `ok` false results into
/// `error` replies with the carried text as the traceback body. The
/// text is line-oriented and pre-trimmed so a kernel can splice it
/// into its output channel without re-formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicOutput {
    pub text: String,
    pub ok: bool,
    /// Optional rich-display mime bundle. `Some` only for magics that
    /// produce a value-render (today: `%show`); `None` for text-only
    /// magics (`%effects`, `%ownership`, `%explain`, `%set`, …). When
    /// `Some`, the Jupyter kernel broadcasts the bundle through
    /// `display_data` so the frontend can pick the richest mime it
    /// understands (e.g. `text/html` table for `Vec[Struct]`). When
    /// `None`, the kernel falls back to `stream(stdout)` with the
    /// plain `text` field.
    pub rich: Option<DisplayBundle>,
}

impl MagicOutput {
    /// Construct a successful magic reply.
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: true,
            rich: None,
        }
    }

    /// Construct an error magic reply. The kernel maps the text into
    /// the `error` channel's traceback body.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: false,
            rich: None,
        }
    }

    /// Construct a successful reply that carries a rich-display mime
    /// bundle alongside the plain text. The `text` argument is what
    /// non-rich-aware consumers (REPL, terse log lines) read; the
    /// `bundle` carries the same content plus any extra mime forms
    /// (`text/html` table for `Vec[Struct]`, etc.) the kernel can
    /// broadcast through `display_data`. The kernel reads `bundle`
    /// preferentially when present.
    pub fn ok_rich(text: impl Into<String>, bundle: DisplayBundle) -> Self {
        Self {
            text: text.into(),
            ok: true,
            rich: Some(bundle),
        }
    }
}

/// Active cross-cell `:provide R = expr` scope. One frame per nested
/// `:provide` call on the session's provider stack; LIFO close order
/// (innermost-first) is enforced at `:end-provide` time. The expression
/// source is stored verbatim so slice 3's wrapping mechanism can splice
/// it into a `with_provider[R](expr, || { … })` block around each
/// subsequent statement-cell body, and so `:save` export can emit the
/// same `with_provider` shape in the rendered `.kara` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFrame {
    /// Resource identifier from `:provide R = …` (validated to be a
    /// Kāra-shaped identifier at parse time).
    pub resource: String,
    /// Verbatim RHS of the `:provide` form. Re-evaluated each subsequent
    /// cell run by the wrapping mechanism; design.md positions this as
    /// the same provider-stack mechanism the runtime uses elsewhere.
    pub expr_src: String,
    /// 1-based cell index at which the `:provide` was issued. Powers the
    /// notebook-aware "declared inside `:provide R` (cell N)" tail on
    /// slice 4's "cannot find value declared inside closed scope"
    /// diagnostic. Set to `cell_history.len() + 1` at push time so it
    /// names the cell the user will see next (meta-commands themselves
    /// don't enter `cell_history`).
    pub opened_cell: usize,
}

/// Internal node type for the saved-session export tree. Built by
/// `render_exported_main_body_with_providers` from the interleaved
/// `cell_history` + `provider_history` timeline; rendered recursively
/// with one level of indentation per nesting depth.
enum ExportNode {
    Statement(String),
    Block {
        resource: String,
        expr_src: String,
        children: Vec<ExportNode>,
    },
}

fn render_export_nodes(nodes: &[ExportNode], indent: usize) -> String {
    let pad = "    ".repeat(indent);
    let mut s = String::new();
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        match node {
            ExportNode::Statement(src) => {
                for line in src.lines() {
                    if line.trim().is_empty() {
                        s.push('\n');
                    } else {
                        s.push_str(&pad);
                        s.push_str(line);
                        s.push('\n');
                    }
                }
            }
            ExportNode::Block {
                resource,
                expr_src,
                children,
            } => {
                s.push_str(&pad);
                s.push_str("with_provider[");
                s.push_str(resource);
                s.push_str("](");
                s.push_str(expr_src);
                s.push_str(", || {\n");
                s.push_str(&render_export_nodes(children, indent + 1));
                s.push_str(&pad);
                s.push_str("});\n");
            }
        }
    }
    s
}

/// Record of a persistent `let` binding that was pruned from the
/// session because its originating `:provide` scope closed. Drives the
/// notebook-aware tail on subsequent `undefined name '...'` resolver
/// errors so the user sees *why* the binding vanished, not just that
/// it's gone (design.md § Cross-Cell Providers diagnostic shape).
#[derive(Debug, Clone)]
struct PrunedProviderLet {
    binding_name: String,
    declared_in_cell: usize,
    pruned_by_resource: String,
    scope_opened_cell: usize,
    pruned_at_cell: usize,
}

/// Event log entry recording one `:provide` / `:end-provide` meta-command
/// in submission order. `cells_seen` is `cell_history.len()` at the moment
/// the meta-command ran — meta-commands themselves don't enter
/// `cell_history`, so this number names the position the next user cell
/// will land at. The export-rendering path interleaves these events with
/// `cell_history` to rebuild the nested `with_provider` shape from the
/// saved-session form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderHistoryEntry {
    pub cells_seen: usize,
    pub kind: ProviderHistoryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderHistoryKind {
    Open { resource: String, expr_src: String },
    Close { resource: String },
}

/// Result of `Session::evaluate_cell_captured` — captured stdout plus any
/// parse / resolve / type errors. Used by integration tests; the
/// production `evaluate_cell` writes directly to the process's stdout/
/// stderr instead.
#[derive(Debug, Default)]
pub struct EvaluatedCell {
    pub stdout: String,
    pub errors: Vec<String>,
    /// Compiler-surface notes that aren't fatal — today this carries the
    /// `perf[auto-clone-in-repl]` lines emitted when `--auto-clone` rewrites
    /// a consume site. Always non-empty when an insertion fired (the spec
    /// requires the channel be visible — never silent). Production
    /// `evaluate_cell` mirrors each entry to stderr so users see them.
    pub notes: Vec<String>,
    /// Per-cell effect set rendered as the `writes(A, B) reads(C)` text
    /// the Jupyter kernel uses to display a cell's effect footer
    /// automatically after every execution. Empty for cells whose
    /// inferred effects on `main` (or accumulated items) are empty —
    /// the kernel suppresses the footer in that case so pure cells
    /// stay visually quiet. Computed by `compute_cell_effect_footer`
    /// after the cell's interpreter run succeeds; the same
    /// `render_effect_decls` helper that drives `:save`'s declared-
    /// effects rendering keeps the format stable.
    pub effect_footer: String,
}

/// Internal result type for `run_with_wrapper_inner` — bundles captured
/// stdout, diagnostic strings, and auto-clone perf notes so the caller
/// can route each to the right surface (stderr / stdout / `EvaluatedCell`
/// fields). Unlike `EvaluatedCell`, both `Ok` and `Err` arms can carry
/// notes — `--auto-clone` emits perf notes on every insertion regardless
/// of whether the cell ultimately compiled cleanly.
struct WrapperOutcome {
    stdout: Vec<String>,
    errors: Vec<String>,
    notes: Vec<String>,
}

impl WrapperOutcome {
    fn errors(errors: Vec<String>, notes: Vec<String>) -> Self {
        Self {
            stdout: Vec::new(),
            errors,
            notes,
        }
    }
}

/// Per-session state: accumulated source for top-level items + cell history
/// captured for `:save`. Public so integration tests can exercise the cell
/// pipeline without driving rustyline through a TTY.
pub struct Session {
    /// Concatenated source text of every top-level item (fn / struct / enum /
    /// trait / impl / const / type / distinct / extern / use / import) seen
    /// in any prior cell, in submission order. Replayed at the head of each
    /// new synthetic program so later cells resolve names from earlier ones.
    items_source: String,
    /// Verbatim cell inputs in submission order, used by `:save` to write a
    /// `.kara` file that reconstructs the session. Excludes meta-commands.
    cell_history: Vec<String>,
    /// Source slices of every top-level `let` / `let mut` / `let ... else`
    /// statement extracted from successfully-evaluated statement cells, in
    /// submission order. Re-emitted at the head of every subsequent
    /// synthetic `fn main()` body so cell N's `let x = 5;` is in scope when
    /// cell N+1 evaluates `println(x);`. v1 source-replay model — values
    /// are recomputed each cell rather than snapshotted; see the module
    /// docs for the caveat.
    persistent_lets: Vec<String>,
    /// 1-based origin cell index for each entry in `persistent_lets`. Length
    /// is invariant-equal to `persistent_lets.len()`. Recorded so the
    /// REPL-aware diagnostic-rendering path can map a span landing inside
    /// a replayed `let` slice back to the cell that originally introduced
    /// the binding (drives the "consumed by cell N" tail on cross-cell
    /// `UseAfterMove` errors). The 1-based convention matches what users
    /// see in cell labels (cell 1, cell 2, …); cell 0 is reserved for
    /// synthetic / wrapper-introduced source that does not correspond to
    /// any user cell.
    persistent_let_origin: Vec<usize>,
    /// Byte ranges, in the most recently built synthetic-program source,
    /// that map back to a user cell. Each entry is `(cell_idx_1_based,
    /// byte_range)`. Re-populated at the top of every `run_with_wrapper_inner`
    /// / `evaluate_cell_captured` call, before the parser runs, so the
    /// downstream diagnostic-rendering layer can call `cell_for_span`
    /// against fresh data. Ranges are non-overlapping and source-ordered
    /// so a binary search over them recovers the cell for any span. The
    /// next slice (`--auto-clone` opt-in mode) consumes the same data to
    /// decide which cell's source to rewrite when inserting `.clone()`;
    /// the shape here is deliberately rich enough to support that without
    /// surfacing helper API the auto-clone slice doesn't need yet.
    cell_byte_ranges: Vec<(usize, Range<usize>)>,
    /// `--auto-clone` opt-in mode. When `true`, the REPL pipeline detects
    /// cross-cell `UseAfterMove` errors (using the same `cell_for_span`
    /// machinery that drives the notebook-aware diagnostic), rewrites the
    /// consume-site cell to insert `.clone()` after the consumed binding,
    /// and re-runs the cell pipeline. The rewrite is recorded in
    /// `cell_history[M-1]` AND the matching `persistent_lets[i]` slot so
    /// the inserted `.clone()` survives both `:save` export and subsequent
    /// cell compilations. Each insertion emits a `perf[auto-clone-in-repl]`
    /// note onto the cell's `notes` channel — never silent (design.md §
    /// Interactive Evaluation Model > Ownership Across Cells).
    auto_clone: bool,
    /// Cached `Value`s from prior cells' top-level `let` bindings,
    /// keyed by binding name. Drives the value-snapshot persistent-let
    /// model: cell N's `let x = expensive();` records the bound value
    /// here; cell N+1's source-replay re-emits the same `let x =
    /// expensive();` slice into the synthetic main, but the
    /// interpreter consults `let_value_overrides` and uses this cached
    /// value instead of re-running `expensive()`. The source-replay
    /// form is what the parser / resolver / typechecker see (so `x`
    /// resolves and has a recorded type); only the interpreter's
    /// runtime evaluation of the RHS is short-circuited. Pattern lets
    /// (`let (a, b) = …`, `let-else`) and `let mut` rebindings stay on
    /// the source-replay path because keying the override on a single
    /// name does not cover them; this map only holds entries for
    /// simple `PatternKind::Binding` lets, matching the interpreter
    /// hook's classification.
    let_snapshots: HashMap<String, Value>,
    /// In-memory `[dependencies]` table built up via the `:dep` meta-command.
    /// Each entry is `name → normalized TOML value` (e.g. `"\"1.2\""` for a
    /// bare semver, `"{ git = \"...\" }"` for an inline-table form). v1
    /// stores the request only — package resolution / download / symbol
    /// registration is the package manager's responsibility (design.md
    /// tags `:dep` as v1.1; the resolver, registry proxy, and lock-file
    /// machinery have not landed yet). The buffer here is the integration
    /// point: when the package manager arrives, it consumes this map at
    /// the head of each subsequent cell to register surface symbols.
    pending_deps: BTreeMap<String, String>,
    /// Active cross-cell `:provide R = expr` scopes, in nesting order
    /// (outermost first). `:provide` pushes after eagerly validating the
    /// construction expression in the current cell — if construction
    /// fails or panics, the frame is NOT pushed (matching design.md §
    /// Cross-Cell Providers's "panic doesn't open the scope" guarantee).
    /// `:end-provide R` pops the innermost frame; LIFO mismatch
    /// surfaces a structured error and leaves the stack untouched.
    /// Slice 3 reads this stack at cell-run time to wrap subsequent
    /// statement-cell bodies in nested `with_provider[R](expr, || { … })`
    /// blocks, and slice 4 uses the `opened_cell` field for the
    /// notebook-aware "declared inside closed provider scope"
    /// diagnostic.
    provider_stack: Vec<ProviderFrame>,
    /// Submission-ordered log of every `:provide` / `:end-provide`
    /// meta-command. Drives the saved-session export's nested
    /// `with_provider` shape — each open/close pair compiles to one
    /// `with_provider[R](expr, || { /* cells in scope */ })` block in
    /// the rendered `.kara` file, with the cells that ran between the
    /// pair living inside the closure body (design.md § Cross-Cell
    /// Providers: "cell boundaries do not survive export"). Empty for
    /// sessions that never opened a scope, in which case the export
    /// path falls back to the flat `render_main_body` form unchanged.
    provider_history: Vec<ProviderHistoryEntry>,
    /// For each entry in `persistent_lets`, the resource names of the
    /// active provider stack at the moment the binding was captured
    /// (outermost first). Empty `Vec` for bindings captured outside
    /// any `:provide` scope. Length is invariant-equal to
    /// `persistent_lets.len()`. Consumed by `end_provider` to detect
    /// which replayed `let`s were declared inside the now-closing
    /// scope so they prune in lockstep with the meta-command;
    /// matches design.md § Cross-Cell Providers's "bindings declared
    /// inside `:provide R` are visible only within that scope" rule.
    persistent_let_provider_scope: Vec<Vec<String>>,
    /// Bindings that were dropped from `persistent_lets` when their
    /// originating `:provide` scope closed. The REPL's
    /// `render_resolve_errors_repl` layer consults this table on
    /// "undefined name 'X'" resolver errors and, if the missing name
    /// matches a pruned entry, appends a notebook-aware tail naming
    /// the closed provider scope and the cell where the binding was
    /// declared (mirrors the use-after-move enrichment shape from
    /// `render_ownership_errors_repl`). Entries are removed when the
    /// user re-binds the same name in a later cell (capture clears
    /// stale pruning data); cleared entirely by `reset_persistent_lets`.
    pruned_provider_lets: Vec<PrunedProviderLet>,
    /// Per-cell structured effect snapshot, aligned 1:1 with
    /// `cell_history`. Entry `i` is the structured form of cell
    /// `i+1`'s inferred effects on `main` — the same data
    /// `compute_cell_effect_footer` renders into the human-readable
    /// `writes(A) reads(B)` string, exposed in a sorted, machine-
    /// readable shape so the line 773 effect-conflict timeline can
    /// compute cross-cell dependencies without re-running the
    /// effect checker. Pure-item cells contribute an empty
    /// snapshot (they don't run main). Rolled back in lockstep with
    /// `cell_history` on diagnostic-side failure.
    cell_effect_history: Vec<CellEffectSnapshot>,

    /// Slice c-repl.B.B: persistent `karac_jit_runner --repl-mode`
    /// subprocess client, lazily spawned on the first cell when JIT
    /// mode is on (`KARAC_REPL_JIT=1` at session construction).
    /// Re-spawned after a cell-induced `exit(1)` terminates the
    /// runner. `None` either when JIT mode is off or after a death
    /// + before the next cell.
    #[cfg(feature = "lljit_prototype")]
    jit_client: Option<jit_runner_client::ReplRunnerClient>,
    /// Slice c-repl.B.B: snapshot of `KARAC_REPL_JIT=1` at session
    /// construction. Env var is read once so a mid-session flip can't
    /// half-route cells.
    #[cfg(feature = "lljit_prototype")]
    jit_enabled: bool,
    /// Slice c-repl.B.B: monotonic cell id for the JIT subprocess
    /// protocol framing. The runner echoes the id back on `result`
    /// lines so framing-integrity drift surfaces as a `RunnerDied`
    /// classification at the client. Each cell's synthesized
    /// `fn main()` is registered in LLVM under `cell_main_<id>`
    /// (slice c-repl.B.4) so multiple cells coexist in the runner's
    /// JITDylib without symbol collisions.
    #[cfg(feature = "lljit_prototype")]
    jit_next_cell_id: u64,
    /// Slice c-repl.B.4: names of free functions already installed
    /// in the JIT runner's JITDylib by a prior successful cell. The
    /// next cell's codegen emits these as `declare`-only (no body)
    /// so the JIT linker resolves calls to them against the prior
    /// definition — cutting per-cell codegen + jitlink cost. Cleared
    /// when the runner subprocess dies (a fresh runner has an empty
    /// JITDylib) and on `:reset` (which drops the runner so the
    /// snapshot globals don't outlive the persistent-let slate that
    /// gave them meaning).
    #[cfg(feature = "lljit_prototype")]
    jit_installed_fns: std::collections::HashSet<String>,
    /// Slice c-repl.B.5.1: top-level `let <name> = <expr>` bindings
    /// whose bound value has been stashed into a cell-spanning LLVM
    /// global by a prior successful cell. Each entry maps the binding
    /// name to the primitive kind the snapshot global was emitted at.
    /// The next cell that sees a re-emitted `let <name> = …` (via
    /// `persistent_lets` replay in the synthetic source) skips the
    /// original RHS and binds to a load from the global instead —
    /// closing the interpreter-vs-JIT semantic gap where a
    /// side-effecting RHS would otherwise re-execute on every cell.
    /// Cleared on runner death (the fresh runner's JITDylib has no
    /// globals) and on `:reset`. Only primitive types (i64, f64,
    /// bool, char) qualify in B.5.1; richer types deferred.
    #[cfg(feature = "lljit_prototype")]
    jit_snapshotted_lets: std::collections::HashMap<String, crate::codegen::SnapshotPrimKind>,
}

/// Per-cell structured effect snapshot used by the line 773 cross-cell
/// effect-conflict timeline. Captures the effect set inferred on each
/// cell's synthesized `main`, in deterministic order (verb emit order,
/// then resource lex order). Empty for pure-item cells and for
/// statement cells with no tracked effects.
#[derive(Debug, Clone, Default)]
pub struct CellEffectSnapshot {
    /// Sorted `(verb, resource)` pairs. Execution verbs (`blocks`,
    /// `suspends`) carry an empty `resource` string; resource verbs
    /// (`reads`, `writes`, `sends`, `receives`, `allocates`,
    /// `panics`) carry the resource name. Within a verb, resources
    /// are lex-sorted; across verbs, emit order matches
    /// `render_effect_decls`.
    pub effects: Vec<(crate::ast::EffectVerbKind, String)>,
}

/// A cross-cell effect dependency surfaced by the `%timeline` magic.
/// Carries the producer cell, the consumer cell, the resource, and
/// the dependency flavor. v1 detects read-after-write and write-
/// after-write — both share the producer-side condition `writes(R)
/// on cell M < N`. Other verbs (sends, receives, allocates, panics,
/// blocks, suspends) appear in the per-cell footer but don't form
/// arrows in v1 (carved as v1.1.x follow-ups).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellDependency {
    /// 1-based index of the producer cell — the prior `writes(R)`.
    pub from_cell: usize,
    /// 1-based index of the consumer cell — the `reads(R)` or
    /// later `writes(R)` that depends on the producer.
    pub to_cell: usize,
    /// Resource name shared by the producer and consumer.
    pub resource: String,
    /// Which kind of cross-cell dependency this entry records.
    pub kind: DependencyKind,
}

/// Flavor of cross-cell effect dependency. Drives the textual tail
/// the timeline renderer attaches to a cell's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyKind {
    /// Consumer reads a resource an earlier cell wrote — the classic
    /// data dependency users mean by "cell N uses what cell M
    /// produced". Spec example: "cell 3 reads `Files` written by
    /// cell 2".
    ReadAfterWrite,
    /// Consumer writes a resource an earlier cell also wrote —
    /// ordering matters even though both cells "produce". Surfaced
    /// because it's the most common reproducibility footgun in
    /// notebook workflows (re-run cell N out of order, the state
    /// drifts from what the linear read-through reproduces).
    WriteAfterWrite,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Session {
            items_source: String::new(),
            cell_history: Vec::new(),
            persistent_lets: Vec::new(),
            persistent_let_origin: Vec::new(),
            cell_byte_ranges: Vec::new(),
            auto_clone: false,
            let_snapshots: HashMap::new(),
            pending_deps: BTreeMap::new(),
            provider_stack: Vec::new(),
            provider_history: Vec::new(),
            persistent_let_provider_scope: Vec::new(),
            pruned_provider_lets: Vec::new(),
            cell_effect_history: Vec::new(),
            #[cfg(feature = "lljit_prototype")]
            jit_client: None,
            #[cfg(feature = "lljit_prototype")]
            jit_enabled: std::env::var("KARAC_REPL_JIT").as_deref() == Ok("1"),
            #[cfg(feature = "lljit_prototype")]
            jit_next_cell_id: 0,
            #[cfg(feature = "lljit_prototype")]
            jit_installed_fns: std::collections::HashSet::new(),
            #[cfg(feature = "lljit_prototype")]
            jit_snapshotted_lets: std::collections::HashMap::new(),
        }
    }

    /// Inspector for the value-snapshot store. Returns the map of
    /// persistent-let binding names to their cached `Value`s. Used by
    /// integration tests that verify side-effecting RHS expressions
    /// only run once across cells.
    pub fn let_snapshots(&self) -> &HashMap<String, Value> {
        &self.let_snapshots
    }

    /// Dispatch a Jupyter-style `%magic` line. The kernel binary
    /// (phase-5 tracker § "Jupyter kernel MVP", `[x]` shipped
    /// 2026-05-19) routes every cell whose first token starts with
    /// `%` through this entry point; the returned `MagicOutput`
    /// carries the rendered text, the channel the kernel should
    /// display on (`stdout` vs `display_data` for rich shapes), and
    /// an `ok` flag the kernel maps to its protocol error semantics.
    /// The same dispatcher is exercised by the test suite so the
    /// surface is observable without a kernel binary.
    ///
    /// Supported magics (line 689 spec):
    ///
    /// - `%effects`               — session-wide effect set on items_source
    /// - `%ownership`             — current binding table with mode / RC status
    /// - `%explain <name>`        — wrap `karac explain` (concept or class)
    /// - `%set auto-clone on|off` — toggle the opt-in ownership mode
    /// - `%provide R = expr`      — open a cross-cell `with_provider` scope
    /// - `%end-provide R`         — close the matching provider scope
    /// - `%show <expr>`           — rich-display an expression
    ///   (text/plain always, text/html for `Vec[Struct]`)
    /// - `%timeline`              — per-cell effect set + cross-cell
    ///   dependency arrows (text/plain + text/html)
    ///
    /// `%provide` / `%end-provide` forward to the same `add_provider` /
    /// `end_provider` handlers the REPL meta-commands use — `:provide`
    /// and `%provide` are wire-compatible, returning the same `Ok` /
    /// `Err` string shapes. `%show` is the rich-display surface
    /// (line 761 in the phase-5 tracker); it evaluates the given
    /// expression against the current session state and renders it via
    /// [`render_display`] into a [`DisplayBundle`] that the Jupyter
    /// kernel broadcasts as `display_data`. `%rc` (line 785) lists
    /// every RC-fallback decision the ownership pass made for the
    /// current session source with the trigger label, Rc/Arc bit,
    /// consume + reuse spans, and (when available) type name.
    pub fn dispatch_magic(&mut self, line: &str) -> MagicOutput {
        let trimmed = line.trim();
        let body = trimmed.strip_prefix('%').unwrap_or(trimmed).trim();
        let (cmd, rest) = match body.split_once(char::is_whitespace) {
            Some((c, r)) => (c.trim(), r.trim()),
            None => (body, ""),
        };
        match cmd {
            "effects" => self.magic_effects(),
            "ownership" => self.magic_ownership(),
            "explain" => self.magic_explain(rest),
            "set" => self.magic_set(rest),
            "show" => self.magic_show(rest),
            "timeline" => self.magic_timeline(rest),
            "rc" => self.magic_rc(rest),
            "provide" => match self.add_provider(rest) {
                Ok(msg) => MagicOutput::ok(msg),
                Err(msg) => MagicOutput::error(msg),
            },
            "end-provide" => match self.end_provider(rest) {
                Ok(msg) => MagicOutput::ok(msg),
                Err(msg) => MagicOutput::error(msg),
            },
            "" => MagicOutput::error(
                "empty magic command. Supported: %effects, %ownership, %explain <name>, %set auto-clone on|off, %show <expr>, %timeline, %rc, %provide R = expr, %end-provide R",
            ),
            other => MagicOutput::error(format!(
                "unknown magic `%{other}`. Supported: %effects, %ownership, %explain <name>, %set auto-clone on|off, %show <expr>, %timeline, %rc, %provide R = expr, %end-provide R"
            )),
        }
    }

    fn magic_effects(&self) -> MagicOutput {
        // Reuse `show_effects`'s logic but accumulate into a string
        // instead of printing. The kernel will route this through
        // `display_data` so a long list lands cleanly in the cell
        // output area.
        if self.items_source.trim().is_empty() {
            return MagicOutput::ok("(no items defined yet)");
        }
        let parsed = crate::parse(&self.items_source);
        if !parsed.errors.is_empty() {
            return MagicOutput::error("(items_source has parse errors — fix them first)");
        }
        let effects = crate::effectcheck(&parsed.program);
        let mut entries: Vec<(&String, &crate::effectchecker::EffectSet)> =
            effects.inferred_effects.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = String::new();
        let mut printed = 0usize;
        for (name, eff) in entries {
            if eff.effects.is_empty() {
                continue;
            }
            let rendered = render_effect_decls(eff);
            if rendered.is_empty() {
                continue;
            }
            out.push_str(&format!("{name}: {rendered}\n"));
            printed += 1;
        }
        if printed == 0 {
            out.push_str("(no effects inferred so far)\n");
        }
        MagicOutput::ok(out.trim_end())
    }

    fn magic_ownership(&self) -> MagicOutput {
        // Compute the ownership surface for the current session view:
        // items_source wrapped in a synthetic `fn main()` so the
        // ownership pass classifies any binding the user has
        // accumulated. The pass's `representations` map records
        // parameters and RC-promoted bindings; owned-stack locals
        // (the most common case in a `let x = 5;` REPL session) do
        // not appear there, so we layer a baseline walk over the
        // session's `persistent_lets` to keep the table populated
        // for plain `let` bindings. Each row is `<binding>: <mode>`
        // sorted lexicographically; RC promotions from the
        // ownership pass win over the baseline owned-stack label.
        let mut synth = String::new();
        if !self.items_source.trim().is_empty() {
            synth.push_str(&strip_main(&self.items_source));
            if !synth.is_empty() && !synth.ends_with('\n') {
                synth.push('\n');
            }
        }
        synth.push_str("fn main() {\n");
        for prior_let in &self.persistent_lets {
            synth.push_str(prior_let);
            if !prior_let.ends_with('\n') {
                synth.push('\n');
            }
        }
        synth.push_str("}\n");
        let parsed = crate::parse(&synth);
        if !parsed.errors.is_empty() {
            return MagicOutput::error(
                "(session source has parse errors — fix them before inspecting ownership)",
            );
        }
        let resolved = crate::resolve(&parsed.program);
        if !resolved.errors.is_empty() {
            return MagicOutput::error(
                "(session source has resolve errors — fix them before inspecting ownership)",
            );
        }
        let typed = crate::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            return MagicOutput::error(
                "(session source has type errors — fix them before inspecting ownership)",
            );
        }
        let owned = crate::ownershipcheck(&parsed.program, &typed);

        // Baseline: every persistent-let binding is owned-stack until
        // the ownership pass overrides it (e.g. with an RC promotion).
        let mut rows: BTreeMap<String, String> = BTreeMap::new();
        for prior_let in &self.persistent_lets {
            for name in parse_let_binding_names(prior_let) {
                rows.insert(name, "owned (stack)".to_string());
            }
        }
        for (key, mode) in &owned.representations {
            if let Some(stripped) = key.strip_prefix("main.") {
                rows.insert(stripped.to_string(), mode.clone());
            }
        }
        if rows.is_empty() {
            return MagicOutput::ok("(no bindings to inspect)");
        }
        let mut out = String::new();
        for (binding, mode) in rows {
            out.push_str(&format!("{binding}: {mode}\n"));
        }
        MagicOutput::ok(out.trim_end())
    }

    fn magic_explain(&self, rest: &str) -> MagicOutput {
        if rest.is_empty() {
            return MagicOutput::error("usage: %explain <concept-or-class-name>");
        }
        // Try concept lookup first, then class. Matches the CLI
        // surface's lookup order (concept names + class wire forms
        // share the same identifier namespace today). The CLI flag
        // `--format=json` is not exposed via the magic surface; the
        // kernel renders the text form into the cell output.
        let concept_target = crate::cli::ExplainTarget::Concept(rest.to_string());
        if let Ok(rendered) =
            crate::cli::explain::render_to_string(&concept_target, crate::cli::ExplainFormat::Text)
        {
            return MagicOutput::ok(rendered.trim_end());
        }
        let class_target = crate::cli::ExplainTarget::Class(rest.to_string());
        match crate::cli::explain::render_to_string(&class_target, crate::cli::ExplainFormat::Text)
        {
            Ok(rendered) => MagicOutput::ok(rendered.trim_end()),
            Err(msg) => MagicOutput::error(msg),
        }
    }

    fn magic_set(&mut self, rest: &str) -> MagicOutput {
        // Spec form: `%set auto-clone on|off`. The flag is the only
        // setting in the v1 surface; future settings extend the same
        // `<key> <value>` shape.
        let (key, value) = match rest.split_once(char::is_whitespace) {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (rest.trim(), ""),
        };
        match key {
            "auto-clone" => match value {
                "on" | "true" | "1" => {
                    self.auto_clone = true;
                    MagicOutput::ok("auto-clone: on")
                }
                "off" | "false" | "0" => {
                    self.auto_clone = false;
                    MagicOutput::ok("auto-clone: off")
                }
                "" => MagicOutput::error("usage: %set auto-clone on|off"),
                other => MagicOutput::error(format!(
                    "unknown value `{other}` for auto-clone. Use `on` or `off`."
                )),
            },
            "" => MagicOutput::error("usage: %set <key> <value>"),
            other => {
                MagicOutput::error(format!("unknown setting `{other}`. Supported: auto-clone."))
            }
        }
    }

    /// `%show <expr>` — rich-display the given expression. Evaluates
    /// the expression against the current session state (items +
    /// persistent lets, value-snapshot semantics for any binding the
    /// user has previously assigned), captures the resulting [`Value`],
    /// and renders it via [`render_display`] into a [`DisplayBundle`].
    /// The bundle always carries `text/plain` (pretty-printed for
    /// nested data); `text/html` is additionally present when the
    /// value is `Vec[Struct]` / `Slice[Struct]`. The kernel broadcasts
    /// the bundle through `display_data` (slice 3 of line 761).
    ///
    /// Side-effect-free: `%show` does NOT mutate `cell_history`,
    /// `persistent_lets`, or `let_snapshots`. The synthetic main is
    /// built locally for this call only. RHS expressions that *would*
    /// be effectful at the language level still run (the interpreter
    /// has no sandbox), but the binding `__k_show` introduced here
    /// never leaks out.
    fn magic_show(&self, rest: &str) -> MagicOutput {
        if rest.is_empty() {
            return MagicOutput::error("usage: %show <expression>");
        }
        match self.eval_expr_for_display(rest) {
            Ok(value) => {
                let bundle = render_display(&value);
                let text = bundle.get("text/plain").unwrap_or("").to_string();
                MagicOutput::ok_rich(text, bundle)
            }
            Err(msg) => MagicOutput::error(msg),
        }
    }

    /// Build a one-off synthetic main that binds `__k_show = <expr>;`,
    /// run the pipeline, and return the captured runtime [`Value`].
    /// Persistent-let snapshots replay through `let_value_overrides`
    /// so side-effecting RHS expressions don't re-fire just because
    /// the user asked to inspect another expression. None of the
    /// session's mutable state is touched on success or failure.
    fn eval_expr_for_display(&self, expr: &str) -> Result<Value, String> {
        let expr_trimmed = expr.trim();
        if expr_trimmed.is_empty() {
            return Err("empty expression".to_string());
        }
        let mut src = String::new();
        src.push_str(&strip_main(&self.items_source));
        if !src.is_empty() && !src.ends_with('\n') {
            src.push('\n');
        }
        src.push_str("fn main() {\n");
        for prior_let in &self.persistent_lets {
            src.push_str(prior_let);
            if !prior_let.ends_with('\n') {
                src.push('\n');
            }
        }
        src.push_str("let __k_show = ");
        src.push_str(expr_trimmed);
        src.push_str(";\n}\n");

        let mut parsed = crate::parse(&src);
        if !parsed.errors.is_empty() {
            return Err(format!("parse error: {}", parsed.errors[0].message));
        }
        let resolved = crate::resolve(&parsed.program);
        if !resolved.errors.is_empty() {
            return Err(format!("resolve error: {}", resolved.errors[0].message));
        }
        let typed = crate::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            return Err(format!("type error: {}", typed.errors[0].message));
        }
        let owned = crate::ownershipcheck(&parsed.program, &typed);
        if !owned.errors.is_empty() {
            return Err(format!("ownership error: {}", owned.errors[0].message));
        }
        crate::lower(&mut parsed.program, &typed);

        let mut interp = crate::interpreter::Interpreter::new(&parsed.program, &typed);
        // Replay value snapshots for every persistent-let binding so
        // side-effecting RHS expressions short-circuit, exactly as the
        // statement-cell pipeline does. The synthetic main here was
        // built from the same `persistent_lets` strings, so the parser
        // sees the bindings and the interpreter consults the override
        // map at the matching `let` arm.
        for prior_let in &self.persistent_lets {
            for name in parse_let_binding_names(prior_let) {
                if let Some(v) = self.let_snapshots.get(&name) {
                    interp.let_value_overrides.insert(name, v.clone());
                }
            }
        }
        // Watch our synthetic `__k_show` binding so the interpreter
        // captures its post-eval Value into `captured_let_values`. We
        // do NOT watch the persistent-let names — we don't intend to
        // write back into `self.let_snapshots` (the session is
        // immutable through this path).
        let mut watch = std::collections::HashSet::new();
        watch.insert("__k_show".to_string());
        interp.let_snapshot_watch = watch;
        interp.run();

        match interp.captured_let_values.remove("__k_show") {
            Some(v) => Ok(v),
            None => {
                Err("expression did not yield a value (did it panic or never bind?)".to_string())
            }
        }
    }

    /// `%timeline` — render the per-cell effect set with cross-cell
    /// dependency arrows. Walks `cell_effect_history` in submission
    /// order; for each cell records the human-readable effect line
    /// (matching the per-cell footer format) and any cross-cell
    /// dependency tail: read-after-write ("← reads X written by cell N")
    /// and write-after-write ("← writes X already written by cell N").
    /// Emits two mime bundles via [`DisplayBundle`]: `text/plain` for
    /// the universal cell-output pane, `text/html` for kernel
    /// frontends that render rich content. Read-only against session
    /// state. Empty session emits a friendly "(no cells yet)" hint on
    /// `text/plain` only — there's no table to render until at least
    /// one cell lands.
    fn magic_timeline(&self, rest: &str) -> MagicOutput {
        if !rest.trim().is_empty() {
            return MagicOutput::error(
                "usage: %timeline (no arguments — renders the full session timeline)",
            );
        }
        if self.cell_effect_history.is_empty() {
            return MagicOutput::ok("(no cells yet)");
        }
        let deps = self.compute_cell_dependencies();
        let plain = render_timeline_text(&self.cell_effect_history, &deps);
        let html = render_timeline_html(&self.cell_effect_history, &deps);
        let mut bundle = DisplayBundle::new();
        bundle = bundle.with("text/plain", plain.clone());
        bundle = bundle.with("text/html", html);
        MagicOutput::ok_rich(plain, bundle)
    }

    /// Line 785 `%rc` magic. Lists every RC-fallback decision the
    /// ownership pass recorded for the current session source — both
    /// inside the synthesized `main` (from accumulated `let` bindings)
    /// and inside any user-declared function in `items_source`. Each
    /// row carries the qualified binding name (`<fn>.<binding>`), the
    /// trigger label (one of the three `RcTrigger` variants — direct
    /// re-use, closure capture, container store), the sharing kind
    /// (`Rc` or `Arc` — the latter set for bindings the Phase-2
    /// promotion pass lifted because they cross a parallel region),
    /// the consume + reuse line:column, and (when the ownership pass
    /// captured it) the type name. Output is two mimes — `text/plain`
    /// for the universal cell-output pane and `text/html` for kernel
    /// frontends that render rich content. Empty surface (no RC
    /// fallbacks) emits a friendly hint on `text/plain` only.
    /// Read-only against session state — `cell_history`,
    /// `persistent_lets`, and `let_snapshots` are untouched. Surfaces
    /// a friendly hint on parse / resolve / typecheck failure rather
    /// than running the ownership pass against a known-broken AST.
    fn magic_rc(&self, rest: &str) -> MagicOutput {
        if !rest.trim().is_empty() {
            return MagicOutput::error(
                "usage: %rc (no arguments — lists every RC-fallback decision in the current session)",
            );
        }
        let mut synth = String::new();
        if !self.items_source.trim().is_empty() {
            synth.push_str(&strip_main(&self.items_source));
            if !synth.is_empty() && !synth.ends_with('\n') {
                synth.push('\n');
            }
        }
        synth.push_str("fn main() {\n");
        for prior_let in &self.persistent_lets {
            synth.push_str(prior_let);
            if !prior_let.ends_with('\n') {
                synth.push('\n');
            }
        }
        synth.push_str("}\n");

        let parsed = crate::parse(&synth);
        if !parsed.errors.is_empty() {
            return MagicOutput::error(
                "(session source has parse errors — fix them before inspecting RC fallbacks)",
            );
        }
        let resolved = crate::resolve(&parsed.program);
        if !resolved.errors.is_empty() {
            return MagicOutput::error(
                "(session source has resolve errors — fix them before inspecting RC fallbacks)",
            );
        }
        let typed = crate::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            return MagicOutput::error(
                "(session source has type errors — fix them before inspecting RC fallbacks)",
            );
        }
        let owned = crate::ownershipcheck(&parsed.program, &typed);

        // Collect rows across every function, sorted by (fn_name,
        // binding) for stable output. RcEntry's spans are AST
        // line/column values; Arc promotion is keyed by binding name
        // inside the same function map. `__cell` / wrapper functions
        // we didn't synthesize ourselves don't exist on this path —
        // every entry maps to either `main` or a user-declared fn in
        // `items_source`.
        let mut fn_keys: Vec<&String> = owned.rc_values.keys().collect();
        fn_keys.sort();
        let mut rows: Vec<RcRow> = Vec::new();
        for fn_name in fn_keys {
            let entries = owned.rc_values.get(fn_name).expect("fn key present");
            let arc_set = owned.arc_values.get(fn_name);
            let mut binding_keys: Vec<&String> = entries.keys().collect();
            binding_keys.sort();
            for binding in binding_keys {
                let entry = entries.get(binding).expect("binding key present");
                let is_arc = arc_set.map(|s| s.contains(binding)).unwrap_or(false);
                rows.push(RcRow {
                    fn_name: fn_name.clone(),
                    binding: binding.clone(),
                    trigger_label: entry.trigger.label(),
                    kind: if is_arc { RcKind::Arc } else { RcKind::Rc },
                    consume_line: entry.consume_span.line,
                    consume_column: entry.consume_span.column,
                    other_use_line: entry.other_use_span.line,
                    other_use_column: entry.other_use_span.column,
                    type_name: entry.type_name.clone(),
                });
            }
        }

        if rows.is_empty() {
            return MagicOutput::ok("(no RC fallbacks in current session)");
        }
        let plain = render_rc_text(&rows);
        let html = render_rc_html(&rows);
        let mut bundle = DisplayBundle::new();
        bundle = bundle.with("text/plain", plain.clone());
        bundle = bundle.with("text/html", html);
        MagicOutput::ok_rich(plain, bundle)
    }

    /// Compute cross-cell effect dependencies from `cell_effect_history`.
    /// Detects two dependency kinds:
    ///
    /// - **Read-after-write** — cell N's `reads(R)` depends on the most
    ///   recent earlier cell M < N that wrote R. The timeline arrow
    ///   reads "cell N reads R written by cell M".
    /// - **Write-after-write** — cell N's `writes(R)` depends on the
    ///   most recent earlier cell M < N that wrote R. The arrow reads
    ///   "cell N writes R already written by cell M".
    ///
    /// Only the most recent producer is recorded (transitive chains
    /// stay readable). Other verbs (`sends`, `receives`, `allocates`,
    /// `panics`, `blocks`, `suspends`) are surfaced in the per-cell
    /// effect line but don't form cross-cell dependency arrows in v1;
    /// the v1.1.x follow-ups widen this set (sends→receives channel
    /// pairing in particular).
    pub fn compute_cell_dependencies(&self) -> Vec<CellDependency> {
        use crate::ast::EffectVerbKind;
        let mut deps: Vec<CellDependency> = Vec::new();
        for (i, snapshot) in self.cell_effect_history.iter().enumerate() {
            let to_cell = i + 1;
            for (verb, resource) in &snapshot.effects {
                let kind = match verb {
                    EffectVerbKind::Reads => DependencyKind::ReadAfterWrite,
                    EffectVerbKind::Writes => DependencyKind::WriteAfterWrite,
                    _ => continue,
                };
                // Look backwards for the most recent earlier cell that
                // wrote the same resource. Read-after-write and
                // write-after-write share the same producer-side
                // condition: `writes(resource)` on some cell M < N.
                for j in (0..i).rev() {
                    let producer = &self.cell_effect_history[j];
                    let produces = producer
                        .effects
                        .iter()
                        .any(|(v, r)| matches!(v, EffectVerbKind::Writes) && r == resource);
                    if produces {
                        deps.push(CellDependency {
                            from_cell: j + 1,
                            to_cell,
                            resource: resource.clone(),
                            kind: kind.clone(),
                        });
                        break;
                    }
                }
            }
        }
        deps
    }

    /// Compute the per-cell effect summary for a statement-style cell:
    /// the structured snapshot used by the line 773 timeline AND the
    /// human-readable footer string the kernel renders after every
    /// cell. Both flow from one effect-checker pass over the synthetic
    /// source (items + replayed lets + the just-evaluated cell body).
    /// Returns `(empty_snapshot, "")` on parse failure or when `main`
    /// has no tracked effects — the kernel suppresses the footer in
    /// that case so pure cells stay visually quiet, and the timeline
    /// records the empty snapshot so cell indices stay aligned.
    fn compute_cell_effect_summary(&self, cell_src: &str) -> (CellEffectSnapshot, String) {
        let mut synth = String::new();
        synth.push_str(&strip_main(&self.items_source));
        if !synth.is_empty() && !synth.ends_with('\n') {
            synth.push('\n');
        }
        synth.push_str("fn main() {\n");
        for prior_let in &self.persistent_lets {
            synth.push_str(prior_let);
            if !prior_let.ends_with('\n') {
                synth.push('\n');
            }
        }
        synth.push_str(cell_src);
        if !cell_src.ends_with('\n') {
            synth.push('\n');
        }
        synth.push_str("}\n");
        let parsed = crate::parse(&synth);
        if !parsed.errors.is_empty() {
            return (CellEffectSnapshot::default(), String::new());
        }
        let effects = crate::effectcheck(&parsed.program);
        let Some(set) = effects.inferred_effects.get("main") else {
            return (CellEffectSnapshot::default(), String::new());
        };
        let snapshot = effect_set_to_snapshot(set);
        let footer = render_effect_decls(set);
        (snapshot, footer)
    }

    /// Construct a session pre-configured from caller-supplied
    /// `ReplOptions` (the CLI flag bag). Used by `run_with_options` and
    /// integration tests that exercise `--auto-clone` without driving
    /// rustyline through a TTY.
    pub fn with_options(opts: ReplOptions) -> Self {
        let mut s = Self::new();
        s.auto_clone = opts.auto_clone;
        s
    }

    /// Inspector for the `--auto-clone` flag. Used by integration tests.
    pub fn auto_clone(&self) -> bool {
        self.auto_clone
    }

    /// Slice c-repl.B.B test hook: flip the JIT-dispatch mode without
    /// relying on the `KARAC_REPL_JIT=1` env var read at construction.
    /// `Session::new` reads the env once; tests want explicit control
    /// without poking shared process state (which Rust 2024 made
    /// `unsafe`). Production callers should set the env var instead.
    #[cfg(feature = "lljit_prototype")]
    pub fn set_jit_enabled_for_tests(&mut self, enabled: bool) {
        self.jit_enabled = enabled;
    }

    /// Slice c-repl.B.B inspector: reports whether JIT-dispatch mode
    /// is active for this session. Used by integration tests to
    /// confirm the env-var read and by future status meta-commands.
    #[cfg(feature = "lljit_prototype")]
    pub fn jit_enabled(&self) -> bool {
        self.jit_enabled
    }

    /// Inspector for the accumulated `let`-statement replay buffer. Used by
    /// integration tests and by a future `:show lets` meta-command.
    pub fn persistent_lets(&self) -> &[String] {
        &self.persistent_lets
    }

    /// Inspector for the in-memory `:dep` registry. Returns `name → spec`
    /// where `spec` is the normalized TOML value (a quoted semver string
    /// or an inline-table source). Used by integration tests; consumed
    /// by the package manager once it lands.
    pub fn pending_deps(&self) -> &BTreeMap<String, String> {
        &self.pending_deps
    }

    /// Inspector for the active cross-cell provider stack. Returns the
    /// frames in nesting order (outermost first). Used by integration
    /// tests to assert push/pop semantics without driving the REPL
    /// through a TTY; the wrapping mechanism in slice 3 and the
    /// notebook-aware diagnostic in slice 4 also consume this.
    pub fn provider_stack(&self) -> &[ProviderFrame] {
        &self.provider_stack
    }

    /// Inspector for accumulated items source. Used by integration tests
    /// and could surface inside a `:show items` meta-command later.
    pub fn items_source(&self) -> &str {
        &self.items_source
    }

    /// Inspector for verbatim cell history. Used by `:save` and integration
    /// tests; could back a future `:history` meta-command.
    pub fn cell_history(&self) -> &[String] {
        &self.cell_history
    }

    /// Inspector for the per-cell structured effect snapshot,
    /// aligned 1:1 with `cell_history`. Entry `i` is cell `i+1`'s
    /// inferred effects on `main`, in deterministic order. Drives
    /// the line 773 effect-conflict timeline (`%timeline` magic +
    /// cross-cell dependency rendering); also used by integration
    /// tests that pin the per-cell snapshot shape.
    pub fn cell_effect_history(&self) -> &[CellEffectSnapshot] {
        &self.cell_effect_history
    }

    /// Inspector for the current cell-byte-range map (the synthetic-source
    /// → cell mapping built by the most recent compilation). Used by
    /// integration tests and consumed in the next slice (`--auto-clone`
    /// opt-in mode) to drive consume-site rewrites.
    pub fn cell_byte_ranges(&self) -> &[(usize, Range<usize>)] {
        &self.cell_byte_ranges
    }

    /// Map a span (recorded against the most recently built synthetic-
    /// program source) to the 1-based cell index that produced the
    /// corresponding source bytes. Returns `None` for spans that fall
    /// outside any tracked range — synthetic wrapper bytes (`fn main() {`,
    /// closing `}`), `items_source` content, or stale spans whose offsets
    /// no longer line up after a re-compile. The `cell_byte_ranges` table
    /// is non-overlapping and source-ordered, so the linear scan here is
    /// O(n) in the number of cells contributing to the current synthetic
    /// source — fine for v1 cell counts.
    pub fn cell_for_span(&self, span: &crate::token::Span) -> Option<usize> {
        let start = span.offset;
        for (cell_idx, range) in &self.cell_byte_ranges {
            if range.contains(&start) {
                return Some(*cell_idx);
            }
        }
        None
    }

    /// Handle a `:meta` command. Returns `false` when the user requested
    /// quit, `true` to continue the loop.
    pub fn dispatch_meta(&mut self, line: &str) -> bool {
        // Split on first whitespace.
        let (cmd, rest) = match line.split_once(char::is_whitespace) {
            Some((c, r)) => (c.trim(), r.trim()),
            None => (line.trim(), ""),
        };
        match cmd {
            ":help" => {
                print_help();
            }
            ":quit" | ":q" | ":exit" => {
                return false;
            }
            ":type" => {
                if rest.is_empty() {
                    eprintln!("usage: :type <expr>");
                } else {
                    self.show_type(rest);
                }
            }
            ":effects" => {
                self.show_effects();
            }
            ":save" => {
                if rest.is_empty() {
                    eprintln!("usage: :save <path.kara>");
                } else {
                    self.save(rest);
                }
            }
            ":reset" => {
                let cleared = self.persistent_lets.len();
                self.reset_persistent_lets();
                println!("(cleared {cleared} persistent let binding(s); items + history kept)");
            }
            ":dep" => {
                if rest.is_empty() {
                    eprintln!(
                        "usage: :dep <name> = \"<version>\"  or  :dep <name> = {{ git = \"...\" }}"
                    );
                } else {
                    self.add_dep(rest);
                }
            }
            ":provide" => {
                if rest.is_empty() {
                    eprintln!("usage: :provide <Resource> = <expr>");
                } else {
                    match self.add_provider(rest) {
                        Ok(msg) => println!("{msg}"),
                        Err(msg) => eprintln!("{msg}"),
                    }
                }
            }
            ":end-provide" => {
                if rest.is_empty() {
                    eprintln!("usage: :end-provide <Resource>");
                } else {
                    match self.end_provider(rest) {
                        Ok(msg) => println!("{msg}"),
                        Err(msg) => eprintln!("{msg}"),
                    }
                }
            }
            other => {
                eprintln!("unknown meta-command '{other}'. Try :help for the supported list.");
            }
        }
        true
    }

    /// Evaluate one cell. Classification is best-effort: the cell is
    /// classified as "pure items" iff a standalone parse of the input
    /// produces at least one top-level item AND no top-level statements
    /// are present. Otherwise the cell is treated as statements and run
    /// inside the synthetic `fn main()` wrapper.
    pub fn evaluate_cell(&mut self, src: &str) {
        let trimmed = src.trim_end();
        self.cell_history.push(trimmed.to_string());

        if classify_input(trimmed) == CellShape::PureItems {
            // Pure-item cell: append to the accumulated source and
            // re-validate the whole thing so type-checker errors surface
            // immediately rather than waiting for the next statement cell.
            self.append_items(trimmed);
            self.validate_accumulated();
            // Pure-item cells contribute no main-effects; keep
            // `cell_effect_history` aligned 1:1 with `cell_history` so
            // `%timeline` indexes the same cells the rest of the
            // session sees.
            self.cell_effect_history.push(CellEffectSnapshot::default());
            return;
        }

        // Statement / expression / mixed cell — wrap in a synthetic main.
        self.run_with_wrapper(trimmed);
    }

    fn append_items(&mut self, src: &str) {
        // Per design.md § Cell Scope, a later cell's `fn f` / `struct Point`
        // / `const X` shadows the earlier one. The accumulated buffer holds
        // both, so the resolver would surface a duplicate-name error if we
        // just appended. Strip the prior definition(s) of any name the new
        // cell re-declares before appending — anonymous items (`impl`
        // blocks, `use` / `import`, `independent`) are never pruned because
        // they have no shadowable name.
        self.prune_shadowed_items(src);
        if !self.items_source.is_empty() && !self.items_source.ends_with('\n') {
            self.items_source.push('\n');
        }
        self.items_source.push_str(src);
        if !self.items_source.ends_with('\n') {
            self.items_source.push('\n');
        }
    }

    /// Remove every prior top-level definition from `items_source` whose
    /// name appears in `new_item_src`. Re-parses the buffer, identifies
    /// each surviving item's source range (extended to the end of the
    /// previous item so doc comments and inter-item whitespace ride along
    /// with the *next* item), and concatenates the kept ranges back into
    /// the buffer. Trailing content after the last item rides with the
    /// last kept item so it doesn't get stranded.
    fn prune_shadowed_items(&mut self, new_item_src: &str) {
        let new_names = collect_top_level_item_names(new_item_src);
        if new_names.is_empty() || self.items_source.trim().is_empty() {
            return;
        }
        let parsed = crate::parse(&self.items_source);
        if !parsed.errors.is_empty() {
            // The buffer is already broken — leave it alone so the user
            // sees the original parse error rather than a corrupted view
            // produced by our prune.
            return;
        }
        let total_len = self.items_source.len();
        let mut keep_ranges: Vec<(usize, usize)> = Vec::new();
        let mut cursor = 0usize;
        for item in &parsed.program.items {
            let span = item_span(item);
            let item_end = span.offset.saturating_add(span.length);
            // Everything from `cursor` (the end of the previous item, or 0)
            // up to `item_end` belongs to this item — including any leading
            // doc comments / attributes / `pub` keyword that sit before the
            // recorded span.
            let extended_start = cursor;
            cursor = item_end;
            let keep = match item_name(item) {
                Some(n) => !new_names.contains(n),
                None => true,
            };
            if keep {
                keep_ranges.push((extended_start, item_end));
            }
        }
        // Attach any trailing content (final newline, dangling comments) to
        // the last kept item so it doesn't get dropped.
        if let Some(last) = keep_ranges.last_mut() {
            if cursor < total_len {
                last.1 = total_len;
            }
        }
        let mut rebuilt = String::with_capacity(total_len);
        for (start, end) in keep_ranges {
            rebuilt.push_str(&self.items_source[start..end]);
        }
        self.items_source = rebuilt;
    }

    /// Run the resolver + typechecker over the accumulated items so that
    /// item-only cells (struct/fn definitions) report errors immediately
    /// rather than waiting for a future evaluation cell.
    fn validate_accumulated(&mut self) {
        if self.items_source.trim().is_empty() {
            return;
        }
        let parsed = crate::parse(&self.items_source);
        if !parsed.errors.is_empty() {
            for e in &parsed.errors {
                eprintln!("parse error: {}", e.message);
            }
            return;
        }
        let resolved = crate::resolve(&parsed.program);
        if !resolved.errors.is_empty() {
            for e in &resolved.errors {
                eprintln!("resolve error: {}", e.message);
            }
            return;
        }
        let typed = crate::typecheck(&parsed.program, &resolved);
        for e in &typed.errors {
            eprintln!("type error: {}", e.message);
        }
    }

    fn run_with_wrapper(&mut self, cell_src: &str) {
        match self.run_with_wrapper_inner(cell_src, /* capture */ false) {
            Ok(out) => {
                for note in &out.notes {
                    eprintln!("{note}");
                }
                self.capture_new_lets(cell_src);
                let (snapshot, _footer) = self.compute_cell_effect_summary(cell_src);
                self.cell_effect_history.push(snapshot);
            }
            Err(out) => {
                for note in &out.notes {
                    eprintln!("{note}");
                }
                for m in &out.errors {
                    eprintln!("{m}");
                }
                // Production `evaluate_cell` does not roll back
                // `cell_history` on diagnostic-side failure; keep
                // `cell_effect_history` aligned by pushing an empty
                // snapshot for the failed cell.
                self.cell_effect_history.push(CellEffectSnapshot::default());
            }
        }
    }

    /// Run a wrapper cell, optionally capturing the interpreter's stdout
    /// output. The returned `WrapperOutcome` carries either captured
    /// stdout lines (`Ok` shape) or diagnostic strings (`Err` shape), plus
    /// any auto-clone perf notes emitted along the way (always surfaced —
    /// `--auto-clone` is never silent).
    fn run_with_wrapper_inner(
        &mut self,
        cell_src: &str,
        capture: bool,
    ) -> Result<WrapperOutcome, WrapperOutcome> {
        // Shadow-prune: drop any prior persistent let whose name(s) the new
        // cell re-binds. Kāra rejects same-scope re-declaration, so without
        // this prune `cell 1: let x = 1;` followed by `cell 2: let x = 99;`
        // would fail at the resolver inside cell 2's synthetic main. Per
        // design.md § Cell Scope, the later cell shadows the earlier
        // binding — source-replay approximates that by pruning.
        self.prune_shadowed_lets(cell_src);

        // Auto-clone iteration loop: when `auto_clone` is on, cross-cell
        // UAM errors found by the ownership pass drive a source rewrite of
        // the consuming cell's stored slice. The rewritten slice replaces
        // the entry in `persistent_lets` AND in `cell_history` (so `:save`
        // exports the clone'd form). After each successful rewrite the
        // pipeline restarts from synthetic-source assembly. The cap is
        // generous — `persistent_lets.len() + 8` covers every realistic
        // multi-binding consume cell while still bounding pathological
        // input. With the flag off this loop runs at most once and falls
        // through the existing rendering paths unchanged.
        let max_iters = self.persistent_lets.len() + 8;
        let mut notes: Vec<String> = Vec::new();
        for _ in 0..=max_iters {
            let synthetic = self.build_synthetic_cell(cell_src);
            let mut parsed = crate::parse(&synthetic);
            if !parsed.errors.is_empty() {
                return Err(WrapperOutcome::errors(
                    parsed
                        .errors
                        .iter()
                        .map(|e| format!("parse error: {}", e.message))
                        .collect(),
                    notes,
                ));
            }
            let resolved = crate::resolve(&parsed.program);
            if !resolved.errors.is_empty() {
                return Err(WrapperOutcome::errors(
                    self.render_resolve_errors_repl(&resolved.errors),
                    notes,
                ));
            }
            let typed = crate::typecheck(&parsed.program, &resolved);
            if !typed.errors.is_empty() {
                return Err(WrapperOutcome::errors(
                    typed
                        .errors
                        .iter()
                        .map(|e| format!("type error: {}", e.message))
                        .collect(),
                    notes,
                ));
            }

            // Round-trip the program through the ownership checker so cross-
            // cell `UseAfterMove` errors fire in REPL context the same way
            // they fire on `.kara` files. Strictness is identical to the
            // .kara surface; only diagnostic *presentation* is enriched.
            // Rendering happens via `render_ownership_errors_repl`, which
            // appends a notebook-aware tail to UAM errors whose consume site
            // and use site land in different cells.
            let owned = crate::ownershipcheck(&parsed.program, &typed);
            if !owned.errors.is_empty() {
                if self.auto_clone {
                    // Try to rewrite consume sites for the cross-cell UAM
                    // arms. `apply_auto_clone_rewrites` mutates
                    // `persistent_lets` / `cell_history` in place and
                    // returns the perf-note lines for the insertions it
                    // performed. If at least one rewrite happened, restart
                    // the compile pipeline; otherwise fall through to the
                    // baseline rendering path. The rewrites can't restore
                    // a binding the user has already moved within the
                    // same cell — same-cell UAM still surfaces, matching
                    // the slice's "cross-cell only" rule.
                    let new_notes = self.apply_auto_clone_rewrites(&owned.errors);
                    if !new_notes.is_empty() {
                        notes.extend(new_notes);
                        continue;
                    }
                }
                return Err(WrapperOutcome::errors(
                    self.render_ownership_errors_repl(&owned.errors),
                    notes,
                ));
            }

            crate::lower(&mut parsed.program, &typed);

            // Slice c-repl.B.B: when `KARAC_REPL_JIT=1` was set at
            // session construction, route the cell through the
            // persistent `karac_jit_runner --repl-mode` subprocess
            // (slice B.A) instead of the in-process tree-walk
            // interpreter. The branch returns directly with a
            // WrapperOutcome so the interpreter-specific snapshot
            // replay / watch-name setup below is skipped.
            //
            // Slice c-repl.B.5.1: the JIT branch now implements
            // value-snapshot semantics for primitive-typed lets — see
            // `run_cell_via_jit` for the capture/replay handoff via
            // codegen-emitted LLVM globals in the runner's JITDylib.
            #[cfg(feature = "lljit_prototype")]
            if self.jit_enabled {
                return Ok(self.run_cell_via_jit(&parsed.program, &typed, capture, notes));
            }

            let mut interp = crate::interpreter::Interpreter::new(&parsed.program, &typed);
            if capture {
                interp.captured_output = Some(Vec::new());
            }
            // Value-snapshot replay: install pre-loaded values for every
            // persistent-let binding that has a cached value in the
            // session snapshot. The interpreter's Let arm consults
            // `let_value_overrides` keyed on the binding name and skips
            // the RHS when a hit occurs — that's what makes a prior
            // `let log = read_file(…);` stop re-reading the file on
            // subsequent cells. Names from the current cell are NOT
            // overridden (their RHS runs normally and seeds the
            // snapshot for the *next* cell).
            for prior_let in &self.persistent_lets {
                let names = parse_let_binding_names(prior_let);
                for name in names {
                    if let Some(v) = self.let_snapshots.get(&name) {
                        interp.let_value_overrides.insert(name, v.clone());
                    }
                }
            }
            // Watch every top-level `let` binding in the just-built
            // synthetic source (both replayed persistent_lets and the
            // current cell's body). For overridden names the captured
            // value matches the override; for fresh names this is
            // what seeds the snapshot for future cells.
            let mut watch_names: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for prior_let in &self.persistent_lets {
                for n in parse_let_binding_names(prior_let) {
                    watch_names.insert(n);
                }
            }
            for entry in scan_top_level_lets(cell_src) {
                for n in entry.names {
                    watch_names.insert(n);
                }
            }
            interp.let_snapshot_watch = watch_names;
            interp.run();
            let captured = std::mem::take(&mut interp.captured_let_values);
            for (name, val) in captured {
                self.let_snapshots.insert(name, val);
            }
            return Ok(WrapperOutcome {
                stdout: interp.captured_output.take().unwrap_or_default(),
                errors: Vec::new(),
                notes,
            });
        }
        // The loop bound is a defensive cap — every realistic auto-clone
        // sequence converges in `persistent_lets.len() + 1` iterations
        // (one rewrite per consume site, plus a final clean compile).
        // Reaching this branch indicates a pathological repeat-rewrite
        // scenario; surface as a regular ownership error so the user
        // isn't left with a silent failure.
        Err(WrapperOutcome::errors(
            vec!["ownership error: --auto-clone exceeded its rewrite budget; aborting".to_string()],
            notes,
        ))
    }

    /// Apply auto-clone rewrites for the cross-cell `UseAfterMove` arms
    /// in `errors`. Returns one `perf[auto-clone-in-repl]` note per
    /// successful insertion (always non-empty when at least one rewrite
    /// was applied; the caller restarts the compile when the slice is
    /// non-empty). The rewrite locates the consume-site span in the
    /// synthetic source we just built, identifies which `persistent_lets`
    /// slot owns the corresponding bytes, and splices `.clone()` after
    /// the consumed identifier. The matching `cell_history[M-1]` entry
    /// is rewritten in lockstep so `:save` exports the clone'd form.
    fn apply_auto_clone_rewrites(
        &mut self,
        errors: &[crate::ownership::OwnershipError],
    ) -> Vec<String> {
        let mut notes: Vec<String> = Vec::new();
        // Collect rewrite intents first so we don't mutate persistent_lets
        // while iterating over its indices. Each intent records the
        // persistent_lets slot, the local byte offset of the identifier
        // end, the binding name, and the consuming cell index.
        struct Intent {
            slot: usize,
            local_end: usize,
            binding: String,
            consume_cell: usize,
            use_cell: usize,
        }
        let mut intents: Vec<Intent> = Vec::new();
        for err in errors {
            if err.kind != crate::ownership::OwnershipErrorKind::UseAfterMove {
                continue;
            }
            let Some(cs) = err.consume_span.as_ref() else {
                continue;
            };
            let consume_cell = match self.cell_for_span(cs) {
                Some(c) => c,
                None => continue,
            };
            let use_cell = match self.cell_for_span(&err.span) {
                Some(c) => c,
                None => continue,
            };
            // Same-cell UAM is out of scope for auto-clone — the existing
            // diagnostic still fires for it.
            if consume_cell == use_cell {
                continue;
            }
            // Locate the persistent_lets slot whose byte range contains
            // the consume span. Falls through silently if the consume
            // site is in the current cell body (use_cell == current and
            // consume_cell < use_cell means the consume sits in a
            // replayed `let`; that's the case the inheritance caveat
            // from slice 5 narrows us to).
            let Some(slot) = self.persistent_let_slot_for_offset(cs.offset) else {
                continue;
            };
            let slot_range = match self.cell_byte_ranges.iter().find_map(|(_, r)| {
                if r.start <= cs.offset && cs.offset < r.end {
                    Some(r.clone())
                } else {
                    None
                }
            }) {
                Some(r) => r,
                None => continue,
            };
            let local_end = cs
                .offset
                .saturating_sub(slot_range.start)
                .saturating_add(cs.length);
            // Guard: `local_end` must fall inside the persistent_lets
            // slice (the trailing `\n` we appended in build_synthetic_cell
            // sits past the stored slice's end, so subtract that off).
            let stored_len = self.persistent_lets[slot].len();
            if local_end > stored_len {
                continue;
            }
            // Extract the binding name from the consume span bytes — it's
            // a bare identifier, so we can read it straight off
            // persistent_lets[slot].
            let local_start = cs.offset.saturating_sub(slot_range.start);
            if local_start >= stored_len {
                continue;
            }
            let binding = self.persistent_lets[slot][local_start..local_end].to_string();
            intents.push(Intent {
                slot,
                local_end,
                binding,
                consume_cell,
                use_cell,
            });
        }
        if intents.is_empty() {
            return notes;
        }
        // Apply intents grouped by slot, descending by local_end so
        // earlier rewrites inside the same slot don't shift later
        // offsets. Multiple rewrites in one slot is rare (one consumed
        // binding per `let` in v1 source-replay) but the descending
        // sort is cheap insurance.
        intents.sort_by(|a, b| {
            a.slot
                .cmp(&b.slot)
                .then_with(|| b.local_end.cmp(&a.local_end))
        });
        // Track which (slot, local_end) we already rewrote so duplicate
        // diagnostics on the same site don't double-insert.
        let mut applied: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();
        for intent in &intents {
            if !applied.insert((intent.slot, intent.local_end)) {
                continue;
            }
            // Splice `.clone()` into the persistent_lets entry.
            let entry = &mut self.persistent_lets[intent.slot];
            entry.insert_str(intent.local_end, ".clone()");
            // Re-emit the rewritten slice into the matching
            // cell_history entry. Because cell_history records the cell
            // verbatim (a multi-line block possibly containing many
            // statements), we rewrite by recomposing: rebuild the
            // history string from the persistent_lets slots that share
            // this consume_cell origin, joined onto whatever non-`let`
            // remainder the original cell carried. v1 simplification:
            // overwrite the cell_history entry by replacing the bare
            // `<binding>` token's *first* occurrence inside the
            // statement that targeted this consume — same effect as the
            // local splice when the original cell was the `let _t =
            // consume(s);` shape the slice's caveat narrows us to.
            let cell_idx0 = intent.consume_cell.saturating_sub(1);
            if let Some(cell_src) = self.cell_history.get_mut(cell_idx0) {
                rewrite_cell_history_consume(cell_src, &intent.binding);
            }
            notes.push(format!(
                "perf[auto-clone-in-repl]: inserted `.clone()` on `{}` at consume site (cell {}, used in cell {}); cross-cell binding kept alive — disable with `karac repl` (no `--auto-clone`) to surface the underlying use-after-move diagnostic instead",
                intent.binding, intent.consume_cell, intent.use_cell,
            ));
        }
        notes
    }

    /// Locate the index in `persistent_lets` whose backing byte range
    /// (inside the most recently built synthetic source) contains
    /// `offset`. Returns `None` when the offset falls outside every
    /// stored slot — including when it lands inside the current cell's
    /// body block (the trailing `cell_byte_ranges` entry tagged with the
    /// current 1-based cell index, which is *not* a persistent_lets slot
    /// yet). Callers use this to decide whether the consume-site rewrite
    /// targets a replayed `let` or the freshly-submitted cell body. The
    /// slot index found is returned in `persistent_lets` order — the
    /// same order ranges are emitted in `build_synthetic_cell`.
    fn persistent_let_slot_for_offset(&self, offset: usize) -> Option<usize> {
        // The first `persistent_lets.len()` entries in `cell_byte_ranges`
        // map onto the persistent_lets slots in order (skipping any
        // entries whose origin was the sentinel 0 — which `capture_new_lets`
        // never produces in practice but the legacy fallback in
        // `build_synthetic_cell` still defends). The trailing entry is
        // the current cell's body block; never a persistent_lets slot.
        // We mirror that iteration order here.
        let n = self.persistent_lets.len();
        for (i, range) in self.cell_byte_ranges.iter().take(n).enumerate() {
            if range.1.start <= offset && offset < range.1.end {
                return Some(i);
            }
        }
        None
    }

    /// Render resolver errors with REPL-aware enrichment for the
    /// "binding declared inside a now-closed `:provide` scope" case.
    /// When the resolver reports `undefined name 'X'` and `X` matches
    /// Slice c-repl.B.B: run the just-typechecked program through the
    /// persistent `karac_jit_runner --repl-mode` subprocess. Lazily
    /// spawns the runner on the first cell; re-spawns after a cell-
    /// induced exit (`emit_panic`'s `exit(1)`, runtime panics)
    /// terminates it. Each cell increments `jit_next_cell_id` so the
    /// runner's echoed `result <id>` line cross-checks framing
    /// integrity.
    ///
    /// Slice c-repl.B.5.1: implements value-snapshot semantics for
    /// primitive-typed lets via codegen-emitted LLVM globals as a
    /// cross-cell side channel. Cell N's top-level `let x = expr` of
    /// i64/f64/bool/char type writes its bound value into
    /// `@__karac_repl_snapshot_x` (External linkage, lives in the
    /// runner's JITDylib past the cell's tracker scope); subsequent
    /// cells' codegen skips the original RHS and binds `x` to a load
    /// from the same global. Closes the interpreter-vs-JIT semantic
    /// gap for the canonical `let log = read_file(…)` pattern.
    /// Non-primitive types (String, Vec, Map, structs) still fall
    /// through to the re-evaluating path; see slice c-repl.B.5
    /// follow-ons for those.
    #[cfg(feature = "lljit_prototype")]
    fn run_cell_via_jit(
        &mut self,
        program: &crate::ast::Program,
        typed: &crate::typechecker::TypeCheckResult,
        capture: bool,
        notes: Vec<String>,
    ) -> WrapperOutcome {
        use jit_runner_client::{CellResult, ReplRunnerClient};

        self.jit_next_cell_id += 1;
        let cell_id = self.jit_next_cell_id;
        let main_symbol = format!("cell_main_{cell_id}");

        // Slice c-repl.B.5.1: classify this cell's top-level lets into
        // replay (prior cell stashed a primitive value into a
        // snapshot global — codegen loads from the global instead of
        // re-evaluating) and capture (new binding with a primitive
        // type — codegen stores the bound value into a fresh global
        // so future cells can replay it). The classification reads
        // the synthesized program directly so the binding's pattern
        // span lines up with the typechecker's recorded inferred
        // type.
        let (snapshot_replay, snapshot_capture) =
            self.compute_snapshot_sets_for_cell(program, typed);

        // Slice c-repl.B.4: prior cells' fn bodies live in the
        // runner's JITDylib. Emit them as `declare`-only in this
        // cell's IR so codegen + jitlink skip the body for them;
        // the JIT linker resolves call sites against the
        // already-installed definitions.
        let ir = match crate::codegen::compile_to_ir_for_repl_cell_with_snapshots(
            program,
            &self.jit_installed_fns,
            &main_symbol,
            &snapshot_capture,
            &snapshot_replay,
        ) {
            Ok(s) => s,
            Err(e) => {
                return WrapperOutcome::errors(vec![format!("JIT codegen failed: {e}")], notes);
            }
        };

        if self.jit_client.is_none() {
            match ReplRunnerClient::spawn() {
                Ok(c) => self.jit_client = Some(c),
                Err(e) => {
                    return WrapperOutcome::errors(
                        vec![format!("JIT runner spawn failed: {e}")],
                        notes,
                    );
                }
            }
        }
        let client = self
            .jit_client
            .as_mut()
            .expect("jit_client just initialized");

        let result = client.run_cell(cell_id, &ir);

        match result {
            CellResult::Completed {
                exit,
                stdout,
                stderr,
            } => {
                let mut errors: Vec<String> = Vec::new();
                if exit != 0 {
                    errors.push(format!("cell exited with code {exit}"));
                    let stderr_str = String::from_utf8_lossy(&stderr);
                    for line in stderr_str.lines() {
                        if !line.is_empty() {
                            errors.push(line.to_string());
                        }
                    }
                } else {
                    // Slice c-repl.B.4: cell ran clean — every fn the
                    // codegen path emitted with a body (i.e., every
                    // program fn NOT already in jit_installed_fns) is
                    // now live in the runner's JITDylib. Add their
                    // names so the next cell emits them as
                    // declare-only. Done only on `exit == 0` — a
                    // failed cell may have aborted mid-init and we
                    // don't want a half-installed symbol to shadow
                    // a future cell's correct definition.
                    //
                    // EXCLUDES `main`: each cell's synthesized
                    // `fn main` is registered in LLVM under
                    // `cell_main_<id>` via `main_symbol_override`, so
                    // the AST name "main" maps to a different LLVM
                    // symbol every cell. If we added "main" to the
                    // installed set, cell N+1's codegen would see
                    // its OWN `fn main()` matched against declare-
                    // only and skip the body emission — installing
                    // a body-less `cell_main_<N+1>` that crashes on
                    // call.
                    for item in &program.items {
                        if let crate::ast::Item::Function(f) = item {
                            if f.generic_params.is_some() {
                                continue;
                            }
                            if f.name == "main" {
                                continue;
                            }
                            if !self.jit_installed_fns.contains(&f.name) {
                                self.jit_installed_fns.insert(f.name.clone());
                            }
                        }
                    }
                    // Slice c-repl.B.5.1: every let in `snapshot_capture`
                    // has been materialized into a live global in the
                    // runner's JITDylib by this cell's main. Record the
                    // names + their primitive kinds so subsequent cells
                    // know to take the replay path. Skipped on cell
                    // failure (same reasoning as `jit_installed_fns`:
                    // a half-initialized symbol shouldn't shadow a
                    // future correct emission).
                    for (name, kind) in &snapshot_capture {
                        self.jit_snapshotted_lets.insert(name.clone(), *kind);
                    }
                }
                WrapperOutcome {
                    stdout: if capture {
                        bytes_to_stdout_lines(&stdout)
                    } else {
                        Vec::new()
                    },
                    errors,
                    notes,
                }
            }
            CellResult::RunnerDied {
                partial_stdout,
                runner_stderr,
                wait_status,
            } => {
                // Drop the dead client; the next cell re-spawns.
                self.jit_client = None;
                // Slice c-repl.B.4: the fresh runner will spawn with
                // an empty JITDylib, so every fn must be re-emitted
                // with its body on the next cell. Clear the installed
                // set to reflect that.
                self.jit_installed_fns.clear();
                // Slice c-repl.B.5.1: snapshot globals lived in the
                // dead runner's JITDylib — they're gone too. Clear so
                // subsequent cells re-take the capture path for every
                // persistent let, not a replay path that would
                // resolve to an unmapped symbol.
                self.jit_snapshotted_lets.clear();
                let exit_code = wait_status.and_then(|s| s.code());
                let mut errors = vec![format!(
                    "JIT runner subprocess died mid-cell (exit code {:?}); \
                     the cell's code likely tripped emit_panic's exit(1). \
                     A fresh runner will spawn on the next cell.",
                    exit_code
                )];
                let runner_stderr_text = String::from_utf8_lossy(&runner_stderr);
                for line in runner_stderr_text.lines() {
                    if !line.is_empty() {
                        errors.push(format!("runner stderr: {line}"));
                    }
                }
                WrapperOutcome {
                    stdout: if capture {
                        bytes_to_stdout_lines(&partial_stdout)
                    } else {
                        Vec::new()
                    },
                    errors,
                    notes,
                }
            }
        }
    }

    /// Slice c-repl.B.5.1: classify every top-level `let` in this
    /// cell's synthetic `fn main` into one of three bins:
    ///
    ///   - **Replay**: binding name already lives in
    ///     `jit_snapshotted_lets` (a prior cell stashed a primitive
    ///     value into the cell-spanning global). Codegen will skip
    ///     the original RHS and emit a load from the global.
    ///
    ///   - **Capture**: binding name is NOT in `jit_snapshotted_lets`
    ///     AND the inferred type is one of the supported primitives
    ///     (i64 / f64 / bool / char) AND the pattern is a single
    ///     `PatternKind::Binding` (destructuring deferred). Codegen
    ///     will emit the let normally and stash the bound value into
    ///     a fresh global so future cells can replay it.
    ///
    ///   - **Pass-through** (neither map populated): everything
    ///     else — destructuring lets, non-primitive types, wildcards,
    ///     `let mut` rebinding chains, etc. The original RHS still
    ///     runs every cell as in pre-B.5.1 behavior. Closing those
    ///     gaps is the work of B.5.2 (String) and beyond.
    ///
    /// The classification reads the typechecker's `expr_types` table
    /// keyed by the let's value expression span. Type extraction
    /// goes through `snapshot_kind_for_type` which intentionally
    /// only accepts the explicit primitive forms (so an `i32` or
    /// `u64` doesn't quietly get stashed at the wrong storage width).
    #[cfg(feature = "lljit_prototype")]
    fn compute_snapshot_sets_for_cell(
        &self,
        program: &crate::ast::Program,
        typed: &crate::typechecker::TypeCheckResult,
    ) -> (
        std::collections::HashMap<String, crate::codegen::SnapshotPrimKind>,
        std::collections::HashMap<String, crate::codegen::SnapshotPrimKind>,
    ) {
        use crate::ast::{Item, PatternKind, StmtKind};
        use crate::resolver::SpanKey;
        let mut replay = std::collections::HashMap::new();
        let mut capture = std::collections::HashMap::new();
        let Some(main_fn) = program.items.iter().find_map(|item| match item {
            Item::Function(f) if f.name == "main" => Some(f),
            _ => None,
        }) else {
            return (replay, capture);
        };
        for stmt in &main_fn.body.stmts {
            let StmtKind::Let {
                is_mut,
                pattern,
                value,
                ..
            } = &stmt.kind
            else {
                continue;
            };
            let PatternKind::Binding(name) = &pattern.kind else {
                continue;
            };
            // Replay path wins: if this binding already has a global
            // installed in the runner, we ALWAYS route through the
            // load-from-global codepath rather than re-emit the RHS
            // and reinstall the global. (The same-cell collision
            // case — a let with a name that matches a prior snapshot
            // — is structurally impossible: Kāra's resolver rejects
            // same-scope re-declaration before codegen runs.)
            if let Some(kind) = self.jit_snapshotted_lets.get(name) {
                replay.insert(name.clone(), *kind);
                continue;
            }
            let Some(ty) = typed.expr_types.get(&SpanKey::from_span(&value.span)) else {
                continue;
            };
            if let Some(kind) = snapshot_kind_for_type(ty) {
                // Slice c-repl.B.5.2: skip `let mut s = …` for String
                // bindings. Capture transfers buffer ownership from
                // the let slot to the snapshot global by zeroing the
                // slot's cap, which leaves the binding intact for
                // reads but breaks same-cell `s.push_str(…)` — push
                // would read cap=0, realloc into a fresh buffer, and
                // the global ends up pointing at the original buffer
                // while the slot points at the new one; cell N+1's
                // replay then loads the old buffer (pre-push) and
                // diverges from the interpreter's post-mutation
                // snapshot. Pass-through gives correct (if slower)
                // re-evaluation semantics. Primitive kinds keep the
                // B.5.1 behavior — mut primitives don't have the
                // same alias hazard.
                // Slice c-repl.B.5.3 extends the mut filter to Vec.
                // Same alias-hazard reasoning as the String case: a
                // same-cell `xs.push(…)` after capture would read
                // cap=0 (the suppression sentinel), realloc into a
                // fresh buffer, leave the snapshot global pointing at
                // the pre-push buffer, and cell N+1's replay would
                // load the pre-push triple and diverge from the
                // interpreter's post-mutation snapshot. Pass-through
                // (no capture, no cap-zero) preserves correct (if
                // slower) semantics. Primitive kinds keep the B.5.1
                // unfiltered behavior — they have no alias hazard.
                if *is_mut
                    && matches!(
                        kind,
                        crate::codegen::SnapshotPrimKind::String
                            | crate::codegen::SnapshotPrimKind::Vec(_)
                    )
                {
                    continue;
                }
                capture.insert(name.clone(), kind);
            }
        }
        (replay, capture)
    }

    /// an entry in `pruned_provider_lets`, append a notebook-aware
    /// tail naming the provider scope that closed and the cell where
    /// `X` was originally declared (design.md § Cross-Cell Providers
    /// diagnostic shape). Other resolver errors use the existing
    /// `resolve error: <message>` rendering verbatim — strictness is
    /// identical to `.kara` files; only presentation differs.
    fn render_resolve_errors_repl(&self, errors: &[crate::resolver::ResolveError]) -> Vec<String> {
        let mut rendered: Vec<String> = Vec::with_capacity(errors.len());
        for err in errors {
            let mut line = format!("resolve error: {}", err.message);
            if let Some(name) = extract_undefined_name(&err.message) {
                if let Some(pruned) = self
                    .pruned_provider_lets
                    .iter()
                    .find(|p| p.binding_name == name)
                {
                    line.push_str(&format!(
                        "\n  note: `{}` was declared inside `:provide {}` (cell {}) and went out of scope when `:end-provide {}` ran in cell {}",
                        pruned.binding_name,
                        pruned.pruned_by_resource,
                        pruned.declared_in_cell,
                        pruned.pruned_by_resource,
                        pruned.pruned_at_cell,
                    ));
                    line.push_str(&format!(
                        "\n  hint: extract `{}` to a cell BEFORE `:provide {}` (cell {}), or delay `:end-provide {}` until you no longer need the binding in later cells",
                        pruned.binding_name,
                        pruned.pruned_by_resource,
                        pruned.scope_opened_cell,
                        pruned.pruned_by_resource,
                    ));
                }
            }
            rendered.push(line);
        }
        rendered
    }

    /// Render ownership errors with REPL-aware enrichment for
    /// `UseAfterMove` diagnostics. When the consume site and the use site
    /// resolve to different cells (via `cell_for_span` over the just-built
    /// `cell_byte_ranges`), append a notebook-aware tail naming the
    /// consuming cell and pointing the user at `.clone()` at the consume
    /// site. Same-cell UAM and every non-UAM kind use the existing
    /// rendering verbatim — strictness is identical to `.kara` files; only
    /// presentation differs in REPL context.
    fn render_ownership_errors_repl(
        &self,
        errors: &[crate::ownership::OwnershipError],
    ) -> Vec<String> {
        let mut rendered: Vec<String> = Vec::with_capacity(errors.len());
        for err in errors {
            let mut line = format!("ownership error: {}", err.message);
            if let Some(s) = err.suggestion.as_deref() {
                line.push_str("\n  help: ");
                line.push_str(s);
            }
            // Notebook-aware tail: only fires for UseAfterMove with a
            // consume-site span that resolves to a different cell than
            // the use-site span. Same-cell UAM and any kind without a
            // recorded `consume_span` fall through unchanged.
            if err.kind == crate::ownership::OwnershipErrorKind::UseAfterMove {
                if let Some(consume_span) = err.consume_span.as_ref() {
                    let consume_cell = self.cell_for_span(consume_span);
                    let use_cell = self.cell_for_span(&err.span);
                    if let (Some(c), Some(u)) = (consume_cell, use_cell) {
                        if c != u {
                            line.push_str(&format!(
                                "\n  note: consumed by cell {c} (`{}`); add `.clone()` at the consume site to keep the original binding usable in later cells",
                                cell_preview(self.cell_history.get(c.saturating_sub(1)).map(String::as_str).unwrap_or("")),
                            ));
                        }
                    }
                }
            }
            rendered.push(line);
        }
        rendered
    }

    /// Test helper: evaluate a cell and return any captured output and
    /// surfaced errors. Mirrors `evaluate_cell` but routes the
    /// interpreter's stdout into an in-memory buffer instead of the
    /// process's stdout.
    pub fn evaluate_cell_captured(&mut self, src: &str) -> EvaluatedCell {
        let trimmed = src.trim_end();
        self.cell_history.push(trimmed.to_string());

        if classify_input(trimmed) == CellShape::PureItems {
            self.append_items(trimmed);
            // Mirror validate_accumulated but collect errors for return.
            let mut errs = Vec::new();
            if !self.items_source.trim().is_empty() {
                let parsed = crate::parse(&self.items_source);
                for e in &parsed.errors {
                    errs.push(format!("parse error: {}", e.message));
                }
                if parsed.errors.is_empty() {
                    let resolved = crate::resolve(&parsed.program);
                    for e in &resolved.errors {
                        errs.push(format!("resolve error: {}", e.message));
                    }
                    let typed = crate::typecheck(&parsed.program, &resolved);
                    for e in &typed.errors {
                        errs.push(format!("type error: {}", e.message));
                    }
                }
            }
            // Roll back history if pure-item parse trips an error so :save
            // never bakes in a broken cell.
            if !errs.is_empty() {
                // Drop the failing cell from history AND from items_source —
                // the failing definition would block all subsequent cells.
                self.cell_history.pop();
                let trimmed_added = trimmed.to_string();
                if let Some(idx) = self.items_source.rfind(&trimmed_added) {
                    self.items_source.truncate(idx);
                }
            } else {
                // Pure-item cell ran clean — record an empty effect
                // snapshot so the timeline's cell index stays aligned
                // with `cell_history`.
                self.cell_effect_history.push(CellEffectSnapshot::default());
            }
            return EvaluatedCell {
                stdout: String::new(),
                errors: errs,
                notes: Vec::new(),
                effect_footer: String::new(),
            };
        }

        // Statement / expression / mixed cell.
        match self.run_with_wrapper_inner(trimmed, /* capture */ true) {
            Ok(out) => {
                self.capture_new_lets(trimmed);
                let (snapshot, effect_footer) = self.compute_cell_effect_summary(trimmed);
                self.cell_effect_history.push(snapshot);
                // Slice c-repl.B.B: pre-cutover the interpreter path
                // never populated `out.errors` on the `Ok` arm
                // (runtime errors flowed into `runtime_errors` inside
                // the interpreter and surfaced as test-runner side
                // outcomes, not via this channel). The JIT path
                // (`run_cell_via_jit`) does populate `errors` when a
                // cell exits non-zero or the runner dies — propagate
                // those so the user sees diagnostics. Interpreter
                // path keeps its existing behavior (out.errors is
                // empty there).
                EvaluatedCell {
                    stdout: out.stdout.join(""),
                    errors: out.errors,
                    notes: out.notes,
                    effect_footer,
                }
            }
            Err(out) => {
                // Roll back history on diagnostic-side failure.
                self.cell_history.pop();
                EvaluatedCell {
                    stdout: String::new(),
                    errors: out.errors,
                    notes: out.notes,
                    effect_footer: String::new(),
                }
            }
        }
    }

    /// Synthesize the source text fed to the parser for a non-pure-item cell:
    /// `<items_source>\nfn main() { <persistent_lets>\n<cell_body> }`.
    /// Using `main` (not `__cell_N`) ensures `Interpreter::run` finds the
    /// entry point automatically. If the user already declared `main` in
    /// `items_source`, the new one shadows it (Cell Scope re-declaration
    /// rule from design.md). The persistent-let replay block sits at the
    /// top of the wrapper body so each cell sees the same flat scope as
    /// design.md § Cell Scope describes.
    ///
    /// Side effect: rebuilds `Session.cell_byte_ranges` so it reflects the
    /// just-assembled source. Each replayed `let` slice is tagged with
    /// the cell index that originally introduced it (from
    /// `persistent_let_origin`); the trailing `cell_src` block is tagged
    /// with the *current* cell's 1-based index (the one being submitted).
    fn build_synthetic_cell(&mut self, cell_src: &str) -> String {
        let mut s = String::new();
        s.push_str(&strip_main(&self.items_source));
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("fn main() {\n");
        let mut ranges: Vec<(usize, Range<usize>)> = Vec::new();
        for (i, prior_let) in self.persistent_lets.iter().enumerate() {
            let start = s.len();
            s.push_str(prior_let);
            if !prior_let.ends_with('\n') {
                s.push('\n');
            }
            let end = s.len();
            // Origin index 0 (a sentinel) means "unknown cell" — should
            // not occur with correct upkeep but if it does, skip the
            // mapping rather than synthesize a misleading number.
            if let Some(&cell_idx) = self.persistent_let_origin.get(i) {
                if cell_idx > 0 {
                    ranges.push((cell_idx, start..end));
                }
            }
        }
        // The cell currently being evaluated occupies the next contiguous
        // slice of the wrapper body. Its 1-based index is one past the
        // current `cell_history` length (the entry has already been pushed
        // by the caller before `build_synthetic_cell` runs).
        //
        // When the provider stack is non-empty, the cell body is wrapped
        // in one `with_provider[R](expr, || { … })` block per active
        // frame (innermost-first nesting so outermost ends up on the
        // outside). The wrap rides into the same byte-range the cell
        // would occupy unwrapped, so UAM spans landing anywhere inside
        // the wrap still map back to the current cell via
        // `cell_for_span`. Persistent-let replay continues to sit OUTSIDE
        // every wrap — bindings declared while a scope was open keep
        // replaying after `:end-provide` until slice 4's pruning lands;
        // for those bindings the `let_snapshots` cache short-circuits
        // RHS re-evaluation so the replay doesn't actually re-touch the
        // closed-scope resource at runtime.
        let cell_idx = self.cell_history.len();
        let body_start = s.len();
        let wrapped_body = self.wrap_in_active_providers(cell_src);
        s.push_str(&wrapped_body);
        if !wrapped_body.ends_with('\n') {
            s.push('\n');
        }
        let body_end = s.len();
        if cell_idx > 0 {
            ranges.push((cell_idx, body_start..body_end));
        }
        s.push_str("}\n");
        self.cell_byte_ranges = ranges;
        s
    }

    /// Extract every top-level `let` / `let mut` / `let ... else` statement
    /// from the cell body and append its raw source to `persistent_lets`.
    /// Called only after the cell evaluated cleanly so failed cells never
    /// leak partial bindings forward. Shadow-pruning happened earlier in
    /// `prune_shadowed_lets` so the synthetic main could parse cleanly;
    /// at this point the new entries can simply append.
    ///
    /// Each new entry is tagged with the 1-based index of the current cell
    /// in `persistent_let_origin` so the diagnostic-rendering layer can map
    /// a span landing inside the replayed slice back to the originating
    /// cell.
    fn capture_new_lets(&mut self, cell_src: &str) {
        // Cell index = `cell_history.len()` because the caller pushed the
        // current cell's source before invoking the wrapper pipeline.
        let cell_idx = self.cell_history.len();
        // Snapshot the active provider scope (resource names) so a future
        // `:end-provide R` knows whether each binding was declared inside
        // its scope. Empty Vec for bindings captured outside any scope.
        let active_scope: Vec<String> = self
            .provider_stack
            .iter()
            .map(|f| f.resource.clone())
            .collect();
        for entry in scan_top_level_lets(cell_src) {
            // A new binding with the same name as a previously-pruned one
            // nullifies the prune diagnostic — the user re-bound it, so
            // future "undefined name" errors for it would be unrelated.
            for n in &entry.names {
                self.pruned_provider_lets.retain(|p| &p.binding_name != n);
            }
            self.persistent_lets.push(entry.slice);
            self.persistent_let_origin.push(cell_idx);
            self.persistent_let_provider_scope
                .push(active_scope.clone());
        }
    }

    /// Drop every persistent `let` whose pattern binds a name the new cell
    /// is about to re-bind. Runs before the synthetic main is constructed
    /// so the pre-shadow binding is gone by the time the resolver sees the
    /// concatenated body. If the cell fails to evaluate, the prune is left
    /// applied — the older binding is conceptually superseded the moment
    /// the user typed the new `let` even if the cell itself errored. This
    /// matches the design.md "later cell shadows" wording at the cost of
    /// an edge case: a typo'd cell that fails type-check still drops the
    /// prior entry. Acceptable for the v1 source-replay model; users can
    /// re-bind explicitly in the next cell.
    fn prune_shadowed_lets(&mut self, cell_src: &str) {
        let mut new_names = std::collections::HashSet::new();
        for entry in scan_top_level_lets(cell_src) {
            new_names.extend(entry.names);
        }
        if new_names.is_empty() {
            return;
        }
        // Drop snapshot entries for any name the new cell is about to
        // re-bind. Without this, a `let x = 5;` (i64) followed by `let
        // x = "hello";` (String) would re-use the stale i64 snapshot
        // when cell 2's source-replay form runs in cell 3+. Clearing
        // here forces the new RHS to evaluate the first time it
        // appears (it's the *current* cell's let, not a replay, so the
        // override is empty anyway — but if the user submits the same
        // cell again later, the override would otherwise kick in with
        // the stale type).
        for name in &new_names {
            self.let_snapshots.remove(name);
        }
        // Slice c-repl.B.5.1 follow-up: under JIT mode the snapshot
        // value lives in the runner's JITDylib as
        // `@__karac_repl_snapshot_<name>`. Cross-cell shadow without
        // a JIT-side clear would route the new cell through the
        // REPLAY classification (name still in `jit_snapshotted_lets`)
        // → codegen emits a load from the stale global → wrong value.
        // Per-cell global isolation (the B.4 W2 finding's documented
        // path) would let us drop just the one global; until then,
        // the cleanest correct fix is to drop the whole runner on
        // shadow — next cell starts with an empty JITDylib so the
        // new RHS evaluates fresh. Trade-off: cached fn definitions
        // also re-emit, but shadows are explicit user actions and
        // not the steady-state pattern.
        #[cfg(feature = "lljit_prototype")]
        if new_names
            .iter()
            .any(|n| self.jit_snapshotted_lets.contains_key(n))
        {
            self.jit_installed_fns.clear();
            self.jit_snapshotted_lets.clear();
            self.jit_client = None;
        }
        // Walk persistent_lets, persistent_let_origin, and
        // persistent_let_provider_scope in lockstep so the parallel
        // metadata stays aligned with the slices.
        let mut kept_lets: Vec<String> = Vec::with_capacity(self.persistent_lets.len());
        let mut kept_origin: Vec<usize> = Vec::with_capacity(self.persistent_let_origin.len());
        let mut kept_scope: Vec<Vec<String>> =
            Vec::with_capacity(self.persistent_let_provider_scope.len());
        for (i, prior) in self.persistent_lets.iter().enumerate() {
            let prior_names = parse_let_binding_names(prior);
            if !prior_names.iter().any(|n| new_names.contains(n)) {
                kept_lets.push(prior.clone());
                kept_origin.push(*self.persistent_let_origin.get(i).unwrap_or(&0));
                kept_scope.push(
                    self.persistent_let_provider_scope
                        .get(i)
                        .cloned()
                        .unwrap_or_default(),
                );
            }
        }
        self.persistent_lets = kept_lets;
        self.persistent_let_origin = kept_origin;
        self.persistent_let_provider_scope = kept_scope;
    }

    /// Drop every persistent `let` binding accumulated so far. Items
    /// (`fn` / `struct` / etc.) and cell history are left intact. Also
    /// clears the value-snapshot cache so a subsequent re-bind starts
    /// fresh (the source-replay buffer being empty means there's
    /// nothing to override on the next cell anyway, but the explicit
    /// clear keeps the two stores in sync).
    pub fn reset_persistent_lets(&mut self) {
        self.persistent_lets.clear();
        self.persistent_let_origin.clear();
        self.persistent_let_provider_scope.clear();
        self.let_snapshots.clear();
        self.pruned_provider_lets.clear();
        // Slice c-repl.B.B: under JIT mode the persistent-let bindings
        // live as snapshot globals inside the runner's JITDylib, and
        // their cached primitive kinds live in `jit_snapshotted_lets`.
        // Clearing only the source-replay slate leaves the runner with
        // stale globals — the next cell whose binding name collides
        // with a prior one would take the replay path and load the
        // dead value. Drop the whole runner so the next cell respawns
        // with a fresh, empty JITDylib; `jit_installed_fns` clears in
        // step so fn bodies re-emit instead of going declare-only.
        #[cfg(feature = "lljit_prototype")]
        {
            self.jit_installed_fns.clear();
            self.jit_snapshotted_lets.clear();
            self.jit_client = None;
        }
    }

    /// Type a single expression by wrapping it as `let __t = <expr>;` inside
    /// a synthetic main and reporting the inferred type of the binding.
    fn show_type(&self, expr: &str) {
        let mut s = String::new();
        s.push_str(&strip_main(&self.items_source));
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("fn main() { let __t = ");
        s.push_str(expr.trim());
        s.push_str("; }\n");

        let parsed = crate::parse(&s);
        if !parsed.errors.is_empty() {
            for e in &parsed.errors {
                eprintln!("parse error: {}", e.message);
            }
            return;
        }
        let resolved = crate::resolve(&parsed.program);
        let typed = crate::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            // Surface resolve-side and type-side errors uniformly.
            for e in &typed.errors {
                eprintln!("type error: {}", e.message);
            }
            return;
        }

        // Find the `__t` binding's recorded type. The typechecker stores
        // expression types keyed by span; for the MVP we just print the
        // RHS expression's type by re-walking the parsed AST.
        if let Some(ty) = lookup_let_rhs_type(&parsed.program, &typed, "__t") {
            println!("{ty}");
        } else {
            eprintln!("(could not infer type — typechecker did not record one)");
        }
    }

    /// Print the union of every cell's effect set so far. v1 MVP runs the
    /// effect checker over the accumulated items + a synthetic main built
    /// from concatenating every prior statement-style cell.
    fn show_effects(&self) {
        // For the MVP, just compute effects on items_source. A richer
        // version that tracks per-cell effects belongs with the Jupyter
        // kernel CR (post-MVP).
        if self.items_source.trim().is_empty() {
            println!("(no items defined yet)");
            return;
        }
        let parsed = crate::parse(&self.items_source);
        if !parsed.errors.is_empty() {
            eprintln!("(items_source has parse errors — fix them first)");
            return;
        }
        let effects = crate::effectcheck(&parsed.program);
        // Surface every function's inferred effect set. Empty sets are
        // skipped so the output focuses on functions that actually touch
        // resources.
        let mut printed = 0usize;
        let mut entries: Vec<(&String, &crate::effectchecker::EffectSet)> =
            effects.inferred_effects.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (name, eff) in entries {
            if eff.effects.is_empty() {
                continue;
            }
            let mut parts: Vec<String> = eff
                .effects
                .iter()
                .map(|e| format!("{:?}", e.effect))
                .collect();
            parts.sort();
            parts.dedup();
            println!("{name}: {}", parts.join(", "));
            printed += 1;
        }
        if printed == 0 {
            println!("(no effects inferred so far)");
        }
    }

    fn save(&self, path: &str) {
        if self.cell_history.is_empty() {
            eprintln!("(no cells to save)");
            return;
        }
        let out = self.render_exported_session();
        match std::fs::write(path, out) {
            Ok(()) => println!("session saved to {path}"),
            Err(e) => eprintln!("error: failed to write {path}: {e}"),
        }
    }

    /// Render the session's cells as a single `.kara` source string that
    /// `karac build` accepts and that reproduces the session's observable
    /// behavior when run. Items are hoisted to file scope (using
    /// `items_source`, which is already shadow-pruned so the latest
    /// definition of a re-declared item is the only one emitted).
    /// Statement-style cells are concatenated, in submission order, into
    /// the body of a synthetic `fn main()`. The effect annotations on
    /// `main` mirror the session's accumulated effect set as inferred
    /// from the synthesized program — over-declaration is harmless and
    /// matches the spec promise that the wrapper carries declared
    /// effects. Auto-clone rewrites land in `cell_history` in lockstep
    /// at insertion time, so they ride out of this method unchanged.
    ///
    /// Public so integration tests can inspect the rendered string
    /// without an intermediate file write.
    pub fn render_exported_session(&self) -> String {
        let items_section = self.items_source.trim_end().to_string();

        // When the session never opened a provider scope, the simpler
        // flat path matches the existing behavior byte-for-byte. The
        // timeline-aware path only kicks in for sessions that touched
        // `:provide` / `:end-provide`.
        let body = if self.provider_history.is_empty() {
            let stmt_cells: Vec<&str> = self
                .cell_history
                .iter()
                .filter(|c| classify_input(c) != CellShape::PureItems)
                .map(String::as_str)
                .collect();
            render_main_body(&stmt_cells)
        } else {
            self.render_exported_main_body_with_providers()
        };
        let effects = self.compute_main_effect_decls(&items_section, &body);

        let mut out = String::new();
        out.push_str("// Saved Kāra REPL session.\n");
        out.push_str("// Re-runs via `karac build` for identical observable behavior.\n");
        if self.auto_clone {
            out.push_str(
                "// (`--auto-clone` insertions, if any, are baked into the cell bodies below.)\n",
            );
        }
        out.push('\n');
        if !items_section.is_empty() {
            out.push_str(&items_section);
            out.push_str("\n\n");
        }
        out.push_str("fn main()");
        if !effects.is_empty() {
            out.push(' ');
            out.push_str(&effects);
        }
        out.push_str(" {\n");
        out.push_str(&body);
        out.push_str("}\n");
        out
    }

    /// Render the synthetic `fn main()` body with one nested
    /// `with_provider[R](expr, || { … })` block per `:provide` /
    /// `:end-provide` pair from `provider_history`. Cells that ran
    /// between a pair land inside that pair's closure body; cells
    /// outside any pair land at the top level of the rendered body.
    /// Empty provider blocks (a `:provide` immediately followed by
    /// `:end-provide` with no cells between) collapse to nothing so the
    /// export stays minimal.
    ///
    /// Walks `cell_history` and `provider_history` in lockstep — an
    /// event with `cells_seen == N` fires BEFORE the cell at index N
    /// (0-based) runs, matching what the recording sites in
    /// `add_provider` / `end_provider` record. Pure-items cells are
    /// skipped (they're already at file scope via `items_source`).
    /// Unbalanced opens at session end (`:provide A` without a matching
    /// `:end-provide`) close implicitly so the rendered file is
    /// well-formed.
    fn render_exported_main_body_with_providers(&self) -> String {
        // Interleave cell events with provider events into a single
        // linear stream, then fold the stream into a tree.
        enum Item<'a> {
            Cell(&'a str),
            Open {
                resource: &'a str,
                expr_src: &'a str,
            },
            Close,
        }

        let events = &self.provider_history;
        let mut merged: Vec<Item<'_>> = Vec::new();
        let mut event_idx = 0usize;
        for (cell_idx, cell_src) in self.cell_history.iter().enumerate() {
            while event_idx < events.len() && events[event_idx].cells_seen <= cell_idx {
                match &events[event_idx].kind {
                    ProviderHistoryKind::Open { resource, expr_src } => merged.push(Item::Open {
                        resource: resource.as_str(),
                        expr_src: expr_src.as_str(),
                    }),
                    ProviderHistoryKind::Close { .. } => merged.push(Item::Close),
                }
                event_idx += 1;
            }
            if classify_input(cell_src) != CellShape::PureItems {
                merged.push(Item::Cell(cell_src.as_str()));
            }
        }
        while event_idx < events.len() {
            match &events[event_idx].kind {
                ProviderHistoryKind::Open { resource, expr_src } => merged.push(Item::Open {
                    resource: resource.as_str(),
                    expr_src: expr_src.as_str(),
                }),
                ProviderHistoryKind::Close { .. } => merged.push(Item::Close),
            }
            event_idx += 1;
        }

        let mut children_stack: Vec<Vec<ExportNode>> = vec![Vec::new()];
        let mut meta_stack: Vec<(String, String)> = Vec::new();
        for item in &merged {
            match item {
                Item::Cell(src) => {
                    children_stack
                        .last_mut()
                        .expect("root frame always present")
                        .push(ExportNode::Statement((*src).to_string()));
                }
                Item::Open { resource, expr_src } => {
                    meta_stack.push(((*resource).to_string(), (*expr_src).to_string()));
                    children_stack.push(Vec::new());
                }
                Item::Close => {
                    let children = children_stack.pop().expect("close without open frame");
                    let (resource, expr_src) =
                        meta_stack.pop().expect("close without matching meta");
                    if !children.is_empty() {
                        children_stack
                            .last_mut()
                            .expect("parent frame")
                            .push(ExportNode::Block {
                                resource,
                                expr_src,
                                children,
                            });
                    }
                }
            }
        }
        // Implicitly close any unbalanced opens so the export is well-formed.
        while !meta_stack.is_empty() {
            let children = children_stack.pop().expect("unbalanced opens leave frames");
            let (resource, expr_src) = meta_stack.pop().expect("unbalanced opens leave meta");
            if !children.is_empty() {
                children_stack
                    .last_mut()
                    .expect("parent frame")
                    .push(ExportNode::Block {
                        resource,
                        expr_src,
                        children,
                    });
            }
        }

        let root = children_stack
            .into_iter()
            .next()
            .expect("root frame always present");
        render_export_nodes(&root, 1)
    }

    /// Build a synthetic single-file program and look up `main`'s
    /// inferred effect set, rendered as `writes(A, B) reads(C)` etc.
    /// Returns `""` when no effects fired (a pure session) or when the
    /// synthesized source fails to parse (defensive — `render_exported_session`
    /// still emits the wrapper, the build pass surfaces the real error).
    fn compute_main_effect_decls(&self, items_section: &str, main_body: &str) -> String {
        let mut synth = String::new();
        if !items_section.is_empty() {
            synth.push_str(items_section);
            synth.push_str("\n\n");
        }
        synth.push_str("fn main() {\n");
        synth.push_str(main_body);
        synth.push_str("}\n");
        let parsed = crate::parse(&synth);
        if !parsed.errors.is_empty() {
            return String::new();
        }
        let effects = crate::effectcheck(&parsed.program);
        let Some(set) = effects.inferred_effects.get("main") else {
            return String::new();
        };
        render_effect_decls(set)
    }

    /// Handle a `:dep <name> = <spec>` line. Parses the right-hand side
    /// against the same shape `[dependencies]` accepts in `kara.toml` —
    /// either a bare semver string (`http = "1.2"`) or an inline table
    /// (`{ git = "..." }` / `{ path = "..." }` / `{ version = "..." }`).
    /// Records the request in `pending_deps` and prints a confirmation
    /// plus a v1.1 caveat: the package manager is not yet wired, so the
    /// surface symbols are NOT yet in scope. When the resolver lands the
    /// stored map is what it consumes.
    fn add_dep(&mut self, rest: &str) {
        // Wrap the user's `name = value` pair as a one-table TOML snippet
        // and lean on `toml::Table` to do the heavy lifting (string
        // escaping, inline-table parsing, etc.). The synthetic table name
        // is unreachable from user input.
        let snippet = format!("[__kara_repl_dep__]\n{rest}\n");
        let table: toml::Table = match snippet.parse() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: invalid `:dep` syntax — {}", e.message());
                eprintln!(
                    "  expected: :dep <name> = \"version\"  or  :dep <name> = {{ git = \"...\" }}"
                );
                return;
            }
        };
        let inner = match table.get("__kara_repl_dep__").and_then(|v| v.as_table()) {
            Some(t) => t,
            None => {
                eprintln!("error: `:dep` argument must be a single `name = value` pair");
                return;
            }
        };
        if inner.len() != 1 {
            eprintln!(
                "error: `:dep` accepts exactly one dependency per command (got {})",
                inner.len()
            );
            return;
        }
        let (name, value) = inner.iter().next().expect("len == 1 just checked");
        if name.is_empty() {
            eprintln!("error: `:dep` package name cannot be empty");
            return;
        }
        // For inline-table forms (`{ git = "..." }`) reuse the user's
        // original RHS slice verbatim — `toml::Value` doesn't impl Display
        // and pulling in the toml serializer just to round-trip one value
        // would be excessive. For bare strings, normalize through a tiny
        // TOML escaper so the stored form is independent of the user's
        // whitespace / quote choice.
        let normalized = match value {
            toml::Value::String(s) => format!("\"{}\"", toml_escape_string(s)),
            toml::Value::Table(_) => match rest.split_once('=') {
                Some((_, rhs)) => rhs.trim().to_string(),
                None => {
                    eprintln!("error: `:dep` syntax must include `=` between name and value");
                    return;
                }
            },
            other => {
                eprintln!(
                    "error: `:dep {name} = ...` must be a version string or an inline table (got {})",
                    other.type_str()
                );
                return;
            }
        };
        let updated = self.pending_deps.insert(name.clone(), normalized.clone());
        let verb = if updated.is_some() {
            "updated"
        } else {
            "registered"
        };
        println!("(:dep {verb}: {name} = {normalized})");
        println!(
            "  note: package resolution / download lands in v1.1 — the entry is recorded but `{name}`'s symbols are not yet in scope."
        );
    }

    /// Open a new cross-cell `:provide R = expr` scope. The construction
    /// expression is eagerly evaluated inside the current cell — if any
    /// pipeline stage (parse / resolve / type / ownership) errors or
    /// the interpreter records a runtime panic, the frame is NOT pushed
    /// and the failure surfaces in the cell output (design.md §
    /// Cross-Cell Providers: "if construction panics, the scope is not
    /// opened and the panic surfaces in the cell output").
    ///
    /// Construction runs inside the active outer providers' wraps so
    /// nested forms (e.g. `:provide B = use_A()` while `A` is already
    /// provided) see the outer provider stack the same way subsequent
    /// cells will.
    fn add_provider(&mut self, rest: &str) -> Result<String, String> {
        let (resource, expr_src) = parse_provide_form(rest)?;
        // Nested same-resource :provides are legal (mirroring nested
        // `with_provider[R]` in file code) but easy to misuse. Prepend a
        // hint to the result without rejecting — :end-provide will close
        // the innermost frame, which is what the user almost certainly
        // wants.
        let mut prefix = String::new();
        if let Some(existing) = self.provider_stack.iter().find(|f| f.resource == resource) {
            prefix.push_str(&format!(
                "(note: `:provide {resource}` shadows an outer `:provide {resource}` opened in cell {}; the innermost scope is what :end-provide will close.)\n",
                existing.opened_cell
            ));
        }
        match self.try_construct_provider(&resource, &expr_src) {
            Ok(()) => {
                let opened_cell = self.cell_history.len() + 1;
                self.provider_history.push(ProviderHistoryEntry {
                    cells_seen: self.cell_history.len(),
                    kind: ProviderHistoryKind::Open {
                        resource: resource.clone(),
                        expr_src: expr_src.clone(),
                    },
                });
                self.provider_stack.push(ProviderFrame {
                    resource: resource.clone(),
                    expr_src,
                    opened_cell,
                });
                Ok(format!(
                    "{prefix}(provider scope `:provide {resource}` opened)"
                ))
            }
            Err(msg) => Err(format!(
                "{prefix}{msg}\n       (`:provide {resource}` was not opened.)"
            )),
        }
    }

    /// Close the innermost cross-cell `:provide` scope. LIFO order is
    /// enforced: closing `:provide A` while `:provide B` is the
    /// innermost frame surfaces a structured error naming the frame
    /// that would have to close first (design.md § Cross-Cell
    /// Providers).
    fn end_provider(&mut self, rest: &str) -> Result<String, String> {
        let resource = rest.trim();
        if resource.is_empty() {
            return Err("usage: :end-provide <Resource>".to_string());
        }
        if !is_valid_resource_ident(resource) {
            return Err(format!(
                "error: `:end-provide {resource}` — resource name must be a Kāra identifier"
            ));
        }
        let Some(top) = self.provider_stack.last() else {
            return Err(format!(
                "error: `:end-provide {resource}` with no active provider scope"
            ));
        };
        if top.resource != resource {
            return Err(format!(
                "error: `:end-provide {resource}` attempts to close an outer scope while `:provide {}` is still active; close {} first",
                top.resource, top.resource
            ));
        }
        // Snapshot the active provider stack BEFORE popping the
        // closing frame — every persistent-let captured with this exact
        // stack snapshot was declared inside the now-closing scope and
        // must drop from the replay buffer (design.md § Cross-Cell
        // Providers's "bindings declared inside `:provide R` are
        // visible only within that scope" rule). Lets captured under a
        // strict prefix of this stack (i.e. before the closing scope
        // opened) stay — they belong to an outer block.
        let active_at_close: Vec<String> = self
            .provider_stack
            .iter()
            .map(|f| f.resource.clone())
            .collect();
        let frame = self
            .provider_stack
            .pop()
            .expect("just inspected the top frame above");
        self.provider_history.push(ProviderHistoryEntry {
            cells_seen: self.cell_history.len(),
            kind: ProviderHistoryKind::Close {
                resource: frame.resource.clone(),
            },
        });

        // Prune persistent_lets whose capture-time scope equals the
        // just-closed-scope's pre-pop stack. Each pruned binding seeds
        // a `pruned_provider_lets` entry so the diagnostic-rendering
        // layer can attach the notebook-aware tail on a future
        // "undefined name 'X'" error.
        let pruned_at_cell = self.cell_history.len() + 1;
        let mut kept_lets: Vec<String> = Vec::with_capacity(self.persistent_lets.len());
        let mut kept_origin: Vec<usize> = Vec::with_capacity(self.persistent_let_origin.len());
        let mut kept_scope: Vec<Vec<String>> =
            Vec::with_capacity(self.persistent_let_provider_scope.len());
        for (i, scope) in self.persistent_let_provider_scope.iter().enumerate() {
            if scope == &active_at_close {
                let let_src = &self.persistent_lets[i];
                let declared_in_cell = *self.persistent_let_origin.get(i).unwrap_or(&0);
                for binding in parse_let_binding_names(let_src) {
                    self.let_snapshots.remove(&binding);
                    self.pruned_provider_lets.push(PrunedProviderLet {
                        binding_name: binding,
                        declared_in_cell,
                        pruned_by_resource: frame.resource.clone(),
                        scope_opened_cell: frame.opened_cell,
                        pruned_at_cell,
                    });
                }
            } else {
                kept_lets.push(self.persistent_lets[i].clone());
                kept_origin.push(*self.persistent_let_origin.get(i).unwrap_or(&0));
                kept_scope.push(scope.clone());
            }
        }
        self.persistent_lets = kept_lets;
        self.persistent_let_origin = kept_origin;
        self.persistent_let_provider_scope = kept_scope;

        Ok(format!(
            "(provider scope `:provide {}` from cell {} closed)",
            frame.resource, frame.opened_cell
        ))
    }

    /// Eagerly evaluate the construction expression for `:provide R =
    /// expr` so a panicking constructor leaves the scope un-opened.
    /// Runs the full parse → resolve → typecheck → ownership →
    /// interpreter pipeline against a synthetic program built from the
    /// current session view, with the expression wrapped in any
    /// already-active provider scopes (so nested `:provide` forms that
    /// depend on outer providers validate correctly). Returns
    /// `Err(message)` on any pipeline failure or interpreter-recorded
    /// runtime error; `Ok(())` indicates a clean construction. This
    /// method intentionally does NOT mutate session state — no
    /// `persistent_lets` capture, no `cell_byte_ranges` update — so
    /// the construction check is a pure validation step.
    fn try_construct_provider(&self, resource: &str, expr_src: &str) -> Result<(), String> {
        let inner = format!("let __karac_provide_{resource}__ = {expr_src};\n");
        let body = self.wrap_in_active_providers(&inner);

        let mut synth = String::new();
        if !self.items_source.trim().is_empty() {
            synth.push_str(&strip_main(&self.items_source));
            if !synth.ends_with('\n') {
                synth.push('\n');
            }
        }
        synth.push_str("fn main() {\n");
        for prior_let in &self.persistent_lets {
            synth.push_str(prior_let);
            if !prior_let.ends_with('\n') {
                synth.push('\n');
            }
        }
        synth.push_str(&body);
        if !body.ends_with('\n') {
            synth.push('\n');
        }
        synth.push_str("}\n");

        let mut parsed = crate::parse(&synth);
        if !parsed.errors.is_empty() {
            return Err(format!(
                "error: parse error in `:provide {resource}` construction: {}",
                parsed.errors[0].message
            ));
        }
        let resolved = crate::resolve(&parsed.program);
        if !resolved.errors.is_empty() {
            return Err(format!(
                "error: resolve error in `:provide {resource}` construction: {}",
                resolved.errors[0].message
            ));
        }
        let typed = crate::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            return Err(format!(
                "error: type error in `:provide {resource}` construction: {}",
                typed.errors[0].message
            ));
        }
        let owned = crate::ownershipcheck(&parsed.program, &typed);
        if !owned.errors.is_empty() {
            return Err(format!(
                "error: ownership error in `:provide {resource}` construction: {}",
                owned.errors[0].message
            ));
        }
        crate::lower(&mut parsed.program, &typed);
        let mut interp = crate::interpreter::Interpreter::new(&parsed.program, &typed);
        interp.run();
        if let Some(err) = interp.runtime_errors.first() {
            return Err(format!(
                "error: panic in `:provide {resource}` construction: {}",
                err.message
            ));
        }
        Ok(())
    }

    /// Wrap a cell-body source fragment in the active provider scopes,
    /// innermost-first so the rendered nesting matches stack push order
    /// (outermost on the outside). Returns the input unchanged when
    /// `provider_stack` is empty. Slice 3 uses this for every
    /// subsequent statement-cell run; slice 2 uses it in
    /// `try_construct_provider` to validate the nested-`:provide` case.
    pub(crate) fn wrap_in_active_providers(&self, body: &str) -> String {
        if self.provider_stack.is_empty() {
            return body.to_string();
        }
        let mut wrapped = body.to_string();
        for frame in self.provider_stack.iter().rev() {
            wrapped = format!(
                "with_provider[{}]({}, || {{\n{}\n}});",
                frame.resource, frame.expr_src, wrapped
            );
        }
        wrapped
    }
}
