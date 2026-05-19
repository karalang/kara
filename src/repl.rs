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

mod util;
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

/// Surface for the binary entry point: launch the REPL with caller-supplied
/// options. Used by `karac repl` flag wiring (`--auto-clone`).
pub fn run_with_options(opts: ReplOptions) {
    let banner = if opts.auto_clone {
        "Kāra REPL  (auto-clone on; type :help for commands, :quit to exit)"
    } else {
        "Kāra REPL  (type :help for commands, :quit to exit)"
    };
    println!("{banner}");

    let mut session = Session::with_options(opts);
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
/// error reply. The Jupyter kernel (tracker line 687, still `[ ]`)
/// will route `ok` true results into `display_data` / `execute_result`
/// and `ok` false results into `error` replies with the carried text
/// as the traceback body. The text is line-oriented and pre-trimmed
/// so a kernel can splice it into its output channel without
/// re-formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicOutput {
    pub text: String,
    pub ok: bool,
}

impl MagicOutput {
    /// Construct a successful magic reply.
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: true,
        }
    }

    /// Construct an error magic reply. The kernel maps the text into
    /// the `error` channel's traceback body.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: false,
        }
    }
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
    /// (line 687 of the tracker, still `[ ]`) routes every cell whose
    /// first token starts with `%` through this entry point; the
    /// returned `MagicOutput` carries the rendered text, the channel
    /// the kernel should display on (`stdout` vs `display_data` for
    /// rich shapes), and an `ok` flag the kernel maps to its protocol
    /// error semantics. The same dispatcher is exercised by the test
    /// suite so the surface is observable without a kernel binary.
    ///
    /// Supported magics (line 689 spec):
    ///
    /// - `%effects`               — session-wide effect set on items_source
    /// - `%ownership`             — current binding table with mode / RC status
    /// - `%explain <name>`        — wrap `karac explain` (concept or class)
    /// - `%set auto-clone on|off` — toggle the opt-in ownership mode
    /// - `%provide R = expr`      — open a cross-cell `with_provider` scope
    /// - `%end-provide R`         — close the matching provider scope
    ///
    /// The `%provide` / `%end-provide` forms parse cleanly but return a
    /// structured `not yet wired` error today — they share their
    /// compilation path with the `:provide` / `:end-provide` REPL
    /// meta-commands tracked at line 681 of the tracker, which has not
    /// shipped. Once line 681 lands, the wiring here forwards to the
    /// same handler the meta-command uses; the magic-side parser stays
    /// in place. `%rc` is deferred to post-MVP per the spec (RC
    /// fallback's introspection surface still settling).
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
            "provide" => MagicOutput::error(
                "magic `%provide` not yet wired — cross-cell provider scoping (`:provide` / `%provide`) lands once tracker line 681 ships; tracked at the `Cross-cell providers` entry in `docs/implementation_checklist/phase-5-diagnostics.md`",
            ),
            "end-provide" => MagicOutput::error(
                "magic `%end-provide` not yet wired — see `%provide` above; tracker line 681",
            ),
            "rc" => MagicOutput::error(
                "magic `%rc` is deferred to post-MVP per the line 689 spec — RC fallback's introspection surface is still settling",
            ),
            "" => MagicOutput::error(
                "empty magic command. Supported: %effects, %ownership, %explain <name>, %set auto-clone on|off, %provide R = expr, %end-provide R",
            ),
            other => MagicOutput::error(format!(
                "unknown magic `%{other}`. Supported: %effects, %ownership, %explain <name>, %set auto-clone on|off, %provide R = expr, %end-provide R"
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

    /// Compute the per-cell effect footer for a statement-style cell.
    /// Runs effect inference on the synthetic source (items + replayed
    /// lets + the just-evaluated cell body) and renders `main`'s
    /// effect set via the same `render_effect_decls` helper that
    /// drives `:save`'s declared-effects. Returns `""` for pure cells
    /// so the kernel can suppress the footer line entirely instead of
    /// rendering an empty annotation. Defensive on parse failure
    /// (returns `""` so footer rendering never surfaces a fresh
    /// parser error after a cell that already evaluated cleanly).
    fn compute_cell_effect_footer(&self, cell_src: &str) -> String {
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
            return String::new();
        }
        let effects = crate::effectcheck(&parsed.program);
        let Some(set) = effects.inferred_effects.get("main") else {
            return String::new();
        };
        render_effect_decls(set)
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
            }
            Err(out) => {
                for note in &out.notes {
                    eprintln!("{note}");
                }
                for m in &out.errors {
                    eprintln!("{m}");
                }
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
                    resolved
                        .errors
                        .iter()
                        .map(|e| format!("resolve error: {}", e.message))
                        .collect(),
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
                let effect_footer = self.compute_cell_effect_footer(trimmed);
                EvaluatedCell {
                    stdout: out.stdout.join(""),
                    errors: Vec::new(),
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
        let cell_idx = self.cell_history.len();
        let body_start = s.len();
        s.push_str(cell_src);
        if !cell_src.ends_with('\n') {
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
        for entry in scan_top_level_lets(cell_src) {
            self.persistent_lets.push(entry.slice);
            self.persistent_let_origin.push(cell_idx);
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
        // Walk persistent_lets and persistent_let_origin in lockstep so
        // the parallel cell-origin tags stay aligned with the slices.
        let mut kept_lets: Vec<String> = Vec::with_capacity(self.persistent_lets.len());
        let mut kept_origin: Vec<usize> = Vec::with_capacity(self.persistent_let_origin.len());
        for (i, prior) in self.persistent_lets.iter().enumerate() {
            let prior_names = parse_let_binding_names(prior);
            if !prior_names.iter().any(|n| new_names.contains(n)) {
                kept_lets.push(prior.clone());
                kept_origin.push(*self.persistent_let_origin.get(i).unwrap_or(&0));
            }
        }
        self.persistent_lets = kept_lets;
        self.persistent_let_origin = kept_origin;
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
        self.let_snapshots.clear();
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
        let stmt_cells: Vec<&str> = self
            .cell_history
            .iter()
            .filter(|c| classify_input(c) != CellShape::PureItems)
            .map(String::as_str)
            .collect();

        let body = render_main_body(&stmt_cells);
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
}
