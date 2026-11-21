//! References and rename across the modules of a package
//! (B-2026-11-21-34).
//!
//! A per-document query sees one file, so renaming a `pub fn` at its
//! declaration left every importing module naming the old one, and the
//! package no longer checked. Here a query on a `pub` item of a file inside
//! a `kara.toml` package runs against a snapshot of the package: its files
//! as the editor holds them (an open buffer's text, else the file on disk),
//! copied to a scratch directory so that what each module imports reads
//! what the editor shows. Every occurrence of the item's name in every
//! module is asked for its definition, and those that go to the item are
//! its references.
//!
//! A rename is checked before it is offered: the package (or the lone
//! document) is checked with the edits made, and a rename that adds an
//! error anywhere is refused with that error, so the editor is never handed
//! a rename that leaves a file failing `karac check`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{Location, Position, Range, TextEdit, Uri};

use crate::analysis::{self, LineIndex};

/// One module of a [`Snapshot`].
struct File {
    real: PathBuf,
    scratch: PathBuf,
    text: String,
}

/// A package's modules as the editor holds them, under a scratch copy.
struct Snapshot {
    dir: PathBuf,
    files: Vec<File>,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn kara_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            kara_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "kara") {
            out.push(p);
        }
    }
}

impl Snapshot {
    /// The package around `path`, `None` when it is in none.
    fn of(path: &Path, docs: &HashMap<String, String>) -> Option<Snapshot> {
        let root = karac::manifest::discover_project_root(path.parent()?)?;
        let root = std::fs::canonicalize(root).ok()?;
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kara-lsp-ws-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        let mut snap = Snapshot {
            dir: dir.clone(),
            files: Vec::new(),
        };
        for name in ["kara.toml", "kara.lock", "karac-toolchain.toml"] {
            if root.join(name).is_file() {
                std::fs::copy(root.join(name), dir.join(name)).ok()?;
            }
        }
        let mut paths = Vec::new();
        kara_files(&root.join("src"), &mut paths);
        for real in paths {
            let rel = real.strip_prefix(&root).ok()?;
            let scratch = dir.join(rel);
            std::fs::create_dir_all(scratch.parent()?).ok()?;
            let uri = analysis::path_uri(&real).map(|u| u.to_string());
            let text = match uri.and_then(|u| docs.get(&u).cloned()) {
                Some(t) => t,
                None => std::fs::read_to_string(&real).ok()?,
            };
            std::fs::write(&scratch, &text).ok()?;
            snap.files.push(File {
                real,
                scratch,
                text,
            });
        }
        Some(snap)
    }

    fn index_of_real(&self, path: &Path) -> Option<usize> {
        let path = std::fs::canonicalize(path).ok()?;
        self.files.iter().position(|f| f.real == path)
    }

    fn index_of_scratch(&self, path: &Path) -> Option<usize> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.files.iter().position(|f| {
            std::fs::canonicalize(&f.scratch).unwrap_or_else(|_| f.scratch.clone()) == path
        })
    }

    /// Where the name at `pos` of file `i` is declared: a file of the
    /// snapshot and the name's range there.
    fn definition(&self, i: usize, pos: Position) -> Option<(usize, Range)> {
        let f = &self.files[i];
        let (uri, range) = analysis::definition_site_in(&f.text, Some(&f.scratch), pos)?;
        let at = match uri {
            None => i,
            Some(u) => self.index_of_scratch(&analysis::file_path(&u)?)?,
        };
        Some((at, range))
    }

    /// The module file `i` is, as an import names it (`src/net/http.kara`
    /// is `net.http`); `None` for the package's entry point.
    fn module_name(&self, i: usize) -> Option<String> {
        let rel = self.files[i]
            .scratch
            .strip_prefix(self.dir.join("src"))
            .ok()?;
        let rel = rel.with_extension("");
        let name: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        (name != ["main"] && name != ["lib"]).then(|| name.join("."))
    }

    /// Every error `karac check` reports in each file, as `file: message`.
    fn errors(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in &self.files {
            for d in karac::check_source_at(&f.text, &f.scratch) {
                if d.severity == "error" {
                    out.push(format!(
                        "{}:{}:{}: {}",
                        f.real.display(),
                        d.line,
                        d.column,
                        d.message
                    ));
                }
            }
        }
        out
    }
}

fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// The identifier around byte `at` of `text`, as a byte range.
fn word_at(text: &str, at: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let (mut a, mut b) = (at.min(bytes.len()), at.min(bytes.len()));
    while a > 0 && ident(bytes[a - 1]) {
        a -= 1;
    }
    while b < bytes.len() && ident(bytes[b]) {
        b += 1;
    }
    (a < b).then_some((a, b))
}

/// The byte offsets of `name` as a whole word in `text`.
fn occurrences(text: &str, name: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    text.match_indices(name)
        .map(|(i, _)| i)
        .filter(|&i| {
            (i == 0 || !ident(bytes[i - 1])) && bytes.get(i + name.len()).is_none_or(|&b| !ident(b))
        })
        .collect()
}

/// Whether the declaration at `range` of `text` is `pub`: only such an item
/// can be named by another module.
fn is_pub(text: &str, range: Range) -> bool {
    let index = LineIndex::new(text);
    let at = index.offset(range.start);
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    text[line_start..at].split_whitespace().any(|w| w == "pub")
}

/// `(definition file, definition range, (file, start, end) of every site)`.
type Sites = (usize, Range, Vec<(usize, usize, usize)>);

/// The sites of the item the name at `pos` of `path` names, across its
/// package, when it is a `pub` item of a package module: `(file index,
/// byte range)`, the declaration included. `None` sends the caller to the
/// one-document query.
fn sites(snap: &Snapshot, i: usize, pos: Position) -> Option<Sites> {
    // On the declaration itself the definition query answers nothing: the
    // name there is the declaration.
    let (def_file, def_range) = match snap.definition(i, pos) {
        Some(d) => d,
        None => {
            let text = &snap.files[i].text;
            let (a, b) = word_at(text, LineIndex::new(text).offset(pos))?;
            (i, LineIndex::new(text).range(a, b - a))
        }
    };
    let def_text = &snap.files[def_file].text;
    if !is_pub(def_text, def_range) {
        return None;
    }
    let def_at = LineIndex::new(def_text).offset(def_range.start);
    let (a, b) = word_at(def_text, def_at)?;
    let name = &def_text[a..b];
    let module = snap.module_name(def_file);
    let mut out = vec![(def_file, a, b)];
    // In the declaring module, the one-document references.
    let def_index = LineIndex::new(def_text);
    for r in analysis::references(def_text, def_range.start, true) {
        let (ra, rb) = (def_index.offset(r.start), def_index.offset(r.end));
        if ra != a && def_text.get(ra..rb) == Some(name) {
            out.push((def_file, ra, rb));
        }
    }
    for (j, f) in snap.files.iter().enumerate() {
        if j == def_file {
            continue;
        }
        let index = LineIndex::new(&f.text);
        for at in occurrences(&f.text, name) {
            // An import of the item names it where no definition query
            // answers: `import util.{helper}`, `import util.helper`.
            let line_start = f.text[..at].rfind('\n').map_or(0, |k| k + 1);
            let line = &f.text[line_start..];
            let line = &line[..line.find('\n').unwrap_or(line.len())];
            let imports = module.as_deref().is_some_and(|m| {
                let l = line.trim_start();
                l.starts_with("import ")
                    && (l.contains(&format!("{m}.{{")) || l.contains(&format!("{m}.{name}")))
            });
            if imports || snap.definition(j, index.position(at)) == Some((def_file, def_range)) {
                out.push((j, at, at + name.len()));
            }
        }
    }
    Some((def_file, def_range, out))
}

/// `textDocument/references` across the package of the document at `path`;
/// `None` when the name is no `pub` item of a package module.
pub fn references(
    path: &Path,
    docs: &HashMap<String, String>,
    pos: Position,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let snap = Snapshot::of(path, docs)?;
    let i = snap.index_of_real(path)?;
    let (_, _, sites) = sites(&snap, i, pos)?;
    let mut out = Vec::new();
    for (k, (j, a, b)) in sites.into_iter().enumerate() {
        if k == 0 && !include_declaration {
            continue;
        }
        let f = &snap.files[j];
        out.push(Location {
            uri: analysis::path_uri(&f.real)?,
            range: LineIndex::new(&f.text).range(a, b - a),
        });
    }
    Some(out)
}

/// `textDocument/rename` across the package of the document at `path`:
/// the edits per file, `Ok(None)` when the name is no `pub` item of a
/// package module (the caller renames in the one document), `Err` when the
/// rename would add an error to the package.
#[allow(clippy::mutable_key_type)] // `Uri` keys, as `WorkspaceEdit` takes them
pub fn rename(
    path: &Path,
    docs: &HashMap<String, String>,
    pos: Position,
    new_name: &str,
) -> Result<Option<HashMap<Uri, Vec<TextEdit>>>, String> {
    if let Some(why) = karac::rename_name_error(new_name) {
        return Err(why);
    }
    let Some(mut snap) = Snapshot::of(path, docs) else {
        return Ok(None);
    };
    let Some(i) = snap.index_of_real(path) else {
        return Ok(None);
    };
    let Some((def_file, def_range, sites)) = sites(&snap, i, pos) else {
        return Ok(None);
    };
    // The declaring module renames as the one-document rename does there
    // (a field's literal and pattern sites, a shorthand), the others at
    // each site found.
    let mut per_file: HashMap<usize, Vec<(usize, usize, String)>> = HashMap::new();
    let def_text = snap.files[def_file].text.clone();
    if let Some(edits) = analysis::rename(&def_text, def_range.start, new_name)? {
        let index = LineIndex::new(&def_text);
        for e in edits {
            let a = index.offset(e.range.start);
            let b = index.offset(e.range.end);
            per_file
                .entry(def_file)
                .or_default()
                .push((a, b, e.new_text));
        }
    }
    for (j, a, b) in sites {
        if j == def_file {
            continue;
        }
        per_file
            .entry(j)
            .or_default()
            .push((a, b, new_name.to_string()));
    }
    let before = snap.errors();
    let mut changes = HashMap::new();
    for (j, mut edits) in per_file {
        edits.sort_by_key(|x| std::cmp::Reverse(x.0));
        edits.dedup_by(|x, y| x.0 == y.0 && x.1 == y.1);
        let f = &snap.files[j];
        let index = LineIndex::new(&f.text);
        let lsp_edits: Vec<TextEdit> = edits
            .iter()
            .map(|(a, b, t)| TextEdit {
                range: index.range(*a, b - a),
                new_text: t.clone(),
            })
            .collect();
        let mut text = f.text.clone();
        for (a, b, t) in &edits {
            text.replace_range(*a..*b, t);
        }
        let Some(uri) = analysis::path_uri(&f.real) else {
            return Ok(None);
        };
        changes.insert(uri, lsp_edits);
        let f = &mut snap.files[j];
        f.text = text;
        let _ = std::fs::write(&f.scratch, &f.text);
    }
    // More errors than before (a message can name the old name, so the
    // count is what is compared): the first one that is new says why.
    let after = snap.errors();
    if after.len() > before.len() {
        let new = after
            .iter()
            .find(|e| !before.contains(e))
            .unwrap_or(&after[0]);
        return Err(format!(
            "renaming to `{new_name}` would leave the package failing `karac check`: {new}"
        ));
    }
    Ok(Some(changes))
}

/// A one-document rename's edits, refused when they add an error to the
/// document (a new name that another in scope already has).
pub fn checked_single_file(
    text: &str,
    path: Option<&Path>,
    edits: &[TextEdit],
    new_name: &str,
) -> Result<(), String> {
    let index = LineIndex::new(text);
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            (
                index.offset(e.range.start),
                index.offset(e.range.end),
                e.new_text.as_str(),
            )
        })
        .collect();
    spans.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut new = text.to_string();
    for (a, b, t) in spans {
        if a <= b && b <= new.len() {
            new.replace_range(a..b, t);
        }
    }
    let errors = |s: &str| -> Vec<String> {
        let diags = match path {
            Some(p) => karac::check_source_at(s, p),
            None => karac::check_source(s),
        };
        diags
            .into_iter()
            .filter(|d| d.severity == "error")
            .map(|d| d.message)
            .collect()
    };
    let before = errors(text);
    let after = errors(&new);
    if after.len() > before.len() {
        let e = after
            .iter()
            .find(|e| !before.contains(e))
            .unwrap_or(&after[0]);
        return Err(format!(
            "renaming to `{new_name}` would leave the file failing `karac check`: {e}"
        ));
    }
    Ok(())
}
