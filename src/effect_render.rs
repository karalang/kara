// src/effect_render.rs

//! Shared effect-set rendering — the one canonical surface for turning a set
//! of effects into text.
//!
//! Three renderings share a single structured root (a canonically-ordered,
//! deduplicated list of `(verb, resource)` atoms):
//!
//! - [`render_compact`] — one line, `reads(UserDB) + sends(Network) + panics`,
//!   matching the source effect-combination operator `+`. For CLI output and
//!   crash reports.
//! - [`render_grouped`] — multi-line, bucketed into Resource / Execution /
//!   Panic with empty groups omitted. For IDE hover.
//! - [`effects_json`] — a `serde_json::Value` array, never colored. The
//!   structured root the other two derive from; for `--output=json`.
//!
//! Ordering is **group-first-then-alphabetical** and fully stable: the same
//! effect set always renders identically, byte for byte, so renderings diff
//! cleanly across revisions (`karac query effects --diff`).
//!
//! This consolidates the verb→keyword map and the canonical verb ordering that
//! were copy-pasted across the REPL footer, the C-header emitter, the
//! formatter, the doc generator, and more. The shared home is a plain module
//! in the compiler lib crate, which the `lsp` workspace member already depends
//! on — so no separate crate is needed to share it (per the roadmap's
//! "rendering logic shared by `karac` and the LSP server").

use crate::ast::EffectVerbKind;
use std::collections::BTreeMap;
use std::io::IsTerminal;

/// Display bucket for the grouped rendering. Note this is a *rendering*
/// grouping, not the effect model's verb classification: `panics` is a
/// resource verb for conflict analysis but gets its own bucket here because
/// it reads very differently to an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectGroup {
    /// `reads`, `writes`, `sends`, `receives`, `allocates`, and user-defined
    /// verbs — everything that acts on a named resource.
    Resource,
    /// `blocks`, `suspends` — scheduler-placement verbs, no resource.
    Execution,
    /// `panics` — control-flow, surfaced on its own for readability.
    Panic,
}

/// When to emit ANSI color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Color iff stdout is a TTY and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always color (the caller has decided the sink accepts ANSI).
    Always,
    /// Never color (JSON, files, `--no-color`, piped output).
    Never,
}

impl ColorChoice {
    /// Resolve `Auto` against the environment. `Always`/`Never` pass through.
    fn enabled(self) -> bool {
        match self {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            // NO_COLOR (https://no-color.org): any value, even empty, disables.
            ColorChoice::Auto => {
                std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
            }
        }
    }
}

// ANSI SGR codes. Resource cyan, execution yellow, panic red — per the
// Track 6 spec. Only the atom text is wrapped; JSON output never reaches here.
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

fn group_color(group: EffectGroup) -> &'static str {
    match group {
        EffectGroup::Resource => CYAN,
        EffectGroup::Execution => YELLOW,
        EffectGroup::Panic => RED,
    }
}

/// The keyword form of a verb (`reads`, `blocks`, …). User-defined verbs
/// render as their declared name. Borrowed, so no allocation for the caller.
pub fn verb_keyword(verb: &EffectVerbKind) -> &str {
    match verb {
        EffectVerbKind::Reads => "reads",
        EffectVerbKind::Writes => "writes",
        EffectVerbKind::Sends => "sends",
        EffectVerbKind::Receives => "receives",
        EffectVerbKind::Allocates => "allocates",
        EffectVerbKind::Panics => "panics",
        EffectVerbKind::Blocks => "blocks",
        EffectVerbKind::Suspends => "suspends",
        EffectVerbKind::UserDefined(name) => name.as_str(),
    }
}

/// Canonical sort rank for the eight built-in verbs; user-defined verbs share
/// the last rank and are disambiguated by keyword at the call sites that build
/// the ordered map (so two distinct user verbs never collide).
pub fn verb_order(verb: &EffectVerbKind) -> usize {
    match verb {
        EffectVerbKind::Reads => 0,
        EffectVerbKind::Writes => 1,
        EffectVerbKind::Sends => 2,
        EffectVerbKind::Receives => 3,
        EffectVerbKind::Allocates => 4,
        EffectVerbKind::Panics => 5,
        EffectVerbKind::Blocks => 6,
        EffectVerbKind::Suspends => 7,
        EffectVerbKind::UserDefined(_) => 8,
    }
}

/// Which display bucket a verb renders under.
pub fn verb_group(verb: &EffectVerbKind) -> EffectGroup {
    match verb {
        EffectVerbKind::Panics => EffectGroup::Panic,
        EffectVerbKind::Blocks | EffectVerbKind::Suspends => EffectGroup::Execution,
        _ => EffectGroup::Resource,
    }
}

/// One rendered verb with its (possibly empty) resource list, in canonical
/// order — the structured root all three renderings share.
struct Atom {
    verb: EffectVerbKind,
    /// Sorted, deduplicated resources. Empty for execution verbs / `panics` /
    /// resourceless user verbs.
    resources: Vec<String>,
}

/// Collapse an iterator of `(verb, resource)` pairs into the canonical ordered,
/// deduplicated atom list. `resource == ""` means "no resource".
///
/// Keyed by `(verb_order, keyword)` so the eight built-ins sort by their fixed
/// rank and any user-defined verbs sort alphabetically among themselves — never
/// colliding the way a rank-only key would.
fn atoms<'a>(effects: impl IntoIterator<Item = (&'a EffectVerbKind, &'a str)>) -> Vec<Atom> {
    let mut by_verb: BTreeMap<
        (usize, String),
        (EffectVerbKind, std::collections::BTreeSet<String>),
    > = BTreeMap::new();
    for (verb, resource) in effects {
        let key = (verb_order(verb), verb_keyword(verb).to_string());
        let entry = by_verb
            .entry(key)
            .or_insert_with(|| (verb.clone(), std::collections::BTreeSet::new()));
        if !resource.is_empty() {
            entry.1.insert(resource.to_string());
        }
    }
    by_verb
        .into_values()
        .map(|(verb, resources)| Atom {
            verb,
            resources: resources.into_iter().collect(),
        })
        .collect()
}

/// Render one atom as `keyword` or `keyword(res, res2)`, optionally wrapped in
/// its group color.
fn render_atom(atom: &Atom, color: bool) -> String {
    let kw = verb_keyword(&atom.verb);
    let body = if atom.resources.is_empty() {
        kw.to_string()
    } else {
        format!("{kw}({})", atom.resources.join(", "))
    };
    if color {
        let c = group_color(verb_group(&atom.verb));
        format!("{c}{body}{RESET}")
    } else {
        body
    }
}

/// Compact one-line rendering: `reads(UserDB) + sends(Network) + panics`.
/// Empty set renders as `(none)`.
pub fn render_compact<'a>(
    effects: impl IntoIterator<Item = (&'a EffectVerbKind, &'a str)>,
    color: ColorChoice,
) -> String {
    let atoms = atoms(effects);
    if atoms.is_empty() {
        return "(none)".to_string();
    }
    let color = color.enabled();
    atoms
        .iter()
        .map(|a| render_atom(a, color))
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Grouped multi-line rendering for IDE hover. One line per non-empty bucket,
/// in Resource → Execution → Panic order:
///
/// ```text
/// Resource: reads(UserDB) + writes(Cache)
/// Execution: blocks
/// Panic: panics
/// ```
///
/// Empty set renders as the single line `(pure)` — "pure" carries meaning.
pub fn render_grouped<'a>(
    effects: impl IntoIterator<Item = (&'a EffectVerbKind, &'a str)>,
    color: ColorChoice,
) -> String {
    let atoms = atoms(effects);
    if atoms.is_empty() {
        return "(pure)".to_string();
    }
    let color = color.enabled();
    // Stated group order: Resource, Execution, Panic. Empty groups omitted.
    let order = [
        (EffectGroup::Resource, "Resource"),
        (EffectGroup::Execution, "Execution"),
        (EffectGroup::Panic, "Panic"),
    ];
    let mut lines = Vec::new();
    for (group, label) in order {
        let rendered: Vec<String> = atoms
            .iter()
            .filter(|a| verb_group(&a.verb) == group)
            .map(|a| render_atom(a, color))
            .collect();
        if !rendered.is_empty() {
            lines.push(format!("{label}: {}", rendered.join(" + ")));
        }
    }
    lines.join("\n")
}

/// The structured root: a canonically-ordered array of
/// `{ "verb", "resource"|null, "group" }`. Never colored. `--output=json`
/// clients render from this; the compact/grouped strings are cosmetic views of
/// the same data.
pub fn effects_json<'a>(
    effects: impl IntoIterator<Item = (&'a EffectVerbKind, &'a str)>,
) -> serde_json::Value {
    let mut out = Vec::new();
    for atom in atoms(effects) {
        let group = match verb_group(&atom.verb) {
            EffectGroup::Resource => "resource",
            EffectGroup::Execution => "execution",
            EffectGroup::Panic => "panic",
        };
        if atom.resources.is_empty() {
            out.push(serde_json::json!({
                "verb": verb_keyword(&atom.verb),
                "resource": serde_json::Value::Null,
                "group": group,
            }));
        } else {
            // One entry per resource keeps the JSON flat and diffable.
            for r in &atom.resources {
                out.push(serde_json::json!({
                    "verb": verb_keyword(&atom.verb),
                    "resource": r,
                    "group": group,
                }));
            }
        }
    }
    serde_json::Value::Array(out)
}

// ── EffectSet convenience wrappers ──────────────────────────────
//
// The compiler's own effect representation is `effectchecker::EffectSet`
// (a `Vec<TracedEffect>`); these adapt it to the iterator API above so the
// query path and crash renderer can hand a set straight in.

/// Iterate an `EffectSet` as `(verb, resource)` pairs.
fn set_pairs(
    set: &crate::effectchecker::EffectSet,
) -> impl Iterator<Item = (&EffectVerbKind, &str)> {
    set.effects
        .iter()
        .map(|t| (&t.effect.verb, t.effect.resource.as_str()))
}

/// [`render_compact`] over an [`EffectSet`](crate::effectchecker::EffectSet).
pub fn compact_of_set(set: &crate::effectchecker::EffectSet, color: ColorChoice) -> String {
    render_compact(set_pairs(set), color)
}

/// [`render_grouped`] over an [`EffectSet`](crate::effectchecker::EffectSet).
pub fn grouped_of_set(set: &crate::effectchecker::EffectSet, color: ColorChoice) -> String {
    render_grouped(set_pairs(set), color)
}

/// [`effects_json`] over an [`EffectSet`](crate::effectchecker::EffectSet).
pub fn json_of_set(set: &crate::effectchecker::EffectSet) -> serde_json::Value {
    effects_json(set_pairs(set))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(EffectVerbKind, &str)]) -> Vec<(EffectVerbKind, String)> {
        v.iter().map(|(k, r)| (k.clone(), r.to_string())).collect()
    }
    fn refs(v: &[(EffectVerbKind, String)]) -> Vec<(&EffectVerbKind, &str)> {
        v.iter().map(|(k, r)| (k, r.as_str())).collect()
    }

    #[test]
    fn compact_canonical_order_and_dedup() {
        // Deliberately out of order + duplicate reads; expect canonical order,
        // merged resources, sorted within a verb.
        let v = pairs(&[
            (EffectVerbKind::Sends, "Network"),
            (EffectVerbKind::Reads, "UserDB"),
            (EffectVerbKind::Reads, "Cache"),
            (EffectVerbKind::Reads, "UserDB"),
            (EffectVerbKind::Panics, ""),
        ]);
        let out = render_compact(refs(&v), ColorChoice::Never);
        assert_eq!(out, "reads(Cache, UserDB) + sends(Network) + panics");
    }

    #[test]
    fn compact_is_order_independent() {
        let a = pairs(&[
            (EffectVerbKind::Writes, "Db"),
            (EffectVerbKind::Reads, "Env"),
        ]);
        let b = pairs(&[
            (EffectVerbKind::Reads, "Env"),
            (EffectVerbKind::Writes, "Db"),
        ]);
        assert_eq!(
            render_compact(refs(&a), ColorChoice::Never),
            render_compact(refs(&b), ColorChoice::Never),
        );
    }

    #[test]
    fn empty_renderings() {
        let empty: Vec<(EffectVerbKind, String)> = vec![];
        assert_eq!(render_compact(refs(&empty), ColorChoice::Never), "(none)");
        assert_eq!(render_grouped(refs(&empty), ColorChoice::Never), "(pure)");
        assert_eq!(effects_json(refs(&empty)), serde_json::json!([]));
    }

    #[test]
    fn grouped_buckets_and_omits_empty() {
        let v = pairs(&[
            (EffectVerbKind::Reads, "UserDB"),
            (EffectVerbKind::Writes, "Cache"),
            (EffectVerbKind::Blocks, ""),
            (EffectVerbKind::Panics, ""),
        ]);
        let out = render_grouped(refs(&v), ColorChoice::Never);
        assert_eq!(
            out,
            "Resource: reads(UserDB) + writes(Cache)\nExecution: blocks\nPanic: panics"
        );
    }

    #[test]
    fn grouped_single_group_no_extra_lines() {
        let v = pairs(&[(EffectVerbKind::Reads, "A")]);
        let out = render_grouped(refs(&v), ColorChoice::Never);
        assert_eq!(out, "Resource: reads(A)"); // no Execution/Panic lines
    }

    #[test]
    fn user_defined_verbs_do_not_collide() {
        // Two distinct user verbs both rank last; must both appear, sorted.
        let v = pairs(&[
            (EffectVerbKind::UserDefined("logs".into()), "Sink"),
            (EffectVerbKind::UserDefined("audits".into()), "Trail"),
            (EffectVerbKind::Reads, "Db"),
        ]);
        let out = render_compact(refs(&v), ColorChoice::Never);
        assert_eq!(out, "reads(Db) + audits(Trail) + logs(Sink)");
    }

    #[test]
    fn color_wraps_by_group_and_never_is_bare() {
        let v = pairs(&[
            (EffectVerbKind::Reads, "Db"),
            (EffectVerbKind::Blocks, ""),
            (EffectVerbKind::Panics, ""),
        ]);
        let colored = render_compact(refs(&v), ColorChoice::Always);
        assert!(colored.contains(CYAN), "resource verb should be cyan");
        assert!(colored.contains(YELLOW), "execution verb should be yellow");
        assert!(colored.contains(RED), "panic should be red");
        assert!(colored.contains(RESET));
        // Never must be free of any escape.
        let bare = render_compact(refs(&v), ColorChoice::Never);
        assert!(!bare.contains('\x1b'));
    }

    #[test]
    fn json_shape_is_structured_and_uncolored() {
        let v = pairs(&[
            (EffectVerbKind::Reads, "A"),
            (EffectVerbKind::Reads, "B"),
            (EffectVerbKind::Suspends, ""),
        ]);
        let j = effects_json(refs(&v));
        assert_eq!(
            j,
            serde_json::json!([
                {"verb": "reads", "resource": "A", "group": "resource"},
                {"verb": "reads", "resource": "B", "group": "resource"},
                {"verb": "suspends", "resource": serde_json::Value::Null, "group": "execution"},
            ])
        );
        // Even under Always, JSON strings never carry escapes.
        assert!(!j.to_string().contains('\x1b'));
    }
}
