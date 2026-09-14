//! A `mono_state` map keyed on `compile_generic_call`'s `mangled` name must be
//! written AFTER the final `mangled` binding (B-2026-09-12-3).
//!
//! # Why this class exists
//!
//! `compile_generic_call` rebinds `mangled` five times in sequence — the base
//! `mangle_mono_name`, then four `append_*` axes (handle, collection,
//! structural, nested instantiation). `compile_mono_function` looks a monomorph
//! up under the LAST of them, because that is the name the function was
//! declared with.
//!
//! So a side-channel record written at an earlier binding is keyed by a name
//! nothing ever reads. That failure is completely silent: no verifier error, no
//! link failure, no missing symbol — just a prologue registration that does not
//! happen, which is indistinguishable from the channel being empty.
//!
//! `mono_handle_param_infos` sat at binding 2 for months and was correct only
//! as a conjunction of three separate "this append does not apply to a
//! Column/Tensor argument" facts, none of them stated near the write. It was
//! moved down by B-2026-09-12-3; its array twin `mono_array_param_tes` was
//! already there, with a comment saying why. This guard is what keeps a third
//! from being added at the wrong binding, since nothing about doing so is
//! visible at the point of writing it.
//!
//! A source scan rather than a behavioural test on purpose: the defect is
//! LATENT — no currently-reachable program makes appends 3–5 non-trivial for an
//! argument that also populates `handle_params`, so there is nothing to observe
//! at runtime. What can be checked is the ordering invariant itself.

/// Every `self.mono_state.<map>.insert(mangled…)` inside `compile_generic_call`
/// comes after the last `let mangled = self.append_…` rebinding.
#[test]
fn mono_state_maps_keyed_on_mangled_are_written_after_the_final_binding() {
    const MONO_SRC: &str = include_str!("../src/codegen/mono.rs");

    // `compile_generic_call`'s body: from its `fn` line to the next
    // same-indentation `fn`, which is enough to bound the scan without parsing.
    let start = MONO_SRC
        .find("fn compile_generic_call")
        .expect("compile_generic_call moved or was renamed — update this guard, do not delete it");
    let rest = &MONO_SRC[start..];
    let end = rest[1..]
        .find("\n    fn ")
        .or_else(|| rest[1..].find("\n    pub(super) fn "))
        .or_else(|| rest[1..].find("\n    pub(crate) fn "))
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    let body = &rest[..end];

    let last_rebind = body
        .rfind("let mangled = self.append_")
        .expect("the `append_*` mangle chain moved — update this guard, do not delete it");

    // Writes are spelled across lines by rustfmt:
    //     self.mono_state
    //         .mono_array_param_tes
    //         .insert(mangled.clone(), array_params);
    // so find each `.insert(mangled` and walk back to the map name.
    let mut offenders: Vec<(String, usize)> = Vec::new();
    let mut at = 0usize;
    while let Some(i) = body[at..].find(".insert(mangled") {
        let pos = at + i;
        at = pos + 1;
        if pos > last_rebind {
            continue;
        }
        let before = &body[..pos];
        if !before.contains("mono_state") {
            continue;
        }
        // The map name is the identifier immediately preceding `.insert(`.
        let name: String = before
            .trim_end()
            .trim_end_matches(|c: char| c.is_whitespace())
            .rsplit('.')
            .next()
            .unwrap_or("<unknown>")
            .trim()
            .to_string();
        offenders.push((name, pos));
    }

    assert!(
        offenders.is_empty(),
        "these `mono_state` maps are keyed on `mangled` but written BEFORE the \
         final `let mangled = self.append_…` rebinding, so they are populated \
         under a name `compile_mono_function` never looks up: {:?}\n\n\
         Move the write below the last `append_*` binding, beside \
         `mono_array_param_tes`. The record's CONTENT does not depend on the \
         later appends — only its KEY does — so the move is mechanical. \
         B-2026-09-12-3 is what this looks like when it is wrong: silent, with \
         no verifier error and no link failure.",
        offenders
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
    );

    // The scan must actually be looking at something.
    assert!(
        body[last_rebind..].contains(".insert(mangled"),
        "no `mangled`-keyed `mono_state` write was found after the final \
         binding — the scan patterns no longer match `compile_generic_call` \
         and this guard needs updating, not deleting"
    );
}
