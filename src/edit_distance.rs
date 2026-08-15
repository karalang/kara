//! Edit-distance helpers shared across diagnostic emitters.
//!
//! Used by the resolver for `did you mean` corrections on undefined names /
//! types, and by the typechecker for `no method named ... did you mean ...`
//! suggestions. Originally lived in `resolver.rs`; promoted to its own module
//! when method-resolution diagnostics needed access to the same logic.

pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut matrix = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in matrix.iter_mut().enumerate().take(m + 1) {
        row[0] = i;
    }
    #[allow(clippy::needless_range_loop)]
    for j in 0..=n {
        matrix[0][j] = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
        }
    }
    matrix[m][n]
}

/// Return the closest candidate (edit distance ≤ 2) to `name`. Returns
/// `None` when `name` is shorter than 3 characters or no candidate is
/// within tolerance.
///
/// **`visible`'s ORDER IS THE TIE-BREAK, so callers must pass a
/// deterministically-ordered slice** (B-2026-08-14-34 leg A). Among candidates
/// at EQUAL distance the earliest wins, and that is deliberate rather than
/// incidental: the resolver builds `visible` by walking scopes inner-to-outer
/// (`SymbolTable::visible_names`), so first-wins means a NEARER binding beats a
/// farther one — the right answer, and one a global sort here would destroy.
///
/// What was missing is the second half of the rule. Where a caller's order
/// carries no proximity signal — the names within one scope, the methods on one
/// impl, the siblings under one module — it used to come straight off a
/// `HashMap`, so a genuine tie resolved by per-process hash order: `Str` against
/// `Shr` / `CStr` / `ptr` (all distance 1) picked a different one on 7 / 7 / 6
/// of 20 runs of the SAME binary, and the winner is also written into a
/// machine-applicable `TextEdit` that `karac fix` applies. Each producer now
/// sorts its own contribution, so the full rule is "nearest scope, then
/// alphabetical".
pub fn suggest_similar(name: &str, visible: &[&str]) -> Option<String> {
    if name.len() < 3 {
        return None;
    }
    let mut best: Option<(&str, usize)> = None;
    for &candidate in visible {
        if candidate == name {
            continue;
        }
        let dist = levenshtein_distance(name, candidate);
        if dist <= 2 {
            match best {
                None => best = Some((candidate, dist)),
                Some((_, best_dist)) if dist < best_dist => best = Some((candidate, dist)),
                _ => {}
            }
        }
    }
    best.map(|(s, _)| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B-2026-08-14-34 leg A — pin the tie-break rule the doc comment states,
    /// so a future refactor cannot quietly change it back to "whatever the
    /// candidate collection yielded".
    #[test]
    fn equal_distance_tie_goes_to_the_earliest_candidate() {
        // All three are distance 1 from "str". First wins, both directions —
        // asserting only one order would pass under a rule that sorted
        // internally, which is exactly the rule this must NOT have (a global
        // sort here would discard the resolver's inner-scope-first ordering).
        assert_eq!(
            suggest_similar("str", &["ztr", "atr", "mtr"]).as_deref(),
            Some("ztr")
        );
        assert_eq!(
            suggest_similar("str", &["atr", "ztr", "mtr"]).as_deref(),
            Some("atr")
        );
        // A strictly nearer candidate still beats an earlier one.
        assert_eq!(
            suggest_similar("str", &["sxyr", "atr"]).as_deref(),
            Some("atr")
        );
    }
}
