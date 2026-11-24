//! JavaScript's, Java's and C#'s method names, told where the receiver's
//! type is known (B-2026-11-24-8): `s.toUpperCase()`, `v.includes(x)`,
//! `list.add(x)`, `map.put(k, v)`, `a.equals(b)`. A name whose Kāra twin
//! takes the same arguments and returns the same value is renamed; one
//! whose twin returns something else (`charAt` is a `char`, Kāra's
//! `char_at` an `Option[char]`; `indexOf` is `-1` when absent) is told,
//! with no edit, so no spelling fix can change what the program computes.

use super::{FixIt, TypeChecker, TypeErrorKind};
use crate::ast::{CallArg, Expr};
use crate::formatter::render_expr_one_line;
use crate::token::Span;

impl TypeChecker<'_> {
    /// Rewrites the "no method" error a foreign method name drew: its
    /// message names the Kāra form, and a rename carries its edit.
    pub(super) fn foreign_method_twin(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        args_close_span: &Span,
        errors_before: usize,
    ) {
        let head = format!("no method '{method}' on type '");
        let Some(k) = (errors_before..self.errors.len()).find(|&k| {
            matches!(self.errors[k].kind, TypeErrorKind::NoMethodFound)
                && self.errors[k].message.starts_with(&head)
        }) else {
            return;
        };
        let ty = self.errors[k].message[head.len()..]
            .split('\'')
            .next()
            .unwrap_or_default()
            .to_string();
        let texty = matches!(ty.as_str(), "String" | "Str" | "StringSlice" | "str");
        let listy = matches!(ty.as_str(), "Vec" | "VecDeque" | "Slice" | "Array");
        let mappy = matches!(ty.as_str(), "Map" | "SortedMap");
        let setty = matches!(ty.as_str(), "Set" | "SortedSet");
        let lang = if method == "empty" {
            "C++'s"
        } else if matches!(
            method,
            "uppercase" | "uppercased" | "lowercase" | "lowercased"
        ) {
            "Kotlin's and Swift's"
        } else {
            "JavaScript's and Java's"
        };
        let (to, note): (Option<&str>, String) = match method {
            "toUpperCase" if texty => (Some("to_uppercase"), String::new()),
            "toLowerCase" if texty => (Some("to_lowercase"), String::new()),
            "uppercase" | "uppercased" if texty && args.is_empty() => {
                (Some("to_uppercase"), String::new())
            }
            "lowercase" | "lowercased" if texty && args.is_empty() => {
                (Some("to_lowercase"), String::new())
            }
            "startsWith" if texty => (Some("starts_with"), String::new()),
            "endsWith" if texty => (Some("ends_with"), String::new()),
            "includes" if texty || listy || setty => (Some("contains"), String::new()),
            "isEmpty" if texty || listy || mappy || setty => (Some("is_empty"), String::new()),
            "size" if listy || mappy || setty => (Some("len"), String::new()),
            "containsKey" if mappy => (Some("contains_key"), String::new()),
            // C++'s `empty()` asks, as `is_empty` does; it never clears.
            "empty" if (texty || listy || mappy || setty) && args.is_empty() => {
                (Some("is_empty"), String::new())
            }
            "back" | "front" if listy && args.is_empty() => (
                None,
                format!(
                    "`v.{method}()` is C++'s {} element, undefined on an empty vector: Kāra's \
                     `v.{}()` is an `Option`, `None` when it is empty, so it is \
                     `v.{}().unwrap()` where `v` is known to hold one",
                    if method == "back" { "last" } else { "first" },
                    if method == "back" { "last" } else { "first" },
                    if method == "back" { "last" } else { "first" },
                ),
            ),
            // Java's `List.add(x)` appends, as `push` does; `Set.add(x)`
            // and `insert` both say whether the value was new.
            "add" if listy && args.len() == 1 => (Some("push"), String::new()),
            "add" if setty => (Some("insert"), String::new()),
            // Java's `put` returns the value it replaced, or null: Kāra's
            // `insert` returns it as an `Option`.
            "put" if mappy => (Some("insert"), String::new()),
            "charAt" if texty => (
                None,
                format!(
                    "`charAt(i)` is {lang}, and is the character itself; Kāra's \
                     `s.char_at(i)` is an `Option[char]`, `None` past the end, so it is \
                     `s.char_at(i).unwrap()` where `i` is known to be in range \
                     (library/strings.md)"
                ),
            ),
            "indexOf" if texty || listy => (
                None,
                format!(
                    "`indexOf` is {lang}, and is -1 when the value is absent; Kāra's \
                     `v.iter().position(|x| x == y)` (and `s.find(p)` for text) is an \
                     `Option[i64]`, `None` when it is absent"
                ),
            ),
            // A list's and a set's `forEach(f)` passes each value, as
            // Kāra's `for_each` does; a map's passes the value and the key.
            "forEach" if (listy || setty) && args.len() == 1 => {
                (Some("iter().for_each"), String::new())
            }
            "forEach" if mappy => (
                None,
                format!(
                    "`forEach` is {lang}: Kāra loops with `for x in v {{ .. }}`, or calls \
                     `v.iter().for_each(f)`"
                ),
            ),
            "getOrDefault" if mappy => (
                None,
                "`getOrDefault(k, d)` is Java's: Kāra's `m.get(k)` is an `Option`, and \
                 `m.get(k) ?? d` supplies the default"
                    .to_string(),
            ),
            "equals" if args.len() == 1 => {
                let a = render_expr_one_line(object);
                let b = render_expr_one_line(&args[0].value);
                let end = args_close_span.offset + args_close_span.length;
                let message = format!(
                    "no method 'equals' on type '{ty}': `a.equals(b)` is Java's value \
                     comparison: Kāra's `==` compares values, `a == b`"
                );
                self.errors[k].message = message;
                self.errors[k].fix_it = Some(FixIt {
                    span: Span {
                        offset: object.span.offset,
                        length: end.saturating_sub(object.span.offset),
                        ..object.span
                    },
                    replacement: format!("({a} == {b})"),
                });
                return;
            }
            _ => return,
        };
        // A message that already names the Kāra form, or carries an edit,
        // is the checker's own and stays (`size` -> "did you mean 'len'?",
        // `map` on a `Vec` -> the `.iter()` hop and its fix).
        if self.errors[k].fix_it.is_some()
            || to.is_some_and(|t| {
                self.errors[k]
                    .message
                    .contains(&format!("did you mean '{t}'"))
            })
        {
            return;
        }
        match to {
            Some(to) => {
                self.errors[k].message = format!(
                    "no method '{method}' on type '{ty}': `{method}` is {lang}; Kāra's is \
                     `{to}`"
                );
                // The name sits between the receiver and the `(`.
                let from = object.span.offset + object.span.length;
                let upto = args
                    .first()
                    .map_or(args_close_span.offset, |a| a.value.span.offset);
                self.errors[k].fix_it = (upto == from + method.len() + 2).then(|| FixIt {
                    span: Span {
                        offset: from,
                        length: upto - from,
                        ..object.span
                    },
                    replacement: format!(".{to}("),
                });
            }
            None => {
                self.errors[k].message = format!("no method '{method}' on type '{ty}': {note}");
                self.errors[k].fix_it = None;
            }
        }
    }
}
