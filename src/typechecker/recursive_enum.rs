//! design.md § Recursive enums require `shared enum`: a plain enum whose
//! payload holds the same enum inline has no finite size. The direct case
//! (`Cons(i64, L)`) is the ownership checker's E0501. Through an inline
//! wrapper (`Option[L]`, `Result[L, E]`, `Array[L, N]`, or a tuple around one
//! of them) 0.8.x has accepted it, giving the payload an indirection of its
//! own, so it is a `deprecated` warning here with the direct case's fix, and
//! an error in the next minor release (B-2026-11-19-11, tracker
//! B-2026-11-19-39).

use std::collections::{HashMap, HashSet};

use super::types::{Type, VariantTypeInfo};
use super::{FixIt, Span, TypeErrorKind};
use crate::ast::Item;

/// The plain types `ty` holds inline, each with the wrapper it is reached
/// through (`None` when directly or through tuples only, which the
/// ownership checker already refuses).
fn inline_targets(
    ty: &Type,
    wrapper: Option<&'static str>,
    out: &mut Vec<(String, Option<&'static str>)>,
) {
    match ty {
        Type::Tuple(ts) => {
            for t in ts {
                inline_targets(t, wrapper, out);
            }
        }
        Type::Array { element, .. } => inline_targets(element, wrapper.or(Some("Array")), out),
        Type::Named { name, args } if name == "Option" || name == "Result" => {
            let w = if name == "Option" { "Option" } else { "Result" };
            for a in args {
                inline_targets(a, wrapper.or(Some(w)), out);
            }
        }
        Type::Named { name, .. } => out.push((name.clone(), wrapper)),
        _ => {}
    }
}

impl<'a> super::TypeChecker<'a> {
    /// Warns on each plain user enum that holds itself inline through a
    /// wrapper, with the fix `shared enum`.
    pub(super) fn warn_recursive_enums_through_wrappers(&mut self) {
        if self.compiling_stdlib {
            return;
        }
        // Plain types only: a `shared` (or `par`) type is a handle, so
        // holding one is no inline containment, and none holds itself.
        let mut graph: HashMap<String, Vec<(String, Option<&'static str>)>> = HashMap::new();
        for (name, info) in &self.env.enums {
            if info.is_shared || info.is_par {
                continue;
            }
            let mut edges = Vec::new();
            for (_, v) in &info.variants {
                match v {
                    VariantTypeInfo::Unit => {}
                    VariantTypeInfo::Tuple(tys) => {
                        for t in tys {
                            inline_targets(t, None, &mut edges);
                        }
                    }
                    VariantTypeInfo::Struct(fs) => {
                        for (_, t) in fs {
                            inline_targets(t, None, &mut edges);
                        }
                    }
                }
            }
            graph.insert(name.clone(), edges);
        }
        for (name, info) in &self.env.structs {
            if info.is_shared || info.is_par {
                continue;
            }
            let mut edges = Vec::new();
            for (_, t, _) in &info.fields {
                inline_targets(t, None, &mut edges);
            }
            graph.insert(name.clone(), edges);
        }
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let mut found = Vec::new();
        for item in &self.program.items {
            let Item::EnumDef(e) = item else { continue };
            if e.is_shared || e.is_par || e.stdlib_origin || e.span.offset >= library {
                continue;
            }
            // Search (type, first wrapper on the way) states from the enum.
            let mut seen: HashSet<(String, Option<&'static str>)> = HashSet::new();
            let mut todo: Vec<(String, Option<&'static str>)> = vec![(e.name.clone(), None)];
            let mut direct = false;
            let mut through = None;
            while let Some((node, w)) = todo.pop() {
                for (next, ew) in graph.get(&node).into_iter().flatten() {
                    let w = w.or(*ew);
                    if *next == e.name {
                        match w {
                            None => direct = true,
                            Some(w) => through = through.or(Some(w)),
                        }
                        continue;
                    }
                    if graph.contains_key(next) && seen.insert((next.clone(), w)) {
                        todo.push((next.clone(), w));
                    }
                }
            }
            // A direct cycle is E0501 already.
            let (false, Some(wrapper)) = (direct, through) else {
                continue;
            };
            let name = &e.name;
            let message = format!(
                "enum '{name}' holds itself inline through `{wrapper}`: a plain enum's payload \
                 is stored in place, so this is a recursive enum without indirection \
                 (design.md § Recursive enums require `shared enum`). 0.8.x still accepts it; \
                 this is deprecated and becomes an error in the next minor release. Declare it \
                 `shared enum {name}`, whose recursive fields are counted handles"
            );
            let at = Span {
                offset: e.span.offset,
                length: 0,
                line: e.span.line,
                column: e.span.column,
            };
            let fix = FixIt {
                span: at,
                replacement: "shared ".to_string(),
            };
            found.push((message, e.span, fix, e.lint_overrides.clone()));
        }
        for (message, span, fix, lints) in found {
            self.lint_override_stack.push(lints);
            self.type_lint_warning_with_fix(
                message,
                span,
                TypeErrorKind::Deprecated,
                "deprecated",
                Some(fix),
            );
            self.lint_override_stack.pop();
        }
    }
}
