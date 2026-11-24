//! `dollar_interpolation`: `$name` or `${expr}` in a plain string literal
//! where `name`, or every name in `expr`, is a binding in scope. Kotlin,
//! Dart, Groovy and the shells interpolate there; Kāra interpolates only in
//! an f-string, so `"total: $n"` is valid Kāra that prints `$n` itself, the
//! silent kind of wrong (B-2026-11-24-47, cells k007, k008, k061).
//!
//! The in-scope test is what keeps the lint precise: `"$5"`, `"$HOME"` and
//! `"$name"` with no binding `name` are left alone, and so is a raw
//! `r"..."` literal, where a regex's `$` lives. The fix writes the literal
//! as an f-string from its own source text, so its escapes are kept and its
//! literal braces doubled; a `$` that names no binding stays as text.

use super::{FixIt, TypeErrorKind};
use crate::token::Span;

/// The f-string body for the plain literal body `body`, with every `$name`
/// and `${expr}` whose names `bound` accepts turned into a `{..}` hole, and
/// the holes made; `None` when no `$` names a binding.
fn interpolated(body: &str, bound: &dyn Fn(&str) -> bool) -> Option<(String, Vec<String>)> {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::new();
    let mut holes = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                // An escape is copied whole, `\u{..}` with its braces.
                out.push(c);
                i += 1;
                if i < chars.len() {
                    out.push(chars[i]);
                    if chars[i] == 'u' && chars.get(i + 1) == Some(&'{') {
                        i += 1;
                        while i < chars.len() && chars[i] != '}' {
                            out.push(chars[i]);
                            i += 1;
                        }
                        if i < chars.len() {
                            out.push('}');
                        }
                    }
                    i += 1;
                }
            }
            '{' | '}' => {
                out.push(c);
                out.push(c);
                i += 1;
            }
            '$' if chars.get(i + 1) == Some(&'{') => {
                let mut depth = 0;
                let mut j = i + 1;
                let mut close = None;
                while j < chars.len() {
                    match chars[j] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(j);
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let inner: Option<String> = close.map(|j| chars[i + 2..j].iter().collect());
                match inner.filter(|e| expr_names_bound(e, bound)) {
                    Some(e) => {
                        out.push('{');
                        out.push_str(e.trim());
                        out.push('}');
                        holes.push(format!("${{{e}}}"));
                        i = close.unwrap_or(i) + 1;
                    }
                    None => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            '$' if chars
                .get(i + 1)
                .is_some_and(|c| c.is_alphabetic() || *c == '_') =>
            {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let name: String = chars[i + 1..j].iter().collect();
                if name != "_" && bound(&name) {
                    out.push('{');
                    out.push_str(&name);
                    out.push('}');
                    holes.push(format!("${name}"));
                    i = j;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    (!holes.is_empty()).then_some((out, holes))
}

/// Whether `expr`, the text of a `${..}`, names at least one binding and
/// nothing that is not one. A name after `.` is a field or method and is
/// not asked about; a quote or backslash, which an f-string hole cannot
/// hold as written, refuses it.
fn expr_names_bound(expr: &str, bound: &dyn Fn(&str) -> bool) -> bool {
    if expr.trim().is_empty() || expr.contains(['"', '\'', '\\', '{', '}']) {
        return false;
    }
    let chars: Vec<char> = expr.chars().collect();
    let mut names = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_alphanumeric() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let after_dot = chars[..i]
                .iter()
                .rev()
                .find(|c| !c.is_whitespace())
                .is_some_and(|c| *c == '.');
            if !c.is_ascii_digit() && !after_dot {
                if !bound(&word) {
                    return false;
                }
                names += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    names > 0
}

impl<'a> super::TypeChecker<'a> {
    /// The `dollar_interpolation` lint over one plain string literal (see
    /// the module documentation).
    pub(super) fn lint_dollar_interpolation(&mut self, value: &str, span: Span) {
        if !value.contains('$') {
            return;
        }
        let Some(text) = self.program.dollar_literals.get(&span.offset) else {
            return;
        };
        if text.len() != span.length || text.len() < 2 {
            return;
        }
        let body = &text[1..text.len() - 1];
        let env = &self.local_scope;
        let Some((fbody, holes)) = interpolated(body, &|n| env.lookup(n).is_some()) else {
            return;
        };
        let first = &holes[0];
        let name = first.trim_start_matches('$');
        let name = name
            .strip_prefix('{')
            .and_then(|n| n.strip_suffix('}'))
            .unwrap_or(name)
            .trim();
        let more = if holes.len() > 1 {
            format!(" (and {} more)", holes.len() - 1)
        } else {
            String::new()
        };
        self.type_lint_warning_with_fix(
            format!(
                "`{first}`{more} in a plain string is the text `{first}`, not the value of \
                 `{name}`: Kāra interpolates only in an f-string, so write \
                 `f\"...{{{name}}}...\"` (Kotlin, Dart and the shells interpolate `$name`; \
                 Kāra does not)"
            ),
            span,
            TypeErrorKind::StyleLint,
            "dollar_interpolation",
            Some(FixIt {
                span,
                replacement: format!("f\"{fbody}\""),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(body: &str, names: &[&str]) -> Option<String> {
        interpolated(body, &|n| names.contains(&n)).map(|(b, _)| b)
    }

    #[test]
    fn dollar_names_a_binding() {
        assert_eq!(run("n = $n", &["n"]).as_deref(), Some("n = {n}"));
        assert_eq!(run("$user.name", &["user"]).as_deref(), Some("{user}.name"));
        assert_eq!(
            run("${a + b.len()} {x}", &["a", "b"]).as_deref(),
            Some("{a + b.len()} {{x}}")
        );
        assert_eq!(
            run("\\u{24}$n\\n", &["n"]).as_deref(),
            Some("\\u{24}{n}\\n")
        );
    }

    #[test]
    fn dollar_names_nothing_bound() {
        assert_eq!(run("$5 and $HOME", &["n"]), None);
        assert_eq!(run("${f(n)}", &["n"]), None);
        assert_eq!(run("^a+$", &["a"]), None);
        assert_eq!(run("$_", &["_"]), None);
    }
}
