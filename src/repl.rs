//! Line-based REPL over the MIR interpreter.
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
//! Bindings across cells (v2):
//! - A cell's program ends by recording each top-level binding it leaves
//!   (`crate::mir::interp::REPL_SNAPSHOT_FN`), and the next cell starts from
//!   those values written as source, so no earlier cell runs again
//!   (`Session::v2_values`). A value with no source form (a function, a
//!   `shared` value, a handle) makes the next cell replay the earlier cells
//!   instead, dropping what the replay prints.
//! - A binding a cell moved away is left out; a later use of it is refused
//!   naming the cell that moved it, unless `--auto-clone` /
//!   `%set auto-clone on` clones it there (`Session::auto_clone_moves`).
//! - `:provide R = expr` runs later cells under a provider; they start from
//!   the values the scope opened with, and their `let`s replay inside it.
//! - `:reset` clears the bindings.
//! - `%show <expr>` renders a value as `text/plain` (and `text/html` for a
//!   list of structs) from the same source form (`display::render_source`).

use std::collections::BTreeMap;
use std::ops::Range;

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

mod display;
mod util;

pub use display::DisplayBundle;
use util::*;

/// A top-level binding a v2 cell ended with, written as source
/// (`Session::v2_values`).
#[derive(Clone, Debug)]
struct V2Value {
    name: String,
    mutable: bool,
    ty: String,
    value: String,
}

/// Whether the `let` statement `slice` binds `name` as `mut`.
fn binds_mut(slice: &str, name: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut rest = slice;
    while let Some(at) = rest.find("mut") {
        let before = rest[..at].chars().next_back();
        let after = rest[at + 3..].trim_start();
        if !before.is_some_and(ident)
            && after.starts_with(name)
            && !after[name.len()..].starts_with(ident)
        {
            return true;
        }
        rest = &rest[at + 3..];
    }
    false
}

/// The values `live` (the bindings a cell ended with) take from what its
/// program recorded, or `None` when one is missing or cannot be written
/// as source.
fn v2_values_of(
    live: &[(String, bool)],
    got: &[crate::mir::interp::ReplBinding],
) -> Option<Vec<V2Value>> {
    if got.len() != live.len() {
        return None;
    }
    live.iter()
        .zip(got)
        .map(|((name, mutable), (got_name, source))| {
            let (ty, value) = source.clone()?;
            (got_name == name).then(|| V2Value {
                name: name.clone(),
                mutable: *mutable,
                ty,
                value,
            })
        })
        .collect()
}

/// The bindings whose recording in `synthetic` the errors `errs` are all
/// at (a binding the cell moved away), or none when any error is
/// elsewhere.
fn moved_at_snapshot(synthetic: &str, errs: &[String]) -> Vec<String> {
    let at = regex::Regex::new(r"<repl>:(\d+):").expect("valid regex");
    let call = format!("{}(\"", crate::mir::interp::REPL_SNAPSHOT_FN);
    let src_lines: Vec<&str> = synthetic.lines().collect();
    let mut names = Vec::new();
    for e in errs {
        let line = at
            .captures(e)
            .and_then(|c| c[1].parse::<usize>().ok())
            .and_then(|l| src_lines.get(l.wrapping_sub(1)));
        let name = line
            .and_then(|l| l.strip_prefix(call.as_str()))
            .and_then(|rest| rest.split('"').next());
        match name {
            Some(n) => names.push(n.to_string()),
            None => return Vec::new(),
        }
    }
    names
}

/// The place a `%show` expression's source takes in
/// `Session::cell_byte_ranges`, in place of a cell's number.
const SHOW_CELL: usize = usize::MAX;

/// A v2 statement cell as a run of statements: a cell may end in an
/// expression without its `;` (`println(x)`), which a later cell's replay
/// would run into. The `;` goes on a line of its own, past any trailing
/// comment, and only where it makes the cell parse.
fn terminated(cell_src: &str) -> String {
    let src = cell_src.trim_end();
    let with = format!("{src}\n;");
    if crate::parse(src).errors.is_empty() || !crate::parse(&with).errors.is_empty() {
        src.to_string()
    } else {
        with
    }
}

/// Caller-supplied options for `run_with_options` — mirrors the flags
/// surfaced on the `karac repl` subcommand. `Default` matches the
/// historical bare-`karac repl` behavior so existing callers keep working.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReplOptions {
    /// `--auto-clone`: a binding an earlier cell moved away and a later
    /// cell uses is cloned where it moved, with a `perf[auto-clone-in-repl]`
    /// note (`Session::auto_clone_moves`).
    pub auto_clone: bool,
    /// `--interp`: accepted for compatibility; v2 cells always run on the
    /// MIR interpreter.
    pub interp: bool,
}

/// Run the interactive REPL until EOF or `:quit`. Returns once the user
/// exits; never reaches the rest of the CLI dispatch.
pub fn run() {
    run_with_options(ReplOptions::default());
}

/// Surface for the binary entry point: launch the REPL with caller-supplied
/// options. Used by `karac repl` flag wiring (`--auto-clone`).
pub fn run_with_options(opts: ReplOptions) {
    let mut session = Session::with_options(opts);
    let mut tags: Vec<&str> = Vec::new();
    if opts.auto_clone {
        tags.push("auto-clone on");
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
/// session because its originating `:provide` scope closed. The
/// notebook-aware tail it used to drive on a later `undefined name`
/// error rendered through the legacy cell path, which M3 removed.
#[derive(Debug, Clone)]
struct PrunedProviderLet {
    binding_name: String,
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
    /// Compiler-surface notes that aren't fatal. Two kinds today: the
    /// `perf[auto-clone-in-repl]` lines emitted when `--auto-clone` rewrites a
    /// consume site, and the `warning[repl-jit-no-snapshot]` line emitted when
    /// a cell declares a binding the JIT lane cannot carry across cells
    /// (B-2026-08-30-7). Always non-empty when either fired — the spec
    /// requires the channel be visible, never silent. Production
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
    /// v2: the top-level bindings the last cell ended with, as source
    /// (B-2026-10-23-6). The next cell starts from these instead of
    /// replaying `persistent_lets`, so the cells that built them do not
    /// run again. `None` when some value could not be written as source,
    /// or a provider scope ran: the next cell replays.
    v2_values: Option<Vec<V2Value>>,
    /// What the running cell's [`Session::v2_values`] become if it is
    /// accepted.
    v2_pending: Option<Vec<V2Value>>,
    /// The values the running cell started from, when it moved a binding
    /// away: kept, if it is accepted, in [`Session::v2_move_starts`].
    v2_pending_move_start: Option<Vec<V2Value>>,
    /// For each cell that moved a binding away, the values it started from.
    /// Auto-clone rewrites such a cell, and the cells from it on run again
    /// from these ([`Session::v2_replay_from`]).
    v2_move_starts: BTreeMap<usize, Vec<V2Value>>,
    /// While [`Session::v2_values`] is `None` after an auto-clone rewrite:
    /// the first rewritten cell and the values it started from. A program
    /// starts from these and replays only the cells from that one on.
    v2_replay_from: Option<(usize, Vec<V2Value>)>,
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
            v2_values: Some(Vec::new()),
            v2_pending: None,
            v2_pending_move_start: None,
            v2_move_starts: BTreeMap::new(),
            v2_replay_from: None,
            persistent_let_origin: Vec::new(),
            cell_byte_ranges: Vec::new(),
            auto_clone: false,
            pending_deps: BTreeMap::new(),
            provider_stack: Vec::new(),
            provider_history: Vec::new(),
            persistent_let_provider_scope: Vec::new(),
            pruned_provider_lets: Vec::new(),
            cell_effect_history: Vec::new(),
        }
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
    /// `Err` string shapes. `%show`, the rich-display surface, is not
    /// supported on v2 yet. `%rc` (line 785) lists
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
        let key = rest
            .split_once(char::is_whitespace)
            .map_or(rest, |(k, _)| k)
            .trim();
        let value = rest[key.len()..].trim();
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
                _ => MagicOutput::error("usage: %set auto-clone on|off"),
            },
            "" => MagicOutput::error("usage: %set <key> <value>"),
            other => {
                MagicOutput::error(format!("unknown setting `{other}`. Supported: auto-clone."))
            }
        }
    }

    /// `%show <expr>` — rich-display the given expression: `text/plain`
    /// always, `text/html` for a list of structs of one type
    /// ([`display::render_source`]). The expression runs as a cell would,
    /// from the values the cells left and under the open providers, and its
    /// value is recorded as a cell's bindings are; the session is not
    /// changed. What it prints comes before the value.
    fn magic_show(&mut self, rest: &str) -> MagicOutput {
        if rest.is_empty() {
            return MagicOutput::error("usage: %show <expression>");
        }
        let snap_fn = crate::mir::interp::REPL_SNAPSHOT_FN;
        let mut s = strip_main(&self.items_source);
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&format!("fn {snap_fn}[T](name: Str, value: T) {{}}\n"));
        s.push_str("fn main() {\n");
        let mut ranges = Vec::new();
        self.push_cell_start(&mut s, &mut ranges, true);
        let head = "let __karac_show = (\n";
        let body = format!("{head}{rest}\n);\n{snap_fn}(\"__karac_show\", __karac_show);");
        let at = self.push_in_scopes(&mut s, &mut ranges, &body, true).start + head.len();
        ranges.push((SHOW_CELL, at..at + rest.len()));
        s.push_str("}\n");
        let saved = std::mem::replace(&mut self.cell_byte_ranges, ranges);
        let r = crate::cli::repl_v2::run_cell(&s);
        let relabel = |me: &Self, text: &str| me.relabel_v2(&s, text);
        let out = match r {
            Ok(ran) => {
                let shown = ran
                    .bindings
                    .first()
                    .and_then(|(_, v)| v.as_ref())
                    .and_then(|(_, source)| display::render_source(source));
                match shown {
                    Some(bundle) => {
                        let value = bundle.get("text/plain").unwrap_or_default();
                        let text = format!("{}{value}", ran.stdout);
                        let mut rich = DisplayBundle::new().with("text/plain", text.clone());
                        rich.mimes.extend(bundle.mimes.into_iter().skip(1));
                        MagicOutput::ok_rich(text, rich)
                    }
                    None => MagicOutput::error(format!(
                        "{}`%show` cannot display this value: it has no written form (a function, a `shared` value, a handle)",
                        ran.stdout
                    )),
                }
            }
            Err(f) => {
                let errors: Vec<String> = f.errors.iter().map(|e| relabel(self, e)).collect();
                MagicOutput::error(format!("{}{}", f.stdout, errors.join("\n")))
            }
        };
        self.cell_byte_ranges = saved;
        out
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
    /// and `persistent_lets` are untouched. Surfaces
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
        let _ = opts.interp;
        s
    }

    /// Inspector for the `--auto-clone` flag. Used by integration tests.
    pub fn auto_clone(&self) -> bool {
        self.auto_clone
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

        if self.classify(trimmed) == CellShape::PureItems {
            for e in self.add_items_v2(trimmed) {
                eprintln!("{e}");
            }
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

    /// Run a wrapper cell on v2, optionally capturing its stdout. The
    /// returned `WrapperOutcome` carries either captured stdout lines (`Ok`
    /// shape) or diagnostic strings (`Err` shape).
    ///
    /// With auto-clone on, a cell refused only because it uses bindings an
    /// earlier cell moved away runs again after [`Session::auto_clone_moves`]
    /// clones them where they moved, with a note for each.
    fn run_with_wrapper_inner(
        &mut self,
        cell_src: &str,
        capture: bool,
    ) -> Result<WrapperOutcome, WrapperOutcome> {
        let mut notes = Vec::new();
        let mut r = self.run_cell_v2(&terminated(cell_src), capture);
        // Each round clones at least one more binding, so a bound on the
        // rounds only stops a cell whose errors keep naming new moves.
        for _ in 0..8 {
            let Err(out) = &r else { break };
            if !self.auto_clone {
                break;
            }
            match self.auto_clone_moves(&out.errors) {
                Some(more) => notes.extend(more),
                None => break,
            }
            r = self.run_cell_v2(&terminated(cell_src), capture);
        }
        match &mut r {
            Ok(out) | Err(out) => out.notes.splice(0..0, notes),
        };
        r
    }

    /// Auto-clone: when every error in `errors` is a use of a binding an
    /// earlier cell moved away, rewrites each moving cell to clone the
    /// binding where it moved (in `cell_history`, for `:save`, and in its
    /// replayed source), and returns one `perf[auto-clone-in-repl]` note
    /// per clone. The next cell starts from a replay of the rewritten
    /// cells. `None`, changing nothing, when any error is something else
    /// or a move cannot be rewritten (a move inside a provider scope's
    /// cell, whose `let`s replay apart).
    fn auto_clone_moves(&mut self, errors: &[String]) -> Option<Vec<String>> {
        let moved = regex::Regex::new(
            r"value '(\w+)' moved here, used again here \(moved at cell (\d+):(\d+):(\d+)\)",
        )
        .expect("valid regex");
        let current = self.cell_history.len();
        let mut edits: BTreeMap<usize, Vec<(usize, String)>> = BTreeMap::new();
        for e in errors {
            let c = moved.captures(e)?;
            let name = c[1].to_string();
            let [cell, line, col] = [&c[2], &c[3], &c[4]].map(|n| n.parse::<usize>().ok());
            let (cell, line, col) = (cell?, line?, col?);
            if cell == 0 || cell >= current {
                return None;
            }
            let src = &self.cell_history[cell - 1];
            let line_start = if line == 1 {
                0
            } else {
                src.match_indices('\n').nth(line - 2)?.0 + 1
            };
            let at = src[line_start..]
                .char_indices()
                .nth(col.checked_sub(1)?)
                .map(|(i, _)| line_start + i)?;
            let ident = |ch: char| ch.is_alphanumeric() || ch == '_';
            let end = at + name.len();
            if !src[at..].starts_with(&name) || src[end..].starts_with(ident) {
                return None;
            }
            let at_cell = edits.entry(cell).or_default();
            if !at_cell.iter().any(|(p, _)| *p == end) {
                at_cell.push((end, name));
            }
        }
        if edits.is_empty() {
            return None;
        }
        let mut rewritten = Vec::new();
        for (&cell, at_cell) in &edits {
            let old = &self.cell_history[cell - 1];
            let slot = (0..self.persistent_lets.len()).find(|&i| {
                self.persistent_let_origin.get(i) == Some(&cell)
                    && self.persistent_lets[i] == terminated(old)
            })?;
            let mut new = old.clone();
            let mut at_cell = at_cell.clone();
            at_cell.sort_by(|a, b| b.0.cmp(&a.0));
            for (end, _) in &at_cell {
                new.insert_str(*end, ".clone()");
            }
            rewritten.push((cell, slot, new));
        }
        let mut notes = Vec::new();
        for (cell, slot, new) in rewritten {
            self.persistent_lets[slot] = terminated(&new);
            self.cell_history[cell - 1] = new;
            let mut names: Vec<&String> = edits[&cell].iter().map(|(_, n)| n).collect();
            names.sort();
            for name in names {
                notes.push(format!(
                    "perf[auto-clone-in-repl]: inserted `.clone()` on `{name}` at consume site (cell {cell}, used in cell {current}); cross-cell binding kept alive — `%set auto-clone off` surfaces the underlying use-after-move diagnostic instead"
                ));
            }
        }
        // The values the cells left were taken with the bindings moved: the
        // cells from the first rewritten one on run again, from the values
        // it started from (all of them, when those were not kept).
        let first = *edits.keys().next().expect("an edit");
        self.v2_replay_from = match (&self.v2_values, self.v2_replay_from.take()) {
            (None, Some((k, vs))) if k <= first => Some((k, vs)),
            (None, None) => None,
            _ => self
                .v2_move_starts
                .get(&first)
                .map(|vs| (first, vs.clone())),
        };
        self.v2_values = None;
        Some(notes)
    }

    /// Test helper: evaluate a cell and return any captured output and
    /// surfaced errors. Mirrors `evaluate_cell` but routes the
    /// interpreter's stdout into an in-memory buffer instead of the
    /// process's stdout.
    pub fn evaluate_cell_captured(&mut self, src: &str) -> EvaluatedCell {
        let trimmed = src.trim_end();
        self.cell_history.push(trimmed.to_string());

        if self.classify(trimmed) == CellShape::PureItems {
            return EvaluatedCell {
                errors: self.add_items_v2(trimmed),
                ..EvaluatedCell::default()
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
                    // What a v2 cell printed before it panicked; empty for
                    // a diagnostic.
                    stdout: out.stdout.join(""),
                    errors: out.errors,
                    notes: out.notes,
                    effect_footer: String::new(),
                }
            }
        }
    }

    /// The cell's shape. On v2 a statement missing its final `;` (`n = 5`)
    /// is a statement, as it is when it ends a cell that parses.
    fn classify(&self, src: &str) -> CellShape {
        let shape = classify_input(src);
        if shape == CellShape::PureItems && crate::parse(&terminated(src)).had_top_level_statements
        {
            return CellShape::Statements;
        }
        shape
    }

    /// An item cell on v2 (its entry already in `cell_history`): the items
    /// join the session when the v2 check accepts them, and otherwise the
    /// cell is rolled back whole, so one bad item does not fail every later
    /// cell. Returns the check's errors.
    fn add_items_v2(&mut self, src: &str) -> Vec<String> {
        let before = self.items_source.clone();
        self.append_items(src);
        let (checked, errs) = self.check_items_v2();
        // Places in the new cell are named by its line, as a statement
        // cell's are.
        let errs: Vec<String> = match checked.rfind(src) {
            Some(at) => {
                let saved = std::mem::replace(
                    &mut self.cell_byte_ranges,
                    vec![(self.cell_history.len(), at..at + src.len())],
                );
                let errs = errs.iter().map(|e| self.relabel_v2(&checked, e)).collect();
                self.cell_byte_ranges = saved;
                errs
            }
            None => errs,
        };
        if errs.is_empty() {
            self.cell_effect_history.push(CellEffectSnapshot::default());
        } else {
            self.items_source = before;
            self.cell_history.pop();
        }
        errs
    }

    /// The v2 check of the accumulated items, under an empty
    /// `main` (a user's own `main` is replaced, as a cell's program does).
    /// Returns the source checked with the errors.
    fn check_items_v2(&self) -> (String, Vec<String>) {
        let mut src = strip_main(&self.items_source);
        if !src.is_empty() && !src.ends_with('\n') {
            src.push('\n');
        }
        src.push_str("fn main() {}\n");
        let errs = crate::cli::repl_v2::check(&src).err().unwrap_or_default();
        (src, errs)
    }

    /// A statement cell on v2: its program checked and run by
    /// `crate::cli::repl_v2`. Printed directly unless `capture`.
    fn run_cell_v2(
        &mut self,
        cell_src: &str,
        capture: bool,
    ) -> Result<WrapperOutcome, WrapperOutcome> {
        use std::io::Write;
        let lines =
            |s: &str| -> Vec<String> { s.split_inclusive('\n').map(|l| l.to_string()).collect() };
        self.v2_pending = None;
        self.v2_pending_move_start = None;
        let mut run = None;
        // The cell's program with its bindings recorded at its end, unless
        // a provider scope is open. A binding the cell moved away cannot be
        // recorded: it is left out, as it is out of scope. When the
        // program is refused for anything else, the cell runs as a replay
        // of the cells before it, whose diagnostics name the cells
        // (a use of a moved binding names the cell that moved it).
        if self.provider_stack.is_empty() {
            let mut live = self.v2_live_bindings(cell_src);
            let mut moved_any = false;
            for _ in 0..2 {
                let synthetic = self.build_synthetic_cell(cell_src, Some(&live));
                if !crate::parse(&synthetic).errors.is_empty() {
                    break;
                }
                match crate::cli::repl_v2::check(&synthetic) {
                    Ok(()) => {
                        let r = crate::cli::repl_v2::run_checked(&synthetic);
                        if let Ok(ran) = &r {
                            self.v2_pending = v2_values_of(&live, &ran.bindings);
                            if moved_any {
                                self.v2_pending_move_start = self.v2_values.clone();
                            }
                        }
                        run = Some((synthetic, r));
                        break;
                    }
                    Err(errs) => {
                        let moved = moved_at_snapshot(&synthetic, &errs);
                        if moved.is_empty() {
                            break;
                        }
                        live.retain(|(n, _)| !moved.contains(n));
                        moved_any = true;
                    }
                }
            }
        }
        let (synthetic, r) = match run {
            Some(run) => run,
            None => {
                let synthetic = self.build_synthetic_cell(cell_src, None);
                let r = crate::cli::repl_v2::run_cell(&synthetic);
                (synthetic, r)
            }
        };
        match r {
            Ok(mut ran) => {
                ran.stderr = self.relabel_v2(&synthetic, &ran.stderr);
                if capture {
                    return Ok(WrapperOutcome {
                        stdout: lines(&ran.stdout),
                        errors: Vec::new(),
                        notes: Vec::new(),
                    });
                }
                print!("{}", ran.stdout);
                let _ = std::io::stdout().flush();
                eprint!("{}", ran.stderr);
                Ok(WrapperOutcome::errors(Vec::new(), Vec::new()))
            }
            Err(f) => {
                let errors = f
                    .errors
                    .iter()
                    .map(|e| self.relabel_v2(&synthetic, e))
                    .collect();
                let mut out = WrapperOutcome::errors(errors, Vec::new());
                if capture {
                    out.stdout = lines(&f.stdout);
                } else {
                    print!("{}", f.stdout);
                    let _ = std::io::stdout().flush();
                }
                Err(out)
            }
        }
    }

    /// `text` (a diagnostic, a panic's trace) with each `<repl>:LINE:COL`
    /// place in `synthetic` that falls in a cell named as that cell's line,
    /// `cell N:LINE:COL`, and a snippet's gutter numbered to match. Places in
    /// the accumulated items keep the program's numbering.
    fn relabel_v2(&self, synthetic: &str, text: &str) -> String {
        let line_of = |at: usize| synthetic[..at.min(synthetic.len())].matches('\n').count() + 1;
        let cells: Vec<(usize, usize, usize)> = self
            .cell_byte_ranges
            .iter()
            .filter(|(_, r)| !r.is_empty())
            .map(|(cell, r)| (*cell, line_of(r.start), line_of(r.end - 1)))
            .collect();
        let place = |line: usize| {
            cells
                .iter()
                .find(|(_, first, last)| (*first..=*last).contains(&line))
                .map(|(cell, first, _)| (*cell, line - first + 1))
        };
        // `<repl>:L:C` in a header or trace, `line L:C` in a message's text.
        let at = regex::Regex::new(r"<repl>:(\d+):(\d+)|\bline (\d+):(\d+)").expect("valid regex");
        let gutter = regex::Regex::new(r"(?m)^( *)(\d+) \|").expect("valid regex");
        let mut out = String::new();
        for block in text.split_inclusive('\n') {
            out.push_str(&at.replace_all(block, |c: &regex::Captures| {
                let (line, col) = match (c.get(1), c.get(2)) {
                    (Some(l), Some(col)) => (l.as_str(), col.as_str()),
                    _ => (&c[3], &c[4]),
                };
                match place(line.parse().unwrap_or(0)) {
                    Some((SHOW_CELL, l)) => format!("%show:{l}:{col}"),
                    Some((cell, l)) => format!("cell {cell}:{l}:{col}"),
                    None => c[0].to_string(),
                }
            }));
        }
        gutter
            .replace_all(&out, |c: &regex::Captures| {
                let line: usize = c[2].parse().unwrap_or(0);
                let width = c[1].len() + c[2].len();
                match place(line) {
                    Some((_, l)) => format!("{l:>width$} |"),
                    None => c[0].to_string(),
                }
            })
            .into_owned()
    }

    /// The top-level bindings in scope at the end of `cell_src` (v2), each
    /// with whether it is `mut`, in the order they were first bound.
    fn v2_live_bindings(&self, cell_src: &str) -> Vec<(String, bool)> {
        fn bind(live: &mut Vec<(String, bool)>, l: &CellLet) {
            let mut names: Vec<&String> = l.names.iter().collect();
            names.sort();
            for n in names {
                let m = binds_mut(&l.slice, n);
                match live.iter_mut().find(|(x, _)| x == n) {
                    Some(e) => e.1 = m,
                    None => live.push((n.clone(), m)),
                }
            }
        }
        let mut live: Vec<(String, bool)> = Vec::new();
        let (from, values) = match (&self.v2_values, &self.v2_replay_from) {
            (Some(vs), _) => (usize::MAX, &vs[..]),
            (None, Some((k, vs))) => (*k, &vs[..]),
            (None, None) => (0, &[][..]),
        };
        live.extend(values.iter().map(|v| (v.name.clone(), v.mutable)));
        for (i, prior) in self.persistent_lets.iter().enumerate() {
            if self.persistent_let_origin.get(i).copied().unwrap_or(0) >= from {
                for l in scan_top_level_lets(prior) {
                    bind(&mut live, &l);
                }
            }
        }
        for l in scan_top_level_lets(cell_src) {
            bind(&mut live, &l);
        }
        live
    }

    /// The bindings a program in the session starts from, written into
    /// `s`: with `values`, the values the last cell outside a provider
    /// ended with ([`Session::v2_values`]), so no earlier cell runs again;
    /// otherwise, or when some value has no source form, a replay of the
    /// earlier cells. The `let`s of the open provider scopes' cells replay
    /// inside the scopes ([`Session::push_in_scopes`]). Each replayed
    /// slice's place goes in `ranges`, with its cell.
    fn push_cell_start(
        &self,
        s: &mut String,
        ranges: &mut Vec<(usize, Range<usize>)>,
        values: bool,
    ) {
        // The cells from `from` on replay after the values (all of them
        // when there are none).
        let (from, values) = match (&self.v2_values, &self.v2_replay_from) {
            _ if !values => (0, None),
            (Some(vs), _) => (usize::MAX, Some(vs)),
            (None, Some((k, vs))) => (*k, Some(vs)),
            (None, None) => (0, None),
        };
        for v in values.into_iter().flatten() {
            let m = if v.mutable { "mut " } else { "" };
            s.push_str(&format!("let {m}{}: {} = {};\n", v.name, v.ty, v.value));
        }
        for (i, prior_let) in self.persistent_lets.iter().enumerate() {
            let scoped = self
                .persistent_let_provider_scope
                .get(i)
                .is_some_and(|scope| !scope.is_empty());
            let origin = self.persistent_let_origin.get(i).copied().unwrap_or(0);
            if scoped || origin < from {
                continue;
            }
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
    }

    /// The program a cell runs as. With `snap` (v2), the bindings in scope
    /// at its end, each with whether it is `mut`: the program starts from
    /// [`Session::v2_values`] when there are some, instead of replaying
    /// the earlier cells, and ends by passing each binding to
    /// [`crate::mir::interp::REPL_SNAPSHOT_FN`].
    fn build_synthetic_cell(&mut self, cell_src: &str, snap: Option<&[(String, bool)]>) -> String {
        let snap_fn = crate::mir::interp::REPL_SNAPSHOT_FN;
        let mut s = String::new();
        s.push_str(&strip_main(&self.items_source));
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        if snap.is_some() {
            s.push_str(&format!("fn {snap_fn}[T](name: Str, value: T) {{}}\n"));
        }
        s.push_str("fn main() {\n");
        let mut ranges: Vec<(usize, Range<usize>)> = Vec::new();
        // Under a provider the cell is not recorded, but it still starts
        // from the values; a refused snapshot program falls back to the
        // replay, whose diagnostics name the cells.
        let values = snap.is_some() || !self.provider_stack.is_empty();
        self.push_cell_start(&mut s, &mut ranges, values);
        // The cell currently being evaluated occupies the next contiguous
        // slice of the wrapper body. Its 1-based index is one past the
        // current `cell_history` length (the entry has already been pushed
        // by the caller before `build_synthetic_cell` runs).
        let cell_idx = self.cell_history.len();
        let body = self.push_in_scopes(&mut s, &mut ranges, cell_src, true);
        if cell_idx > 0 {
            ranges.push((cell_idx, body));
        }
        for (name, _) in snap.into_iter().flatten() {
            s.push_str(&format!("{snap_fn}(\"{name}\", {name});\n"));
        }
        s.push_str("}\n");
        self.cell_byte_ranges = ranges;
        s
    }

    /// Extract every top-level `let` / `let mut` / `let ... else` statement
    /// from the cell body and append its raw source to `persistent_lets`.
    /// Called only after the cell evaluated cleanly so failed cells never
    /// leak partial bindings forward. A re-bound name's earlier binders
    /// stay in the buffer (same-scope shadowing is legal — the appended
    /// entry shadows them in replay order, exactly as in the `:save`
    /// export), so new entries simply append.
    ///
    /// Each new entry is tagged with the 1-based index of the current cell
    /// in `persistent_let_origin` so the diagnostic-rendering layer can map
    /// a span landing inside the replayed slice back to the originating
    /// cell.
    fn capture_new_lets(&mut self, cell_src: &str) {
        // A cell under a provider is not recorded: the values stay those
        // the scope opened with, and its `let`s replay after them.
        let pending = self.v2_pending.take();
        let move_start = self.v2_pending_move_start.take();
        // Cell index = `cell_history.len()` because the caller pushed the
        // current cell's source before invoking the wrapper pipeline.
        let cell_idx = self.cell_history.len();
        if self.provider_stack.is_empty() {
            if pending.is_some() {
                self.v2_replay_from = None;
            }
            self.v2_values = pending;
            if let Some(start) = move_start {
                self.v2_move_starts.insert(cell_idx, start);
            }
        }
        // Snapshot the active provider scope (resource names) so a future
        // `:end-provide R` knows whether each binding was declared inside
        // its scope. Empty Vec for bindings captured outside any scope.
        let active_scope: Vec<String> = self
            .provider_stack
            .iter()
            .map(|f| f.resource.clone())
            .collect();
        if active_scope.is_empty() {
            // v2 carries no values across cells, so a later cell replays
            // this one whole: its statements as well as its `let`s, so a
            // mutation (`v.push(x)`, `n = 5`) reaches the cells after it.
            // What the replay prints is dropped (`repl_v2::CELL_START`).
            // Under a provider the cell's `let`s alone are replayed, inside
            // the provider's closure (`Session::push_in_scopes`).
            self.persistent_lets.push(terminated(cell_src));
            self.persistent_let_origin.push(cell_idx);
            self.persistent_let_provider_scope.push(active_scope);
            return;
        }
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

    /// Drop every persistent `let` binding accumulated so far. Items
    /// (`fn` / `struct` / etc.) and cell history are left intact. Also
    /// clears the value-snapshot cache so a subsequent re-bind starts
    /// fresh (the source-replay buffer being empty means there's
    /// nothing to override on the next cell anyway, but the explicit
    /// clear keeps the two stores in sync).
    pub fn reset_persistent_lets(&mut self) {
        self.persistent_lets.clear();
        self.v2_values = Some(Vec::new());
        self.v2_move_starts.clear();
        self.v2_replay_from = None;
        self.persistent_let_origin.clear();
        self.persistent_let_provider_scope.clear();
        self.pruned_provider_lets.clear();
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
            // A cell's final `;` may be missing, which the cell after it
            // would run into.
            let stmt_cells: Vec<String> = self
                .cell_history
                .iter()
                .filter(|c| self.classify(c) != CellShape::PureItems)
                .map(|c| terminated(c))
                .collect();
            render_main_body(&stmt_cells.iter().map(String::as_str).collect::<Vec<_>>())
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
            if self.classify(cell_src) != CellShape::PureItems {
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
                        .push(ExportNode::Statement(terminated(src)));
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
        // a `pruned_provider_lets` entry.
        let mut kept_lets: Vec<String> = Vec::with_capacity(self.persistent_lets.len());
        let mut kept_origin: Vec<usize> = Vec::with_capacity(self.persistent_let_origin.len());
        let mut kept_scope: Vec<Vec<String>> =
            Vec::with_capacity(self.persistent_let_provider_scope.len());
        for (i, scope) in self.persistent_let_provider_scope.iter().enumerate() {
            if scope == &active_at_close {
                let let_src = &self.persistent_lets[i];
                for binding in parse_let_binding_names(let_src) {
                    self.pruned_provider_lets.push(PrunedProviderLet {
                        binding_name: binding,
                    });
                }
            } else {
                kept_lets.push(self.persistent_lets[i].clone());
                kept_origin.push(*self.persistent_let_origin.get(i).unwrap_or(&0));
                kept_scope.push(scope.clone());
            }
        }
        // The bindings the scope's cells made are gone; the values the
        // scope opened with are where the next cell starts again.
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
    /// The expression runs as the provider of `R` over an empty body, under
    /// any already-active provider scopes (so nested `:provide` forms that
    /// depend on outer providers validate correctly), so a provider that
    /// does not implement `R`'s trait is refused here rather than at the
    /// next cell. The program starts from the values the cells left
    /// ([`Session::push_cell_start`]), so the earlier cells do not run
    /// again. Returns `Err(message)` on any pipeline failure or runtime
    /// error; `Ok(())` indicates a clean construction. This method
    /// intentionally does NOT mutate session state — no `persistent_lets`
    /// capture, no `cell_byte_ranges` update — so the construction check
    /// is a pure validation step.
    fn try_construct_provider(&self, resource: &str, expr_src: &str) -> Result<(), String> {
        let inner = format!("with_provider[{resource}]({expr_src}, || {{}});");

        let mut synth = String::new();
        if !self.items_source.trim().is_empty() {
            synth.push_str(&strip_main(&self.items_source));
            if !synth.ends_with('\n') {
                synth.push('\n');
            }
        }
        synth.push_str("fn main() {\n");
        let mut ranges = Vec::new();
        self.push_cell_start(&mut synth, &mut ranges, true);
        self.push_in_scopes(&mut synth, &mut ranges, &inner, false);
        synth.push_str("}\n");

        crate::cli::repl_v2::run_cell(&synth)
            .map(|_| ())
            .map_err(|f| {
                format!(
                    "error: in `:provide {resource}` construction: {}",
                    f.errors.join("\n")
                )
            })
    }

    /// `body` written into `s` inside the open provider scopes, one
    /// `with_provider[R](expr, || { … })` per frame, outermost on the
    /// outside, each scope's closure starting with the `let`s its cells
    /// made, so they replay under its provider and are seen only in it.
    /// With `marker`, the [`crate::cli::repl_v2::CELL_START`] line comes
    /// just before `body`, so what the replay prints is dropped. Each
    /// replayed `let`'s place goes in `ranges`, with its cell; returns
    /// `body`'s place.
    fn push_in_scopes(
        &self,
        s: &mut String,
        ranges: &mut Vec<(usize, Range<usize>)>,
        body: &str,
        marker: bool,
    ) -> Range<usize> {
        for (depth, frame) in self.provider_stack.iter().enumerate() {
            s.push_str(&format!(
                "with_provider[{}]({}, || {{\n",
                frame.resource, frame.expr_src
            ));
            for (i, prior_let) in self.persistent_lets.iter().enumerate() {
                let in_frame = self
                    .persistent_let_provider_scope
                    .get(i)
                    .is_some_and(|scope| scope.len() == depth + 1);
                if !in_frame {
                    continue;
                }
                let start = s.len();
                s.push_str(prior_let);
                if !prior_let.ends_with('\n') {
                    s.push('\n');
                }
                if let Some(&cell) = self.persistent_let_origin.get(i).filter(|&&c| c > 0) {
                    ranges.push((cell, start..s.len()));
                }
            }
        }
        if marker {
            s.push_str(&format!(
                "println(\"{}\");\n",
                crate::cli::repl_v2::CELL_START
            ));
        }
        let start = s.len();
        s.push_str(body);
        if !body.ends_with('\n') {
            s.push('\n');
        }
        let end = s.len();
        for _ in &self.provider_stack {
            s.push_str("});\n");
        }
        start..end
    }
}
