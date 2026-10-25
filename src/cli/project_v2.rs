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
//! Names the flat scope cannot hold as written are rewritten in the copy
//! (B-2026-10-23-14): an import that renames (`open as make`) has the
//! file's uses of `make` read `open`; an import of a module as a whole
//! (`import db.connection;`) has its qualified uses (`connection.open`) lose
//! the qualifier; and two modules declaring the same top-level name have
//! the one that gives it up, and its importers, use the name
//! `module_rename::plan` mints, as does a member a module binding reaches
//! (`conn.open`) when the file has something else named `open`. A rewrite pads with spaces when the new name
//! is shorter, so only a longer name moves the columns after it on its line.
//! A module-level inner attribute (`#![..]`) in any file but the first is
//! refused by name.

use super::*;
use crate::ast::{ImportDecl, Item};
use crate::module::ProgramTree;
use std::collections::HashMap;

/// The combined source of a package, and where each of its lines came from.
pub(super) struct Amalgam {
    pub text: String,
    /// `(first combined line, file, that file's line)`, ascending by the
    /// first field; a line maps through the last entry at or before it.
    pub lines: Vec<(usize, String, usize)>,
    /// `(first combined byte, byte length, file)` for each copied file.
    bytes: Vec<(usize, usize, String)>,
    /// The package's module-aware check refused it by a type or late-phase
    /// rule: the v2 check of this source reports the refusal if it makes it
    /// too, and this is reported if it does not.
    pub refusal: Option<super::build_cmds::PackageCheck>,
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
            // A test file with no production sibling is the module's own
            // file as well as its companion.
            if let Some(t) = m.test_file.as_ref().filter(|t| **t != m.file) {
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
    let quals_of: Vec<Vec<(String, Vec<String>)>> = files
        .iter()
        .map(|f| {
            f.items
                .iter()
                .filter_map(|item| match item {
                    Item::Import(imp) => match import_kind(tree, f.module, imp) {
                        Kind::ModuleImport(modules) => Some(modules),
                        _ => None,
                    },
                    _ => None,
                })
                .flatten()
                .collect()
        })
        .collect();
    let renames = Flat::new(tree, crate::module_rename::plan(tree), files, &quals_of);
    let mut header: Vec<(String, String, usize)> = Vec::new();
    let mut seen_std: std::collections::HashSet<(Vec<String>, String)> = Default::default();
    let mut bodies: Vec<String> = Vec::new();
    for (index, f) in files.iter().enumerate() {
        let mut edits = Vec::new();
        // The names this file's text must read differently in the flat
        // scope: its own renamed declarations, and what it imports under
        // another name or from a module that renamed it.
        let mut subst: HashMap<String, String> = renames.renamed_in(f.module.id).collect();
        // What a wildcard import brings in under a name its module gave up,
        // unless the file declares or names that item itself.
        let mut wild: Vec<(String, String)> = Vec::new();
        // Module names this file uses as qualifiers, with each module's path.
        let quals = &quals_of[index];
        for item in &f.items {
            let Item::Import(imp) = item else { continue };
            let end = statement_end(&f.text, imp);
            match import_kind(tree, f.module, imp) {
                Kind::Project(target) => {
                    for it in &imp.items {
                        let bound = it.alias.as_ref().unwrap_or(&it.name);
                        let flat = target
                            .and_then(|t| flat_item(tree, &renames, t, &it.name, 8))
                            .unwrap_or_else(|| it.name.clone());
                        if *bound != flat {
                            subst.insert(bound.clone(), flat);
                        }
                    }
                    if let Some(t) = target.filter(|_| imp.is_wildcard) {
                        wild.extend(renames.renamed_in(t));
                    }
                    edits.push(blank(&f.text, statement_start(&f.text, imp), end));
                }
                Kind::ModuleImport(_) => {
                    edits.push(blank(&f.text, statement_start(&f.text, imp), end));
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
                    edits.push(blank(&f.text, statement_start(&f.text, imp), end));
                }
            }
        }
        if !wild.is_empty() {
            let own = crate::import_alias::declared_names(&f.items);
            for (name, flat) in wild {
                if !own.contains(&name) {
                    subst.entry(name).or_insert(flat);
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
        if !subst.is_empty() || !quals.is_empty() {
            edits.extend(flat_names(tree, &renames, f, &subst, quals)?);
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
    Ok(Amalgam {
        text,
        lines,
        bytes,
        refusal: None,
    })
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

/// Where an import statement starts: its span, after the `pub` of a
/// re-export, which the span leaves out.
fn statement_start(src: &str, imp: &ImportDecl) -> usize {
    let start = imp.span.offset.min(src.len());
    let before = src[..start].trim_end();
    match before.strip_suffix("pub") {
        Some(rest) if imp.is_pub && !rest.ends_with(|c: char| c.is_alphanumeric() || c == '_') => {
            rest.len()
        }
        _ => start,
    }
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
    /// Items of one of the package's own modules (that module, when the
    /// tree has it): blanked, a renamed one's uses rewritten.
    Project(Option<crate::module::ModuleId>),
    /// Modules as a whole, each as the name it binds and its path: blanked,
    /// their qualified uses unqualified.
    ModuleImport(Vec<(String, Vec<String>)>),
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
    let modules: Vec<(String, Vec<String>)> = imp
        .items
        .iter()
        .filter_map(|it| {
            let mut full = imp.path.clone();
            full.push(it.name.clone());
            own(&full).then(|| (it.alias.clone().unwrap_or_else(|| it.name.clone()), full))
        })
        .collect();
    if !modules.is_empty() {
        return Kind::ModuleImport(modules);
    }
    if !own(&imp.path) {
        return Kind::Other;
    }
    Kind::Project(module_id(tree, m, &imp.path))
}

/// The package module at `path` as `m`'s imports write it.
fn module_id(
    tree: &ProgramTree,
    m: &crate::module::Module,
    path: &[String],
) -> Option<crate::module::ModuleId> {
    tree.graph.lookup(path).or_else(|| {
        let mut full = vec![m.package.clone()?];
        full.extend_from_slice(path);
        tree.graph.lookup(&full)
    })
}

/// What every renamed item is called in the flat scope: the names
/// `module_rename::plan` mints for colliding declarations, and the ones a
/// contested member reached through a module binding takes.
///
/// A member is contested when the file that writes `conn.open` also has
/// something else under `open` (a local, its own item, another import):
/// unqualified, the use would bind to that instead. Legacy's module-binding
/// desugar reached such a member under an alias; here the member's module
/// gives the name up, as a collision would make it.
struct Flat {
    plan: crate::module_rename::FlatRenames,
    extra: HashMap<crate::module::ModuleId, HashMap<String, String>>,
}

impl Flat {
    fn new(
        tree: &ProgramTree,
        plan: crate::module_rename::FlatRenames,
        files: &[SourceFile<'_>],
        quals_of: &[Vec<(String, Vec<String>)>],
    ) -> Flat {
        let mut flat = Flat {
            plan,
            extra: HashMap::new(),
        };
        let mut taken: Option<std::collections::HashSet<String>> = None;
        for (f, quals) in files.iter().zip(quals_of) {
            if quals.is_empty() {
                continue;
            }
            let bound = crate::import_alias::bound_value_names(&mut f.items.clone());
            let quals = live_qualifiers(quals, &bound);
            let mut toks = Vec::new();
            idents(&f.text, 0, &mut toks);
            let uses = qualified_uses(tree, f, &toks, &quals);
            if uses.iter().all(|u| u.member.is_none()) {
                continue;
            }
            // Every name the file's text answers to besides its qualified uses.
            let mut names = bound;
            names.extend(crate::import_alias::declared_names(&f.items));
            for item in &f.items {
                let Item::Import(imp) = item else { continue };
                match import_kind(tree, f.module, imp) {
                    Kind::Project(target) => {
                        for it in &imp.items {
                            names.insert(
                                target
                                    .and_then(|t| flat_item(tree, &flat, t, &it.name, 8))
                                    .unwrap_or_else(|| it.name.clone()),
                            );
                        }
                        if let Some(t) = target.filter(|_| imp.is_wildcard) {
                            for n in crate::import_alias::declared_names(&tree.modules[t].items) {
                                names.insert(flat.get(t, &n).cloned().unwrap_or(n));
                            }
                        }
                    }
                    Kind::Other => {
                        names.extend(
                            imp.items
                                .iter()
                                .map(|it| it.alias.clone().unwrap_or_else(|| it.name.clone())),
                        );
                    }
                    Kind::ModuleImport(_) => {}
                }
            }
            for u in &uses {
                let (Some(m), Some(member)) = (u.module, u.member) else {
                    continue;
                };
                let Some((origin, name)) = origin(tree, m, &toks[member].name, 8) else {
                    continue;
                };
                if !names.contains(&name) || flat.get(origin, &name).is_some() {
                    continue;
                }
                let module = &tree.modules[origin];
                // The root keeps its names, and a linkage name or one the
                // module also binds as a local cannot be rewritten, as in
                // `module_rename::plan`.
                if origin == tree.root
                    || crate::module_rename::name_is_linkage(module, &name)
                    || crate::import_alias::local_value_bindings(&mut module.items.clone())
                        .contains(&name)
                {
                    continue;
                }
                let taken = taken.get_or_insert_with(|| {
                    let mut taken: std::collections::HashSet<String> = tree
                        .modules
                        .iter()
                        .flat_map(|m| crate::import_alias::declared_names(&m.items))
                        .collect();
                    for m in &tree.modules {
                        taken.extend(flat.plan.renamed_in(m.id).map(|(_, b)| b.clone()));
                    }
                    taken
                });
                let fresh = crate::module_rename::qualified_name(&name, &module.path, taken);
                flat.extra.entry(origin).or_default().insert(name, fresh);
            }
        }
        flat
    }

    fn get(&self, m: crate::module::ModuleId, name: &str) -> Option<&String> {
        self.extra
            .get(&m)
            .and_then(|x| x.get(name))
            .or_else(|| self.plan.flat_name(m, name))
    }

    /// Every name module `m` gave up, with the name it takes instead.
    fn renamed_in(
        &self,
        m: crate::module::ModuleId,
    ) -> impl Iterator<Item = (String, String)> + '_ {
        self.plan
            .renamed_in(m)
            .chain(self.extra.get(&m).into_iter().flatten())
            .map(|(a, b)| (a.clone(), b.clone()))
    }
}

/// The module that declares the item module `m` offers as `name`, and the
/// name it declares it under: `m` itself, or, for a `pub import`
/// re-export, the re-exported item's, followed `depth` deep.
fn origin(
    tree: &ProgramTree,
    m: crate::module::ModuleId,
    name: &str,
    depth: u32,
) -> Option<(crate::module::ModuleId, String)> {
    let module = &tree.modules[m];
    if crate::import_alias::declared_names(&module.items).contains(name) {
        return Some((m, name.to_string()));
    }
    if depth == 0 {
        return None;
    }
    module
        .imports
        .iter()
        .filter(|imp| imp.is_pub)
        .find_map(|imp| {
            let it = imp
                .items
                .iter()
                .find(|it| it.alias.as_deref().unwrap_or(&it.name) == name)?;
            origin(
                tree,
                module_id(tree, module, &imp.path)?,
                &it.name,
                depth - 1,
            )
        })
}

/// The flat name of the item module `m` offers as `name`: its own, or, for
/// a `pub import` re-export, the re-exported item's, followed `depth` deep.
fn flat_item(
    tree: &ProgramTree,
    renames: &Flat,
    m: crate::module::ModuleId,
    name: &str,
    depth: u32,
) -> Option<String> {
    if let Some(flat) = renames.get(m, name) {
        return Some(flat.clone());
    }
    let module = &tree.modules[m];
    if depth == 0 || crate::import_alias::declared_names(&module.items).contains(name) {
        return None;
    }
    module
        .imports
        .iter()
        .filter(|imp| imp.is_pub)
        .find_map(|imp| {
            let it = imp
                .items
                .iter()
                .find(|it| it.alias.as_deref().unwrap_or(&it.name) == name)?;
            let from = module_id(tree, module, &imp.path)?;
            Some(
                flat_item(tree, renames, from, &it.name, depth - 1)
                    .unwrap_or_else(|| it.name.clone()),
            )
        })
}

/// An identifier token of a file, with what is written on either side.
struct Ident {
    offset: usize,
    name: String,
    /// Written after a `.`: a field, a method or a later path segment.
    after_dot: bool,
    /// Written before a `.`.
    before_dot: bool,
    /// Written before a single `:`: a field, a parameter or a label.
    before_colon: bool,
    /// Written after `const` or `let`: the name a declaration binds.
    declared: bool,
}

/// Every identifier token of `src`, an f-string's holes included, with
/// offsets from `base`.
fn idents(src: &str, base: usize, out: &mut Vec<Ident>) {
    use crate::token::{InterpolationPart, Token};
    let mut lexer = crate::lexer::Lexer::new(src);
    let mut toks = Vec::new();
    loop {
        let t = lexer.next_token();
        if t.token == Token::EOF {
            break;
        }
        toks.push(t);
    }
    for (i, t) in toks.iter().enumerate() {
        match &t.token {
            Token::Identifier { name, raw: false } => out.push(Ident {
                offset: base + t.span.offset,
                name: name.clone(),
                after_dot: i > 0 && toks[i - 1].token == Token::Dot,
                before_dot: toks.get(i + 1).is_some_and(|n| n.token == Token::Dot),
                before_colon: toks.get(i + 1).is_some_and(|n| n.token == Token::Colon),
                declared: i > 0 && matches!(toks[i - 1].token, Token::Const | Token::Let),
            }),
            Token::InterpolatedStringLiteral(parts) => {
                for p in parts {
                    if let InterpolationPart::Expr { raw, offset, .. } = p {
                        idents(raw, *offset, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The module imports of a file that are qualifiers in its text: those it
/// does not also bind as a value, as the legacy merge's rewrite deferred
/// to them, so `connection.v` on a local `connection` is a field access.
fn live_qualifiers<'q>(
    quals: &'q [(String, Vec<String>)],
    bound: &std::collections::HashSet<String>,
) -> Vec<&'q (String, Vec<String>)> {
    quals.iter().filter(|(q, _)| !bound.contains(q)).collect()
}

/// A use of a module binding as a qualifier (`connection.open`,
/// `db.connection.open`).
struct Qualified {
    /// The first token of the qualifier.
    first: usize,
    /// Where the `.` before the member ends.
    end: usize,
    /// The module the qualifier chain names.
    module: Option<crate::module::ModuleId>,
    /// The member's token.
    member: Option<usize>,
}

/// Every qualified use of `quals` in `toks`, in order.
fn qualified_uses(
    tree: &ProgramTree,
    f: &SourceFile<'_>,
    toks: &[Ident],
    quals: &[&(String, Vec<String>)],
) -> Vec<Qualified> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        i += 1;
        if t.after_dot || !t.before_dot {
            continue;
        }
        let Some((_, path)) = quals.iter().find(|(q, _)| *q == t.name) else {
            continue;
        };
        let first = i - 1;
        // `q.` and each submodule after it, up to the item.
        let mut path = path.clone();
        let mut end = t.offset + t.name.len();
        while let Some(n) = toks.get(i).filter(|n| n.after_dot && n.before_dot) {
            let mut sub = path.clone();
            sub.push(n.name.clone());
            if module_id(tree, f.module, &sub).is_none() {
                break;
            }
            path = sub;
            end = n.offset + n.name.len();
            i += 1;
        }
        let end = f.text[end..].find('.').map_or(end, |d| end + d + 1);
        let member = toks.get(i).filter(|n| n.after_dot).map(|_| i);
        if member.is_some() {
            i += 1;
        }
        out.push(Qualified {
            first,
            end,
            module: module_id(tree, f.module, &path),
            member,
        });
    }
    out
}

/// The edits that make `f`'s text name items as the flat scope does: each
/// use of a name in `subst` reads its flat name, and each use of a module
/// in `quals` as a qualifier loses it, its member taking the member's flat
/// name. A name the file also binds as a local keeps its spelling.
fn flat_names(
    tree: &ProgramTree,
    renames: &Flat,
    f: &SourceFile<'_>,
    subst: &HashMap<String, String>,
    quals: &[(String, Vec<String>)],
) -> Result<Vec<Edit>, String> {
    let mut items = f.items.clone();
    let bound = crate::import_alias::bound_value_names(&mut items);
    let locals = crate::import_alias::local_value_bindings(&mut items);
    let quals = live_qualifiers(quals, &bound);
    let mut toks = Vec::new();
    idents(&f.text, 0, &mut toks);
    let mut edits = Vec::new();
    let mut skip = vec![false; toks.len()];
    for u in qualified_uses(tree, f, &toks, &quals) {
        edits.push(blank(&f.text, toks[u.first].offset, u.end));
        skip[u.first..u.member.map_or(u.first + 1, |m| m + 1)].fill(true);
        let (Some(m), Some(member)) = (u.module, u.member) else {
            continue;
        };
        let n = &toks[member];
        if let Some(flat) = flat_item(tree, renames, m, &n.name, 8) {
            if flat != n.name {
                edits.push(rename(n, &flat));
            }
        }
    }
    for (t, _) in toks.iter().zip(&skip).filter(|(_, s)| !**s) {
        if t.after_dot || (t.before_colon && !t.declared) || locals.contains(&t.name) {
            continue;
        }
        if let Some(flat) = subst.get(&t.name) {
            edits.push(rename(t, flat));
        }
    }
    Ok(edits)
}

/// `t` spelled `flat`, padded to its length when `flat` is shorter.
fn rename(t: &Ident, flat: &str) -> Edit {
    let mut text = flat.to_string();
    if text.len() < t.name.len() {
        text.push_str(&" ".repeat(t.name.len() - text.len()));
    }
    Edit {
        start: t.offset,
        end: t.offset + t.name.len(),
        text,
    }
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
