//! Compiler queries channel. Phase-8 stdlib-floor § Compiler queries
//! channel sub-item 2. Spec at `docs/design.md § Specification Layers
//! > Compiler Queries`.
//!
//! A *query* is a decision the compiler hedged on — a site where the
//! cost model would benefit from author confirmation rather than guess.
//! Each phase that runs in the pipeline carries a `Vec<CompilerQuery>`
//! on its result struct; phases push queries as they encounter decision
//! sites with attributable alternatives. The CLI's `karac query
//! queries` subcommand (phase-8 sub-item 3) collates and renders the
//! union. Authors resolve queries by writing attributes named in the
//! [`ResolutionSurface`] — subsequent compiles re-emit only the
//! still-open queries.
//!
//! **v1 ships the channel infrastructure plus the P1.3 catalogue entry.**
//! P1.3 (inlining + branch hints) landed 2026-05-18 — emitted by the
//! plain-data [`crate::codegen_queries`] analyzer, not from a phase
//! result struct (it walks the parsed AST directly; see the CLI's
//! `query_queries`). The remaining entries land alongside the phases
//! that populate them — P1.1 (RC fallback, `ownership.rs`), P1.2
//! (specialization, `typechecker.rs`), P1.4 (effect-set narrowing,
//! `effectchecker.rs`), P1.5 (layout), P1.6 (fork threshold,
//! `concurrency.rs`); the `queries` vec on each of those phase results
//! is still empty. Each new variant on [`QueryKind`] is a non-breaking
//! addition for tools that gracefully ignore unknown variants
//! (matches the streaming-output discipline from Phase 5 §
//! Structured Compiler Output).

use crate::def_path::QueryId;
use crate::token::Span;

/// One query emitted by a pipeline phase. Tools persist resolved
/// answers keyed on [`QueryId`]; subsequent compiles drop the
/// resolved entries from the queries report.
#[derive(Debug, Clone)]
pub struct CompilerQuery {
    /// Stable identity (def-path + sub-item hash). Survives unrelated
    /// source edits; tools can store answers cross-compile.
    pub id: QueryId,
    /// Source span of the decision site — used for human-readable
    /// `--format=md` rendering. Not part of the query identity
    /// (`QueryId` is span-free by construction).
    pub site: Span,
    /// What kind of decision this is. v1 ships P1.3
    /// (`InliningDecision` / `BranchHint`); the remaining catalogue
    /// entries land alongside their populating phases.
    pub kind: QueryKind,
    /// Alternatives the compiler considered. `default` indexes the
    /// option the compiler would pick absent a resolution attribute.
    pub options: Vec<QueryOption>,
    /// Index into `options` — the compiler's pick.
    pub default: usize,
    /// How confident the compiler is in its default. Low / Medium /
    /// High lets the report sort queries by "most worth confirming
    /// first."
    pub default_confidence: Confidence,
    /// Attributes the author can write at the decision site to
    /// resolve this query. The query disappears from the next
    /// compile's report once any of these annotations lands.
    pub resolution_surface: ResolutionSurface,
    /// Reserved slot for post-v1 cross-phase deduplication. v1 emits
    /// every cross-phase-discovered query independently; v2+ may
    /// merge — e.g. typechecker proposing specialization on
    /// monomorphization counts vs codegen proposing on hot-path
    /// data — but the field is present in v1 so the merger is non-
    /// breaking.
    pub cross_phase_origin: Option<Phase>,
}

/// What kind of query this is. P1.3 (phase-7-codegen.md line 25)
/// landed the first two real catalogue entries — `InliningDecision`
/// and `BranchHint`, both emitted by [`crate::codegen_queries`].
/// Marked `non_exhaustive` so adding the remaining catalogue entries
/// (P1.1 RC fallback, P1.2 specialization, P1.4 effect-set narrowing,
/// P1.5 layout, P1.6 fork threshold) stays a non-breaking change for
/// downstream tooling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum QueryKind {
    /// Placeholder so the enum is non-trivial and matchable from v1
    /// even when the analyzer pushes no queries. Never emitted by any
    /// phase.
    Stub,
    /// Inlining decision at a non-attributed function definition that
    /// reads as "hot-looking" (called from inside loops at multiple
    /// sites). Resolution surface: `#[inline]` / `#[inline(never)]`
    /// on the function definition.
    InliningDecision,
    /// Branch-hint decision at a `match` expression or `if`/`else`
    /// with no `#[likely]`/`#[unlikely]` annotation on any arm/branch
    /// when the cost model considers the choice notable (asymmetric
    /// arm/branch body sizes). Resolution surface: `#[likely]` /
    /// `#[unlikely]` on the arm or branch the author considers hot.
    BranchHint,
}

/// One alternative the compiler considered at a decision site.
#[derive(Debug, Clone)]
pub struct QueryOption {
    /// Short label rendered in the report (e.g. `"inline"`,
    /// `"keep_call"`, `"specialize_T_i64"`).
    pub label: String,
    /// Optional one-line note explaining the cost the compiler
    /// associates with this option.
    pub note: Option<String>,
}

/// Confidence in the default pick. Drives "which queries should the
/// author look at first?" sorting in the report — `Low` first,
/// `High` last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Attributes the author can write at the decision site to resolve
/// this query. The first compile that sees one of these attributes
/// at the site drops the query from the next report.
#[derive(Debug, Clone, Default)]
pub struct ResolutionSurface {
    /// Attribute names (single-segment paths) that resolve this
    /// query. Path-form attrs (`#[diagnostic::*]`) are not currently
    /// query-resolving; the catalogue convention is bare names.
    pub attributes: Vec<String>,
}

/// Which pipeline phase emitted a given query. The phase is recorded
/// on the `cross_phase_origin` field for post-v1 cross-phase
/// deduplication; v1 readers can also use it to attribute queries to
/// the originating phase for debugging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Resolver,
    TypeChecker,
    EffectChecker,
    Ownership,
    Concurrency,
    Codegen,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_orderable_by_attention_priority() {
        // The report sorts Low → Medium → High so the author looks at
        // the most-uncertain queries first. The enum just needs to be
        // comparable; the sort key is computed by the renderer.
        assert_ne!(Confidence::Low, Confidence::High);
    }

    #[test]
    fn resolution_surface_default_is_empty() {
        let r: ResolutionSurface = Default::default();
        assert!(r.attributes.is_empty());
    }

    #[test]
    fn query_kind_stub_is_matchable() {
        // Smoke that the placeholder variant exists and is
        // pattern-matchable from outside the module.
        let k = QueryKind::Stub;
        let matched = matches!(k, QueryKind::Stub);
        assert!(matched);
    }
}
