//! The library modules a program imports before it uses their items:
//! `std.regex` and `std.json` (docs/library/README.md; design.md, "everything
//! else is under `std.*`"; B-2026-10-21-56). Their items stay globals of the
//! implementation, so `import std.regex.{Regex}` binds nothing new (the
//! import is dropped as a [`crate::prelude::LIBRARY_GLOBAL_MODULES`] entry);
//! what the import changes is whether `karac check` accepts the use. A bare
//! use is an error whose fix-it adds the import.

use crate::ast::Item;
use crate::token::{Span, Token};
use crate::typechecker::{FixIt, TypeError, TypeErrorKind};
use std::collections::{BTreeSet, HashSet};

/// Each module, and the items a program names only through its import.
pub const IMPORT_REQUIRED_MODULES: &[(&str, &[&str])] = &[
    ("std.regex", &["Regex", "Match", "Captures", "RegexError"]),
    ("std.json", &["Json", "JsonError"]),
];

/// Whether `name` is an item of an [`IMPORT_REQUIRED_MODULES`] module, so a
/// program's own type of that name shadows nothing it can see unimported.
pub fn is_import_required(name: &str) -> bool {
    IMPORT_REQUIRED_MODULES
        .iter()
        .any(|(_, names)| names.contains(&name))
}

/// An error for each module in [`IMPORT_REQUIRED_MODULES`] whose items `src`
/// uses without importing them, at the first such use. Every error carries
/// the same fix-it, which adds all the missing imports after the program's
/// last import (or above its first item), so `karac fix` applies them in one
/// pass. A program's own item or enum variant of the same name is not a use,
/// and neither is a field or method of that name.
pub fn missing_library_imports(src: &str) -> Vec<TypeError> {
    let parsed = crate::parse(src);
    let mut declared: HashSet<String> = HashSet::new();
    let mut imported: HashSet<(String, String)> = HashSet::new();
    // A variant named like an item (`enum Format { Json }`) is not a use.
    let mut variants: Vec<(usize, String)> = Vec::new();
    let mut after_imports = None;
    for item in &parsed.program.items {
        match item {
            Item::StructDef(s) => {
                declared.insert(s.name.clone());
            }
            Item::EnumDef(e) => {
                declared.insert(e.name.clone());
                variants.extend(e.variants.iter().map(|v| (v.span.offset, v.name.clone())));
            }
            Item::TraitDef(t) => {
                declared.insert(t.name.clone());
            }
            Item::TypeAlias(t) => {
                declared.insert(t.name.clone());
            }
            Item::Function(f) => {
                declared.insert(f.name.clone());
            }
            Item::Import(imp) => {
                let module = imp.path.join(".");
                if imp.is_wildcard {
                    if let Some((_, names)) =
                        IMPORT_REQUIRED_MODULES.iter().find(|(m, _)| *m == module)
                    {
                        for n in *names {
                            imported.insert((module.clone(), n.to_string()));
                        }
                    }
                }
                for ii in &imp.items {
                    imported.insert((module.clone(), ii.name.clone()));
                }
                let end = line_end(src, imp.span.offset + imp.span.length);
                after_imports = Some(after_imports.map_or(end, |a: usize| a.max(end)));
            }
            _ => {}
        }
    }
    let tokens = crate::tokenize(src);
    let variant_names: HashSet<usize> = variants
        .iter()
        .filter_map(|(at, name)| {
            tokens.iter().find_map(|t| match &t.token {
                Token::Identifier { name: n, .. } if t.span.offset >= *at && n == name => {
                    Some(t.span.offset)
                }
                _ => None,
            })
        })
        .collect();
    // Each module, its first use (name and span), and every item used.
    let mut missing: Vec<(&str, &str, Span, BTreeSet<&str>)> = Vec::new();
    let mut in_import = false;
    for (i, t) in tokens.iter().enumerate() {
        match &t.token {
            Token::Import => in_import = true,
            Token::Semicolon => in_import = false,
            Token::Identifier { name, raw: false } if !in_import => {
                if i > 0 && tokens[i - 1].token == Token::Dot
                    || declared.contains(name)
                    || variant_names.contains(&t.span.offset)
                {
                    continue;
                }
                let Some((module, names)) = IMPORT_REQUIRED_MODULES
                    .iter()
                    .find(|(_, names)| names.contains(&name.as_str()))
                else {
                    continue;
                };
                if imported.contains(&(module.to_string(), name.clone())) {
                    continue;
                }
                let Some(name) = names.iter().find(|n| **n == name.as_str()).copied() else {
                    continue;
                };
                match missing.iter_mut().find(|(m, _, _, _)| m == module) {
                    Some((_, _, _, used)) => {
                        used.insert(name);
                    }
                    None => missing.push((module, name, t.span, BTreeSet::from([name]))),
                }
            }
            _ => {}
        }
    }
    if missing.is_empty() {
        return Vec::new();
    }
    let imports: Vec<String> = missing
        .iter()
        .map(|(module, _, _, used)| {
            let used: Vec<&str> = used.iter().copied().collect();
            format!("import {module}.{{{}}};", used.join(", "))
        })
        .collect();
    let (insert_at, replacement) = match after_imports {
        Some(at) => (at, format!("{}\n", imports.join("\n"))),
        None => (above_first_item(src), format!("{}\n\n", imports.join("\n"))),
    };
    let (line, column) = line_column(src, insert_at);
    let fix_it = FixIt {
        span: Span {
            line,
            column,
            offset: insert_at,
            length: 0,
        },
        replacement,
    };
    missing
        .into_iter()
        .zip(imports)
        .map(|((module, first, span, _), import)| TypeError {
            message: format!(
                "`{first}` is in the library module `{module}`, which a program \
                     imports before it uses it: `{import}` (docs/library/README.md)"
            ),
            span,
            kind: TypeErrorKind::RemovedInCore,
            lint_name: None,
            fix_it: Some(fix_it.clone()),
            class: None,
            expected: None,
            got: None,
        })
        .collect()
}

/// The offset just past the newline that ends the line holding `at`, or the
/// end of `src`.
fn line_end(src: &str, at: usize) -> usize {
    match src.get(at..).and_then(|rest| rest.find('\n')) {
        Some(i) => at + i + 1,
        None => src.len(),
    }
}

/// The start of the first line that is neither blank nor a `//` comment: a
/// program's opening comment stays above the imports a fix adds.
fn above_first_item(src: &str) -> usize {
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        if !(t.is_empty() || t.starts_with("//") && !t.starts_with("///")) {
            break;
        }
        at += line.len();
    }
    at.min(src.len())
}

/// The 1-based line and column of byte `at`.
fn line_column(src: &str, at: usize) -> (usize, usize) {
    let before = &src[..at.min(src.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    (line, column)
}
