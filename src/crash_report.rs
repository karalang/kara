//! Crash-report model + renderer — the consumer side of the `std.panic`
//! wire-format contract ([`docs/design.md § 4. Crash Report Format`]).
//!
//! When a Kāra program panics, the runtime's `std.panic` handler serializes a
//! structured JSON crash report (the wire format is a language-level contract
//! so production tooling can dedupe/group across compiler versions). This
//! module reads that JSON and renders it two ways, behind `karac debug`:
//!
//! - **Human-readable** ([`render`]) — the 3am-operator surface: panic site,
//!   kind, and message up top; the effect set in the shared compact form; the
//!   logical stack, parallel context, provider stack, and RC-fallback
//!   annotations; a build-metadata footer.
//! - **Structured** — `karac debug --output=json` re-emits the parsed JSON
//!   pretty-printed (a faithful, additive-safe passthrough; the CLI owns that
//!   path since it still holds the original [`serde_json::Value`]).
//!
//! ## Wire schema (proposed v1)
//!
//! `std.panic` does not emit yet, so this consumer *pins down* the concrete
//! JSON keys for the eight required fields of design.md § 4. The emitter, when
//! it lands, matches these keys; the fixture at `examples/crash/` is the
//! executable spec. Parsing is deliberately lenient — every field is optional
//! at the parse layer (missing → placeholder / empty), and unknown keys are
//! ignored — so the wire format can grow additively (design.md § 4 *Stability*)
//! without breaking an older `karac debug`.
//!
//! | # | design.md § 4 field        | JSON key                  |
//! |---|----------------------------|---------------------------|
//! | 1 | Panic site                 | `panic_site`              |
//! | 2 | Panic kind discriminant    | `panic_kind`              |
//! | 3 | Message                    | `message`                 |
//! | 4 | Logical stack              | `logical_stack`           |
//! | 5 | Provider stack             | `provider_stack`          |
//! | 6 | RC-fallback annotations    | `rc_fallback`             |
//! | 7 | Parallel context           | `parallel_context`        |
//! | 8 | Build metadata             | `build_metadata`          |
//!
//! Optional extras: `caused_by` (drop-during-unwind chain), `concurrent_with`
//! (sibling crash-file paths), `tracing` (`trace_id`/`span_id`, OTel field
//! names), `gpu_marker`.

use crate::ast::EffectVerbKind;
use crate::effect_render::{render_compact, verb_from_keyword, ColorChoice};
use serde_json::Value;

// ── Model ────────────────────────────────────────────────────────────────────

/// A `file:line:column` source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLoc {
    pub file: String,
    pub line: u64,
    pub column: u64,
}

impl SourceLoc {
    fn render(&self) -> String {
        format!("{}:{}:{}", self.file, self.line, self.column)
    }
}

/// Field 1 — where the panic was raised. Under `#[track_caller]` the location
/// is the caller's site while `function` still names the emitting frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanicSite {
    pub loc: Option<SourceLoc>,
    pub function: Option<String>,
    pub instruction_pointer: Option<String>,
}

/// One frame of the logical stack (field 4): a `par` block, a `suspends` task,
/// or an ordinary call frame. Native FFI frames are opaque (`native: true`,
/// no effect data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub function: String,
    pub loc: Option<SourceLoc>,
    /// `(verb, resource)` pairs; resource empty for execution verbs / `panics`.
    pub effects: Vec<(EffectVerbKind, String)>,
    pub native: bool,
}

/// One `with_provider` binding active at panic time (field 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderBinding {
    pub resource: String,
    pub provider: String,
    pub bound_at: Option<SourceLoc>,
}

/// One RC-fallback annotation (field 6): a binding the compiler chose to
/// represent via RC, and where/why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcFallback {
    pub binding: String,
    pub chosen_at: Option<SourceLoc>,
    pub reason: Option<String>,
}

/// A sibling branch cancelled by fail-fast, with the effect boundary at which
/// cancellation took effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledSibling {
    pub name: String,
    pub effect_boundary: Option<String>,
}

/// Field 7 — the `par {}` / `spawn()` / `TaskGroup` context of the panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParallelContext {
    pub spawn_site_id: Option<u64>,
    pub worker_index: Option<u64>,
    pub panicked_branch: Option<String>,
    pub siblings_running: Vec<String>,
    pub siblings_cancelled: Vec<CancelledSibling>,
}

/// Field 8 — build provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildMetadata {
    pub kara_version: Option<String>,
    pub compiler_commit: Option<String>,
    pub target_triple: Option<String>,
    pub profile: Option<String>,
    pub edition: Option<String>,
}

impl BuildMetadata {
    fn is_empty(&self) -> bool {
        self.kara_version.is_none()
            && self.compiler_commit.is_none()
            && self.target_triple.is_none()
            && self.profile.is_none()
            && self.edition.is_none()
    }
}

/// Optional `std.tracing` cross-link (OTel field names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracing {
    pub trace_id: String,
    pub span_id: Option<String>,
}

/// A parsed crash report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    pub panic_site: PanicSite,
    pub panic_kind: String,
    pub message: String,
    pub logical_stack: Vec<Frame>,
    pub provider_stack: Vec<ProviderBinding>,
    pub rc_fallback: Vec<RcFallback>,
    pub parallel_context: Option<ParallelContext>,
    pub build_metadata: BuildMetadata,
    /// Drop-during-unwind chain: the original triggering panic.
    pub caused_by: Option<Box<CrashReport>>,
    /// Cross-references to concurrent tasks' crash files.
    pub concurrent_with: Vec<String>,
    pub tracing: Option<Tracing>,
    pub gpu_marker: Option<String>,
}

// ── Parse (lenient — every field optional, unknown keys ignored) ─────────────

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn u64_field(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

fn parse_loc(v: &Value) -> Option<SourceLoc> {
    let file = str_field(v, "file")?;
    Some(SourceLoc {
        file,
        line: u64_field(v, "line").unwrap_or(0),
        column: u64_field(v, "column").unwrap_or(0),
    })
}

/// Parse a `[{"verb","resource"}, …]` array into `(EffectVerbKind, String)`
/// pairs, routing verb keywords through the shared [`verb_from_keyword`] so the
/// reconstructed set renders identically to a freshly-inferred one.
fn parse_effects(v: &Value) -> Vec<(EffectVerbKind, String)> {
    v.get("effects")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let verb = str_field(e, "verb")?;
                    let resource = str_field(e, "resource").unwrap_or_default();
                    Some((verb_from_keyword(&verb), resource))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_frame(v: &Value) -> Frame {
    Frame {
        function: str_field(v, "function").unwrap_or_else(|| "<unknown>".to_string()),
        loc: v.get("loc").and_then(parse_loc).or_else(|| parse_loc(v)),
        effects: parse_effects(v),
        native: v.get("native").and_then(Value::as_bool).unwrap_or(false),
    }
}

fn parse_parallel_context(v: &Value) -> ParallelContext {
    let siblings_running = v
        .get("siblings_running")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let siblings_cancelled = v
        .get("siblings_cancelled")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    // Accept either a bare string or {name, effect_boundary}.
                    if let Some(name) = s.as_str() {
                        return Some(CancelledSibling {
                            name: name.to_string(),
                            effect_boundary: None,
                        });
                    }
                    Some(CancelledSibling {
                        name: str_field(s, "name")?,
                        effect_boundary: str_field(s, "effect_boundary"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    ParallelContext {
        spawn_site_id: u64_field(v, "spawn_site_id"),
        worker_index: u64_field(v, "worker_index"),
        panicked_branch: str_field(v, "panicked_branch"),
        siblings_running,
        siblings_cancelled,
    }
}

fn parse_array<T>(root: &Value, key: &str, f: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    root.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(&f).collect())
        .unwrap_or_default()
}

impl CrashReport {
    /// Parse a crash report from an already-decoded [`Value`]. Fails only when
    /// the value is not a JSON object; every field is otherwise optional.
    pub fn from_value(v: &Value) -> Result<CrashReport, String> {
        if !v.is_object() {
            return Err("crash report must be a JSON object".to_string());
        }
        let panic_site = v
            .get("panic_site")
            .map(|ps| PanicSite {
                loc: parse_loc(ps),
                function: str_field(ps, "function"),
                instruction_pointer: str_field(ps, "instruction_pointer"),
            })
            .unwrap_or(PanicSite {
                loc: None,
                function: None,
                instruction_pointer: None,
            });

        let caused_by = v
            .get("caused_by")
            .and_then(|c| CrashReport::from_value(c).ok())
            .map(Box::new);

        let tracing = v.get("tracing").and_then(|t| {
            Some(Tracing {
                trace_id: str_field(t, "trace_id")?,
                span_id: str_field(t, "span_id"),
            })
        });

        Ok(CrashReport {
            panic_site,
            panic_kind: str_field(v, "panic_kind").unwrap_or_else(|| "unknown".to_string()),
            message: str_field(v, "message").unwrap_or_else(|| "<no message>".to_string()),
            logical_stack: parse_array(v, "logical_stack", |f| Some(parse_frame(f))),
            provider_stack: parse_array(v, "provider_stack", |p| {
                Some(ProviderBinding {
                    resource: str_field(p, "resource").unwrap_or_else(|| "<resource>".to_string()),
                    provider: str_field(p, "provider").unwrap_or_else(|| "<provider>".to_string()),
                    bound_at: p.get("bound_at").and_then(parse_loc),
                })
            }),
            rc_fallback: parse_array(v, "rc_fallback", |r| {
                Some(RcFallback {
                    binding: str_field(r, "binding")?,
                    chosen_at: r.get("chosen_at").and_then(parse_loc),
                    reason: str_field(r, "reason"),
                })
            }),
            parallel_context: v.get("parallel_context").map(parse_parallel_context),
            build_metadata: BuildMetadata {
                kara_version: v
                    .get("build_metadata")
                    .and_then(|b| str_field(b, "kara_version")),
                compiler_commit: v
                    .get("build_metadata")
                    .and_then(|b| str_field(b, "compiler_commit")),
                target_triple: v
                    .get("build_metadata")
                    .and_then(|b| str_field(b, "target_triple")),
                profile: v
                    .get("build_metadata")
                    .and_then(|b| str_field(b, "profile")),
                edition: v
                    .get("build_metadata")
                    .and_then(|b| str_field(b, "edition")),
            },
            caused_by,
            concurrent_with: v
                .get("concurrent_with")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            tracing,
            gpu_marker: str_field(v, "gpu_marker"),
        })
    }
}

// ── Render (human-readable) ──────────────────────────────────────────────────

// ANSI SGR. The crash renderer's own palette (section headers bold, the panic
// kind red); the effect line is coloured by `render_compact` itself.
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

struct Palette {
    on: bool,
}
impl Palette {
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("{code}{s}{RESET}")
        } else {
            s.to_string()
        }
    }
    fn bold(&self, s: &str) -> String {
        self.wrap(BOLD, s)
    }
    fn dim(&self, s: &str) -> String {
        self.wrap(DIM, s)
    }
    fn red(&self, s: &str) -> String {
        self.wrap(RED, s)
    }
    fn cyan(&self, s: &str) -> String {
        self.wrap(CYAN, s)
    }
}

/// Human-friendly one-liner for a panic-kind discriminant. Unknown kinds
/// (the vocabulary is open/additive) fall back to the raw string.
fn panic_kind_label(kind: &str) -> &str {
    match kind {
        "explicit" => "explicit panic",
        "rc_fallback_borrow" => "RC-fallback borrow conflict",
        "par_fail_fast_cancel" => "parallel fail-fast cancellation",
        "div_by_zero" => "division by zero",
        "index_out_of_bounds" => "index out of bounds",
        "effect_boundary_violation" => "effect-boundary violation",
        "provider_missing" => "missing provider",
        "unreachable" => "reached unreachable code",
        "drop_during_unwind" => "panic while unwinding (drop)",
        "gpu_kernel_failed" => "GPU kernel failure",
        other => other,
    }
}

fn effects_line(effects: &[(EffectVerbKind, String)], color: ColorChoice) -> String {
    render_compact(effects.iter().map(|(v, r)| (v, r.as_str())), color)
}

/// Render `report` as a human-readable crash report. `color` controls ANSI
/// (Auto honours TTY + `NO_COLOR`).
pub fn render(report: &CrashReport, color: ColorChoice) -> String {
    let on = color_enabled(color);
    let p = Palette { on };
    let mut out = String::new();
    render_into(&mut out, report, &p, color, 0);
    out
}

/// Resolve a [`ColorChoice`] to a bool the same way `effect_render` does, so
/// the crash renderer's own ANSI and `render_compact`'s agree. `Auto` → TTY on
/// native, never on wasm.
fn color_enabled(color: ColorChoice) -> bool {
    match color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => auto_color(),
    }
}

#[cfg(not(target_family = "wasm"))]
fn auto_color() -> bool {
    use std::io::IsTerminal;
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}
#[cfg(target_family = "wasm")]
fn auto_color() -> bool {
    false
}

fn render_into(
    out: &mut String,
    report: &CrashReport,
    p: &Palette,
    color: ColorChoice,
    depth: usize,
) {
    use std::fmt::Write as _;

    // ── Header: kind + site + message ────────────────────────────────────────
    let rule = "━".repeat(60);
    if depth == 0 {
        let _ = writeln!(out, "{}", p.dim(&rule));
        let _ = writeln!(out, "{}", p.bold("  Kāra crash report"));
        let _ = writeln!(out, "{}", p.dim(&rule));
        let _ = writeln!(out);
    } else {
        // A `caused_by` nested report.
        let _ = writeln!(out, "  {}", p.bold("caused by:"));
    }

    let kind = panic_kind_label(&report.panic_kind);
    let _ = writeln!(out, "  {} {}", p.red(&p.bold("panic:")), p.red(kind));
    if let Some(loc) = &report.panic_site.loc {
        let fnname = report
            .panic_site
            .function
            .as_deref()
            .map(|f| format!("  ({f})"))
            .unwrap_or_default();
        let _ = writeln!(out, "  {} {}{}", p.dim("at"), loc.render(), p.dim(&fnname));
    } else if let Some(f) = &report.panic_site.function {
        let _ = writeln!(out, "  {} {}", p.dim("in"), f);
    }
    let _ = writeln!(out, "  {}", report.message);

    // ── Effect set (shared compact form) ─────────────────────────────────────
    // The panic-site effects are the top frame's if present, else the union of
    // all frames — but for a single-line summary we use the top frame's set,
    // which is where the panic actually occurred.
    if let Some(top) = report.logical_stack.first() {
        if !top.effects.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "  {} {}",
                p.dim("effects:"),
                effects_line(&top.effects, color)
            );
        }
    }

    // ── Logical stack ────────────────────────────────────────────────────────
    if !report.logical_stack.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "  {}", p.bold("logical stack:"));
        for frame in &report.logical_stack {
            let loc = frame
                .loc
                .as_ref()
                .map(SourceLoc::render)
                .unwrap_or_else(|| "<no location>".to_string());
            let marker = if frame.native { "⋯" } else { "▸" };
            let name = if frame.native {
                p.dim(&frame.function)
            } else {
                frame.function.clone()
            };
            let eff = if frame.native {
                p.dim("(native frame)")
            } else if frame.effects.is_empty() {
                p.dim("pure")
            } else {
                effects_line(&frame.effects, color)
            };
            let _ = writeln!(out, "    {marker} {name}");
            let _ = writeln!(out, "        {}  {}", p.dim(&loc), eff);
        }
    }

    // ── Parallel context ─────────────────────────────────────────────────────
    if let Some(pc) = &report.parallel_context {
        let _ = writeln!(out);
        let _ = writeln!(out, "  {}", p.bold("parallel context:"));
        let branch = pc.panicked_branch.clone().unwrap_or_else(|| {
            pc.spawn_site_id
                .map(|id| format!("spawn_site_{id}"))
                .unwrap_or_else(|| "<branch>".to_string())
        });
        let worker = pc
            .worker_index
            .map(|w| format!(" (worker {w})"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "    {} {}{}",
            p.dim("panicked branch:"),
            p.cyan(&branch),
            p.dim(&worker)
        );
        if !pc.siblings_running.is_empty() {
            let _ = writeln!(
                out,
                "    {} {}",
                p.dim("still running:"),
                pc.siblings_running.join(", ")
            );
        }
        for c in &pc.siblings_cancelled {
            let at = c
                .effect_boundary
                .as_ref()
                .map(|b| format!(" at {} boundary", effects_boundary_label(b)))
                .unwrap_or_default();
            let _ = writeln!(out, "    {} {}{}", p.dim("cancelled:"), c.name, p.dim(&at));
        }
    }

    // ── Provider stack ───────────────────────────────────────────────────────
    if !report.provider_stack.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "  {}", p.bold("provider stack:"));
        for pb in &report.provider_stack {
            let at = pb
                .bound_at
                .as_ref()
                .map(|l| format!("   {} {}", p.dim("bound at"), l.render()))
                .unwrap_or_default();
            let _ = writeln!(out, "    {} ← {}{}", pb.resource, pb.provider, at);
        }
    }

    // ── RC-fallback annotations ──────────────────────────────────────────────
    if !report.rc_fallback.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "  {}", p.bold("RC fallback:"));
        for rc in &report.rc_fallback {
            let at = rc
                .chosen_at
                .as_ref()
                .map(|l| format!(" at {}", l.render()))
                .unwrap_or_default();
            let _ = writeln!(out, "    `{}` became RC{}", rc.binding, p.dim(&at));
            if let Some(reason) = &rc.reason {
                let _ = writeln!(out, "        {} {}", p.dim("reason:"), reason);
            }
        }
    }

    // ── Tracing cross-link ───────────────────────────────────────────────────
    if let Some(t) = &report.tracing {
        let _ = writeln!(out);
        let span = t
            .span_id
            .as_ref()
            .map(|s| format!(" (span: {s})"))
            .unwrap_or_default();
        let _ = writeln!(out, "  {} {}{}", p.dim("trace:"), t.trace_id, p.dim(&span));
    }

    // ── GPU marker ───────────────────────────────────────────────────────────
    if let Some(g) = &report.gpu_marker {
        let _ = writeln!(out, "  {} {}", p.dim("gpu:"), g);
    }

    // ── caused_by chain ──────────────────────────────────────────────────────
    if let Some(cause) = &report.caused_by {
        let _ = writeln!(out);
        render_into(out, cause, p, color, depth + 1);
    }

    // ── concurrent crash files ───────────────────────────────────────────────
    if depth == 0 && !report.concurrent_with.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  {} {}",
            p.dim("concurrent crashes:"),
            report.concurrent_with.join(", ")
        );
    }

    // ── Build-metadata footer ────────────────────────────────────────────────
    if depth == 0 && !report.build_metadata.is_empty() {
        let m = &report.build_metadata;
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = &m.kara_version {
            let commit = m
                .compiler_commit
                .as_ref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default();
            parts.push(format!("kara {v}{commit}"));
        } else if let Some(c) = &m.compiler_commit {
            parts.push(c.clone());
        }
        if let Some(t) = &m.target_triple {
            parts.push(t.clone());
        }
        if let Some(pr) = &m.profile {
            parts.push(pr.clone());
        }
        if let Some(e) = &m.edition {
            parts.push(e.clone());
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "  {}", p.dim(&"─".repeat(60)));
        let _ = writeln!(out, "  {}", p.dim(&parts.join(" · ")));
    }
}

/// A cancellation boundary in a crash report is stored as an effect keyword
/// (e.g. `"sends"`); render it as the compact effect atom so the boundary reads
/// consistently with every other effect surface.
fn effects_boundary_label(boundary: &str) -> String {
    // A boundary may be a bare verb keyword or already a rendered atom; if it
    // parses as a known/user verb keyword, canonicalise it, else pass through.
    let verb = verb_from_keyword(boundary);
    render_compact(std::iter::once((&verb, "")), ColorChoice::Never)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!({
            "panic_site": {
                "file": "src/handlers.kara", "line": 42, "column": 18,
                "function": "fetch_user_dashboard",
                "instruction_pointer": "0x1040"
            },
            "panic_kind": "index_out_of_bounds",
            "message": "index out of bounds: the len is 3 but the index is 99",
            "logical_stack": [
                { "function": "fetch_user_dashboard", "file": "src/handlers.kara", "line": 42, "column": 18,
                  "effects": [ {"verb":"sends","resource":"Network"}, {"verb":"reads","resource":"UserDB"} ] },
                { "function": "handle_request", "file": "src/server.kara", "line": 88, "column": 5,
                  "effects": [ {"verb":"reads","resource":"UserDB"} ] },
                { "function": "main", "file": "src/main.kara", "line": 12, "column": 3, "effects": [] }
            ],
            "provider_stack": [
                { "resource": "Db", "provider": "PostgresPool",
                  "bound_at": {"file":"src/main.kara","line":8,"column":3} }
            ],
            "rc_fallback": [
                { "binding": "session", "chosen_at": {"file":"src/handlers.kara","line":40,"column":9},
                  "reason": "captured by closure with subsequent outer use" }
            ],
            "parallel_context": {
                "spawn_site_id": 42, "worker_index": 3, "panicked_branch": "par@spawn_site_42",
                "siblings_running": ["render_sidebar", "load_notifications"],
                "siblings_cancelled": [ {"name":"fetch_activity","effect_boundary":"sends"} ]
            },
            "build_metadata": {
                "kara_version": "0.1.0", "compiler_commit": "abc1234",
                "target_triple": "x86_64-unknown-linux-gnu", "profile": "release", "edition": "2026"
            }
        })
    }

    #[test]
    fn parses_all_eight_fields() {
        let r = CrashReport::from_value(&sample()).unwrap();
        assert_eq!(r.panic_kind, "index_out_of_bounds");
        assert_eq!(r.panic_site.loc.as_ref().unwrap().line, 42);
        assert_eq!(
            r.panic_site.function.as_deref(),
            Some("fetch_user_dashboard")
        );
        assert_eq!(r.logical_stack.len(), 3);
        assert_eq!(r.provider_stack.len(), 1);
        assert_eq!(r.rc_fallback.len(), 1);
        let pc = r.parallel_context.as_ref().unwrap();
        assert_eq!(pc.spawn_site_id, Some(42));
        assert_eq!(pc.siblings_cancelled[0].name, "fetch_activity");
        assert_eq!(r.build_metadata.kara_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn top_frame_effects_render_in_canonical_order() {
        // Declared sends-before-reads; compact re-sorts to reads then sends.
        let r = CrashReport::from_value(&sample()).unwrap();
        let line = effects_line(&r.logical_stack[0].effects, ColorChoice::Never);
        assert_eq!(line, "reads(UserDB) + sends(Network)");
    }

    #[test]
    fn render_is_uncolored_under_never_and_hits_every_section() {
        let r = CrashReport::from_value(&sample()).unwrap();
        let out = render(&r, ColorChoice::Never);
        assert!(!out.contains('\x1b'), "Never must not emit ANSI");
        assert!(out.contains("panic: index out of bounds"));
        assert!(out.contains("src/handlers.kara:42:18"));
        assert!(out.contains("effects: reads(UserDB) + sends(Network)"));
        assert!(out.contains("logical stack:"));
        assert!(out.contains("parallel context:"));
        assert!(out.contains("par@spawn_site_42"));
        assert!(out.contains("worker 3"));
        assert!(out.contains("provider stack:"));
        assert!(out.contains("PostgresPool"));
        assert!(out.contains("RC fallback:"));
        assert!(out.contains("`session` became RC"));
        assert!(out.contains("x86_64-unknown-linux-gnu"));
        assert!(out.contains("kara 0.1.0 (abc1234)"));
    }

    #[test]
    fn render_colored_wraps_and_resets() {
        let r = CrashReport::from_value(&sample()).unwrap();
        let out = render(&r, ColorChoice::Always);
        assert!(out.contains('\x1b'));
        assert!(out.contains(RESET));
    }

    #[test]
    fn lenient_parse_fills_placeholders_and_ignores_unknown_keys() {
        let v = serde_json::json!({ "panic_kind": "explicit", "totally_new_field": 7 });
        let r = CrashReport::from_value(&v).unwrap();
        assert_eq!(r.panic_kind, "explicit");
        assert_eq!(r.message, "<no message>");
        assert!(r.panic_site.loc.is_none());
        assert!(r.logical_stack.is_empty());
        // Renders without panicking even when almost everything is absent.
        let out = render(&r, ColorChoice::Never);
        assert!(out.contains("panic: explicit panic"));
    }

    #[test]
    fn non_object_is_rejected() {
        let v = serde_json::json!([1, 2, 3]);
        assert!(CrashReport::from_value(&v).is_err());
    }

    #[test]
    fn caused_by_chain_and_tracing_render() {
        let v = serde_json::json!({
            "panic_kind": "drop_during_unwind",
            "message": "panic in Drop",
            "tracing": { "trace_id": "abc123def4567890", "span_id": "1234567890abcdef" },
            "caused_by": { "panic_kind": "explicit", "message": "original boom" }
        });
        let r = CrashReport::from_value(&v).unwrap();
        assert_eq!(r.caused_by.as_ref().unwrap().message, "original boom");
        let out = render(&r, ColorChoice::Never);
        assert!(out.contains("caused by:"));
        assert!(out.contains("original boom"));
        assert!(out.contains("trace: abc123def4567890"));
        assert!(out.contains("span: 1234567890abcdef"));
    }
}
