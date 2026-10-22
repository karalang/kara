//! Project mode on v2 (B-2026-10-22-14): a package with several modules is
//! built to MIR as ONE source text, each module's file copied in whole and
//! in dependency order, with the imports between the package's own modules
//! blanked out and the `std` imports gathered, once each, into a header.
//!
//! Whole files rather than printed items, because the copy is exact: a line
//! of the combined text is a line of one file, so a location the program
//! reports (a fault, a `dbg`, a diagnostic) maps back to the file and line
//! the author wrote. Blanking replaces each character with spaces of the
//! same byte length and keeps every newline, so columns and byte offsets
//! inside a file survive too.
//!
//! What one flat scope cannot express is refused by name: two modules
//! declaring the same top-level name, an import that renames (`as`), an
//! import that names a module rather than items in it, and a module-level
//! inner attribute (`#![..]`) in any file but the first.

use super::*;
use crate::ast::{ImportDecl, Item};
use crate::module::ProgramTree;

/// The combined source of a package, and where each of its lines came from.
pub(super) struct Amalgam {
    pub text: String,
    /// `(first combined line, file, that file's line)`, ascending by the
    /// first field; a line maps through the last entry at or before it.
    pub lines: Vec<(usize, String, usize)>,
    /// `(first combined byte, byte length, file)` for each copied file.
    bytes: Vec<(usize, usize, String)>,
}

impl Amalgam {
    /// The file and line a line of the combined text came from.
    pub fn locate(&self, line: usize) -> Option<(&str, usize)> {
        let i = self.lines.partition_point(|(first, _, _)| *first <= line);
        let (first, file, file_line) = self.lines.get(i.checked_sub(1)?)?;
        Some((file, file_line + (line - first)))
    }

    /// The file and offset in it of a byte of the combined text, when that
    /// byte was copied from a file (not the import header).
    pub fn locate_offset(&self, offset: usize) -> Option<(&str, usize)> {
        self.bytes
            .iter()
            .find(|(first, len, _)| (*first..first + len).contains(&offset))
            .map(|(first, _, file)| (file.as_str(), offset - first))
    }

    /// Hand the line map to the MIR interpreter and to codegen, which report
    /// a fault's or a `dbg`'s place through it.
    pub fn install(&self) {
        crate::mir::interp::set_source_map(self.lines.clone());
    }

    /// Rewrite one rendered text diagnostic: its `name:line:col` header and
    /// its snippet's line gutter name the file and line it came from.
    pub fn remap_text(&self, name: &str, block: &str) -> String {
        let mut out = String::with_capacity(block.len());
        let mut gutter: Option<(usize, usize)> = None;
        for (i, line) in block.split_inclusive('\n').enumerate() {
            if i == 0 {
                out.push_str(&self.remap_header(name, line));
                continue;
            }
            // A snippet line: `<n> | text`, or `<pad> | carets`.
            let trimmed = line.trim_start();
            let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && trimmed[digits..].starts_with(" |") {
                let n: usize = trimmed[..digits].parse().unwrap_or(0);
                if let Some((_, l)) = self.locate(n) {
                    let indent = &line[..line.len() - trimmed.len()];
                    gutter = Some((digits, l.to_string().len()));
                    out.push_str(indent);
                    out.push_str(&l.to_string());
                    out.push_str(&trimmed[digits..]);
                    continue;
                }
            }
            match gutter {
                // Re-pad the bare gutter rows to the new number's width.
                Some((old, new)) if line.starts_with(&" ".repeat(old + 1)) && old != new => {
                    out.push_str(&" ".repeat(new));
                    out.push_str(&line[old..]);
                }
                _ => out.push_str(line),
            }
        }
        out
    }

    /// `… name:L:C: …` → `… file:l:C: …`, for the first such place.
    fn remap_header(&self, name: &str, line: &str) -> String {
        let needle = format!("{name}:");
        let Some(at) = line.find(&needle) else {
            return line.to_string();
        };
        let rest = &line[at + needle.len()..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let Some((file, l)) = rest[..digits]
            .parse()
            .ok()
            .and_then(|n: usize| self.locate(n))
        else {
            return line.to_string();
        };
        format!("{}{file}:{l}{}", &line[..at], &rest[digits..])
    }

    /// Rewrite a JSON value's locations in place: an object whose `file` is
    /// `name` takes the file and line its `line` came from, and every
    /// `offset` names a byte of that file.
    pub fn remap_json(&self, name: &str, v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                if map.get("file").and_then(|f| f.as_str()) == Some(name) {
                    if let Some((file, l)) = map
                        .get("line")
                        .and_then(|l| l.as_u64())
                        .and_then(|l| self.locate(l as usize))
                    {
                        let file = file.to_string();
                        map.insert("file".into(), file.into());
                        map.insert("line".into(), l.into());
                    }
                }
                if let Some(off) = map.get("offset").and_then(|o| o.as_u64()) {
                    if let Some((_, o)) = self.locate_offset(off as usize) {
                        map.insert("offset".into(), o.into());
                    }
                }
                for (_, child) in map.iter_mut() {
                    self.remap_json(name, child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    self.remap_json(name, item);
                }
            }
            _ => {}
        }
    }

    /// One rendered JSON diagnostic, remapped; the text unchanged when it
    /// does not parse.
    pub fn remap_json_text(&self, name: &str, text: &str) -> String {
        match serde_json::from_str::<serde_json::Value>(text) {
            Ok(mut v) => {
                self.remap_json(name, &mut v);
                v.to_string()
            }
            Err(_) => text.to_string(),
        }
    }
}

/// A MIR build error's places (`L:C in `f``) named by file, when the program
/// is a package's combined source; unchanged otherwise.
pub(super) fn remap_build_error(e: &str) -> String {
    if !crate::mir::interp::has_source_map() {
        return e.to_string();
    }
    let mut out = String::with_capacity(e.len());
    let mut rest = e;
    while let Some(at) = rest.find(" in `") {
        let head = &rest[..at];
        // The `L:C` just before ` in `.
        let start = head
            .rfind(|c: char| !(c.is_ascii_digit() || c == ':'))
            .map_or(0, |i| i + 1);
        let place = &head[start..];
        match place
            .split_once(':')
            .and_then(|(l, c)| Some((l.parse::<usize>().ok()?, c.parse::<usize>().ok()?)))
        {
            Some((line, col)) => {
                let (file, l) = crate::mir::interp::source_at(line);
                out.push_str(&head[..start]);
                out.push_str(&format!("{file}:{l}:{col}"));
            }
            None => out.push_str(head),
        }
        out.push_str(" in `");
        rest = &rest[at + 5..];
    }
    out.push_str(rest);
    out
}

/// One change to a file's text before it is copied: `start..end` becomes
/// `text`. Edits never overlap.
pub(super) struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// `start..end` of `src` as spaces, keeping its newlines and byte length.
pub(super) fn blank(src: &str, start: usize, end: usize) -> Edit {
    let text = src[start..end]
        .chars()
        .map(|c| {
            if c == '\n' {
                "\n".to_string()
            } else {
                " ".repeat(c.len_utf8())
            }
        })
        .collect();
    Edit { start, end, text }
}

/// A module file as project mode reads it.
pub(super) struct SourceFile<'a> {
    pub module: &'a crate::module::Module,
    pub path: &'a std::path::Path,
    pub text: String,
    pub items: Vec<Item>,
}

/// The package's files in dependency order (synthetic modules skipped), each
/// re-parsed so its spans are offsets into its own text. `with_tests` adds
/// each module's `_test.kara` companion after its production file.
pub(super) fn source_files(
    tree: &ProgramTree,
    with_tests: bool,
) -> Result<Vec<SourceFile<'_>>, String> {
    let mut out = Vec::new();
    for id in crate::module::emission_order(tree) {
        let m = &tree.modules[id];
        if m.is_synthetic {
            continue;
        }
        let mut paths = vec![m.file.as_path()];
        if with_tests {
            if let Some(t) = &m.test_file {
                paths.push(t.as_path());
            }
        }
        for path in paths {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let parsed = crate::parse(&text);
            if let Some(e) = parsed.errors.first() {
                return Err(format!(
                    "{}:{}:{}: {}",
                    path.display(),
                    e.span.line,
                    e.span.column,
                    e.message
                ));
            }
            if !parsed.program.inner_attrs.is_empty() && !out.is_empty() {
                return Err(format!(
                    "{} has a module-level inner attribute (`#![..]`), which v2 project \
                     mode keeps only in the package's first file",
                    path.display()
                ));
            }
            out.push(SourceFile {
                module: m,
                path,
                text,
                items: parsed.program.items,
            });
        }
    }
    Ok(out)
}

/// Combine the package's files into one source text. `extra` adds edits per
/// file (by its index in `files`) on top of the import handling; `tail` is
/// appended after the last file and maps to no file.
pub(super) fn amalgamate(
    tree: &ProgramTree,
    files: &[SourceFile<'_>],
    mut extra: impl FnMut(usize, &SourceFile<'_>) -> Vec<Edit>,
    tail: &str,
) -> Result<Amalgam, String> {
    if !crate::module_rename::plan(tree).is_empty() {
        return Err(
            "two modules of this package declare the same top-level name (or an import \
             renames one), which v2 project mode does not combine yet"
                .into(),
        );
    }
    let mut header: Vec<(String, String, usize)> = Vec::new();
    let mut seen_std: std::collections::HashSet<(Vec<String>, String)> = Default::default();
    let mut bodies: Vec<String> = Vec::new();
    for (index, f) in files.iter().enumerate() {
        let mut edits = Vec::new();
        for item in &f.items {
            let Item::Import(imp) = item else { continue };
            let end = statement_end(&f.text, imp);
            match import_kind(tree, f.module, imp) {
                Kind::Project => edits.push(blank(&f.text, imp.span.offset, end)),
                Kind::ModuleImport(name) => {
                    return Err(format!(
                        "{}:{}: `import` of the module `{name}` as a whole; v2 project mode \
                         combines the package into one scope, so import the items it uses",
                        f.path.display(),
                        imp.span.line
                    ))
                }
                Kind::Renamed(name) => {
                    return Err(format!(
                        "{}:{}: `{name} as ..` renames an import, which v2 project mode \
                         does not combine yet",
                        f.path.display(),
                        imp.span.line
                    ))
                }
                Kind::Other => {
                    // A `std` (or unknown) import moves to the header, once.
                    let fresh: Vec<String> = imp
                        .items
                        .iter()
                        .filter(|it| {
                            let bound = it.alias.clone().unwrap_or_else(|| it.name.clone());
                            seen_std.insert((imp.path.clone(), bound))
                        })
                        .map(|it| match &it.alias {
                            Some(a) => format!("{} as {a}", it.name),
                            None => it.name.clone(),
                        })
                        .collect();
                    let wildcard =
                        imp.is_wildcard && seen_std.insert((imp.path.clone(), "*".to_string()));
                    let path = imp.path.join(".");
                    let line = if wildcard {
                        Some(format!("import {path}.*;"))
                    } else if fresh.is_empty() {
                        None
                    } else {
                        Some(format!("import {path}.{{{}}};", fresh.join(", ")))
                    };
                    if let Some(line) = line {
                        header.push((line, shown(f.path), imp.span.line));
                    }
                    edits.push(blank(&f.text, imp.span.offset, end));
                }
            }
        }
        // `//!` lines are the module's doc, legal only at the top of a file.
        let mut at = 0;
        for line in f.text.split_inclusive('\n') {
            if line.trim_start().starts_with("//!") {
                edits.push(blank(&f.text, at, at + line.trim_end_matches('\n').len()));
            }
            at += line.len();
        }
        edits.extend(extra(index, f));
        bodies.push(apply(&f.text, edits)?);
    }

    let mut text = String::new();
    let mut lines = Vec::new();
    let mut bytes = Vec::new();
    let mut line = 1;
    for (h, file, l) in header {
        lines.push((line, file, l));
        text.push_str(&h);
        text.push('\n');
        line += 1;
    }
    for (f, body) in files.iter().zip(bodies) {
        lines.push((line, shown(f.path), 1));
        bytes.push((text.len(), body.len(), shown(f.path)));
        line += body.matches('\n').count();
        text.push_str(&body);
        if !body.ends_with('\n') {
            text.push('\n');
            line += 1;
        }
    }
    // The tail maps to no file: its lines keep their combined numbers.
    lines.push((line, String::from("<generated>"), line));
    text.push_str(tail);
    Ok(Amalgam { text, lines, bytes })
}

/// A file's path as reports show it: relative to the working directory when
/// it is under it, as `karac run src/main.kara` names its file.
pub(super) fn shown(path: &std::path::Path) -> String {
    let cwd = std::env::current_dir().ok();
    let canonical = cwd.as_ref().and_then(|c| std::fs::canonicalize(c).ok());
    for base in [cwd, canonical].into_iter().flatten() {
        if let Ok(rel) = path.strip_prefix(&base) {
            return rel.display().to_string();
        }
    }
    path.display().to_string()
}

/// Apply non-overlapping edits to `src`.
fn apply(src: &str, mut edits: Vec<Edit>) -> Result<String, String> {
    edits.sort_by_key(|e| e.start);
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for e in edits {
        if e.start < at {
            // Two edits of one desugared statement (`import a.{b.{c}, d}`):
            // the earlier one already covered it, unless it reaches further.
            if e.end > at {
                return Err("overlapping edits in one import statement".into());
            }
            continue;
        }
        out.push_str(&src[at..e.start]);
        out.push_str(&e.text);
        at = e.end;
    }
    out.push_str(&src[at..]);
    Ok(out)
}

/// Where an import statement ends: its span, and the `;` after it.
fn statement_end(src: &str, imp: &ImportDecl) -> usize {
    let end = (imp.span.offset + imp.span.length).min(src.len());
    let rest = &src[end..];
    let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    if rest[ws..].starts_with(';') {
        end + ws + 1
    } else {
        end
    }
}

enum Kind {
    /// Items of one of the package's own modules: blanked.
    Project,
    /// A module as a whole, used qualified: refused.
    ModuleImport(String),
    /// `name as alias` from a package module: refused.
    Renamed(String),
    /// `std`, or a path the checker reports.
    Other,
}

fn import_kind(tree: &ProgramTree, m: &crate::module::Module, imp: &ImportDecl) -> Kind {
    // A dependency's own imports are written without the package prefix the
    // tree registers its modules under.
    let lookup = |path: &[String]| -> bool {
        tree.graph.lookup(path).is_some()
            || m.package.as_ref().is_some_and(|p| {
                let mut full = vec![p.clone()];
                full.extend_from_slice(path);
                tree.graph.lookup(&full).is_some()
            })
    };
    let own = |path: &[String]| {
        lookup(path)
            && !path.first().is_some_and(|s| s == "std")
            && tree
                .graph
                .lookup(path)
                .is_none_or(|id| !tree.modules[id].is_synthetic)
    };
    for it in &imp.items {
        let mut full = imp.path.clone();
        full.push(it.name.clone());
        if own(&full) {
            return Kind::ModuleImport(full.join("."));
        }
    }
    if !own(&imp.path) {
        return Kind::Other;
    }
    if let Some(it) = imp.items.iter().find(|it| it.alias.is_some()) {
        return Kind::Renamed(it.name.clone());
    }
    Kind::Project
}

/// The package `filename` belongs to, when it has more than one module and
/// `filename` is one of them: its root and module tree. `with_tests` merges
/// each module's `_test.kara` companion into it.
pub(super) fn package_tree(
    filename: &str,
    no_manifest: bool,
    with_tests: bool,
) -> Option<(std::path::PathBuf, ProgramTree)> {
    if no_manifest {
        return None;
    }
    let entry = std::fs::canonicalize(filename).ok()?;
    let root = manifest::discover_project_root(entry.parent()?)?;
    let walked = walker::walk_project(
        &root,
        WalkerOpts {
            include_tests: with_tests,
            ..WalkerOpts::default()
        },
    )
    .ok()?;
    let dep_walks = quiet_dep_package_walks(&root);
    let built = module::build_program_tree_with_deps(
        &walked,
        &dep_walks,
        module::BuildTreeOpts {
            merge_test_companions: with_tests,
        },
    )
    .ok()?;
    if !built.parse_errors.is_empty() {
        return None;
    }
    let tree = built.tree;
    if tree.modules.iter().filter(|m| !m.is_synthetic).count() <= 1 {
        return None;
    }
    let in_tree = tree.modules.iter().filter(|m| !m.is_synthetic).any(|m| {
        std::fs::canonicalize(&m.file)
            .map(|p| p == entry)
            .unwrap_or(false)
    });
    in_tree.then_some((root, tree))
}
