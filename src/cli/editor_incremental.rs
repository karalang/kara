//! Incremental checking of an editor buffer (B-2026-11-09-33).
//!
//! A full `karac check` of a 15k-line file takes about two seconds, and an
//! editor re-checks on every edit. Most edits change one function body, and
//! a body's diagnostics depend on the items' signatures, not on the other
//! bodies, with two exceptions this module guards: the effects a private
//! function infers from the functions it calls, and the checks that look at
//! every use of an item.
//!
//! So [`DocumentChecker::check`] keeps the last result, and when the new
//! text differs from it only inside function bodies it re-checks a copy
//! where every other body is `loop {}`, except the edited ones and every
//! body they may call, transitively. Each byte keeps its offset, so the
//! edited functions' diagnostics are read off that check, and every other
//! diagnostic is the last result's, moved by the edit's length. It falls
//! back to a full check when:
//! - anything outside a function body changed, a signature included;
//! - an edited body names a different set of the buffer's items (a new call,
//!   or a use of an import, changes what "unused" or "recursive" means
//!   for another item);
//! - an edited function's inferred effects changed (its callers infer
//!   theirs from it);
//! - an edited body is one that can decide a signature: a `comptime fn`, or
//!   one that returns an `impl` type.
//!
//! [`focus`] is the same emptying, for completion, which needs only the
//! types inside the body at the cursor.

use std::collections::{HashMap, HashSet};

use crate::ast::{ImplItem, Item, TraitItem};
use crate::PlaygroundDiagnostic;

/// One function body of the buffer.
#[derive(Debug, Clone)]
struct Body {
    /// The key the effect checker files the function under.
    key: String,
    /// Its own name, as a call names it.
    name: String,
    /// The whole item, signature and body: the span its diagnostics fall in.
    item: (usize, usize),
    /// Between the braces.
    inner: (usize, usize),
    /// A body that may decide a signature, never emptied.
    fixed: bool,
}

/// The function bodies of a buffer that parses, in source order, and the
/// names its items declare.
#[derive(Debug, Clone)]
struct Outline {
    bodies: Vec<Body>,
    names: HashSet<String>,
}

/// The identifier-shaped words of `text`.
fn words(text: &str) -> HashSet<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty() && !w.starts_with(|c: char| c.is_ascii_digit()))
        .collect()
}

fn outline(text: &str) -> Option<Outline> {
    let parsed = crate::parse(text);
    if !parsed.errors.is_empty() || parsed.had_top_level_statements {
        return None;
    }
    let mut bodies = Vec::new();
    let mut names = HashSet::new();
    let b = text.as_bytes();
    let mut add = |key: String,
                   name: &str,
                   item: &crate::token::Span,
                   body: &crate::ast::Block,
                   comptime: bool|
     -> Option<()> {
        let (start, end) = (body.span.offset, body.span.offset + body.span.length);
        if end > b.len() || start >= end || b[start] != b'{' || b[end - 1] != b'}' {
            return None;
        }
        let item_start = item.offset.min(start);
        let sig = text.get(item_start..start)?;
        let fixed = comptime || words(sig).contains("impl");
        bodies.push(Body {
            key,
            name: name.to_string(),
            item: (item_start, end.max(item.offset + item.length)),
            inner: (start + 1, end - 1),
            fixed,
        });
        Some(())
    };
    for item in &parsed.program.items {
        match item {
            Item::Function(f) => {
                names.insert(f.name.clone());
                add(f.name.clone(), &f.name, &f.span, &f.body, f.is_comptime)?;
            }
            Item::ImplBlock(ib) => {
                let span = &ib.target_type.span;
                let target = text.get(span.offset..span.offset + span.length)?;
                let head = target.split('[').next().unwrap_or(target).trim();
                for it in &ib.items {
                    if let ImplItem::Method(f) = it {
                        names.insert(f.name.clone());
                        let key = format!("{head}.{}", f.name);
                        add(key, &f.name, &f.span, &f.body, f.is_comptime)?;
                    }
                }
            }
            Item::TraitDef(td) => {
                names.insert(td.name.clone());
                for it in &td.items {
                    match it {
                        TraitItem::Method(m) => {
                            names.insert(m.name.clone());
                            if let Some(body) = &m.body {
                                let key = format!("{}.{}", td.name, m.name);
                                add(key, &m.name, &m.span, body, false)?;
                            }
                        }
                        TraitItem::AssocType(_) => {}
                    }
                }
            }
            Item::StructDef(d) => {
                names.insert(d.name.clone());
            }
            Item::UnionDef(d) => {
                names.insert(d.name.clone());
            }
            Item::EnumDef(d) => {
                names.insert(d.name.clone());
                names.extend(d.variants.iter().map(|v| v.name.clone()));
            }
            Item::ConstDecl(d) => {
                names.insert(d.name.clone());
            }
            Item::TypeAlias(d) => {
                names.insert(d.name.clone());
            }
            Item::ModuleBinding(d) => {
                names.insert(d.name.clone());
            }
            Item::Import(d) => {
                names.extend(d.path.last().cloned());
                for it in &d.items {
                    names.insert(it.alias.clone().unwrap_or_else(|| it.name.clone()));
                }
            }
            // Any other item, every word of it.
            other => {
                let s = other.span();
                let t = text.get(s.offset..(s.offset + s.length).min(text.len()))?;
                names.extend(words(t).into_iter().map(str::to_string));
            }
        }
    }
    Some(Outline { bodies, names })
}

/// What an emptied body holds, by the effects it must keep inferring: a
/// body that only panics still panics. `None` for any other set. The
/// check after confirms each emptied body inferred what it did.
fn fill_for(effects: Option<&str>) -> Option<&'static [u8]> {
    match effects {
        None | Some("pure") => Some(b"loop {}"),
        Some("panics") => Some(b"panic(\"\")"),
        Some("allocates(Heap)") => Some(b"let _a = String.new(); loop {}"),
        Some("allocates(Heap) + panics") => Some(b"let _a = String.new(); panic(\"\")"),
        _ => None,
    }
}

/// `out[s..e]` as `fill` and spaces, its line breaks kept; `false` (and
/// `out` unchanged) when no line of it has room for `fill`.
fn empty_into(out: &mut [u8], s: usize, e: usize, fill: &[u8]) -> bool {
    let region = &out[s..e];
    let mut run = 0;
    let mut at = None;
    for (i, c) in region.iter().enumerate() {
        if *c == b'\n' {
            run = 0;
        } else {
            run += 1;
            if run == fill.len() {
                at = Some(i + 1 - fill.len());
                break;
            }
        }
    }
    let Some(at) = at else { return false };
    for c in &mut out[s..e] {
        if *c != b'\n' {
            *c = b' ';
        }
    }
    out[s + at..s + at + fill.len()].copy_from_slice(fill);
    true
}

/// `text` with each body `fill` gives a fill for emptied to it, and the
/// bodies emptied.
fn emptied(
    text: &str,
    outline: &Outline,
    fill: impl Fn(usize, &Body) -> Option<&'static [u8]>,
) -> Option<(String, Vec<usize>)> {
    let mut out = text.as_bytes().to_vec();
    let mut done = Vec::new();
    for (i, b) in outline.bodies.iter().enumerate() {
        if b.fixed {
            continue;
        }
        if let Some(fill) = fill(i, b) {
            if empty_into(&mut out, b.inner.0, b.inner.1, fill) {
                done.push(i);
            }
        }
    }
    let any = !done.is_empty();
    if !any {
        return None;
    }
    String::from_utf8(out).ok().map(|t| (t, done))
}

/// For each body, the bodies it may call: those whose function's name is
/// a word of it.
fn call_edges(text: &str, outline: &Outline) -> Vec<Vec<usize>> {
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, b) in outline.bodies.iter().enumerate() {
        by_name.entry(b.name.as_str()).or_default().push(i);
    }
    outline
        .bodies
        .iter()
        .map(|b| {
            let mut out: Vec<usize> = words(&text[b.inner.0..b.inner.1])
                .into_iter()
                .flat_map(|w| by_name.get(w).cloned().unwrap_or_default())
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        })
        .collect()
}

/// `start` and every body `edges` reaches from it.
fn reach(edges: &[Vec<usize>], start: &[usize]) -> HashSet<usize> {
    let mut seen: HashSet<usize> = start.iter().copied().collect();
    let mut work: Vec<usize> = start.to_vec();
    while let Some(i) = work.pop() {
        for &j in &edges[i] {
            if seen.insert(j) {
                work.push(j);
            }
        }
    }
    seen
}

/// `text` with every function body but the one around byte `at` emptied
/// to `loop {}`, each byte where it was: what completion checks, as it
/// needs only the types inside that body. `None` when nothing is emptied (the buffer does not
/// parse, is a script, or `at` is in no body).
pub(crate) fn focus(text: &str, at: usize) -> Option<String> {
    if !enabled() {
        return None;
    }
    let outline = outline(text)?;
    let here = outline
        .bodies
        .iter()
        .position(|b| b.inner.0 <= at && at <= b.inner.1)?;
    emptied(text, &outline, |i, _| {
        (i != here).then_some(&b"loop {}"[..])
    })
    .map(|(t, _)| t)
}

fn enabled() -> bool {
    std::env::var("KARAC_LSP_INCREMENTAL").as_deref() != Ok("0")
}

/// The last checked revision of a buffer.
struct Snapshot {
    text: String,
    diags: Vec<PlaygroundDiagnostic>,
    outline: Outline,
    effects: HashMap<String, String>,
}

/// How a [`DocumentChecker::check`] was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// The whole buffer was checked.
    Full,
    /// Only the edited bodies were checked.
    Incremental,
    /// The text was the last checked text.
    Unchanged,
}

/// The diagnostics of one editor buffer, re-checked edit by edit.
#[derive(Default)]
pub struct DocumentChecker {
    last: Option<Snapshot>,
}

impl DocumentChecker {
    pub fn new() -> Self {
        Self::default()
    }

    /// `karac check`'s diagnostics for `text`, the same set
    /// [`crate::check_source`] gives, from the last result where the edit
    /// allows.
    pub fn check(&mut self, text: &str) -> Vec<PlaygroundDiagnostic> {
        self.check_reporting(text).0
    }

    /// [`Self::check`], saying how it was answered.
    pub fn check_reporting(&mut self, text: &str) -> (Vec<PlaygroundDiagnostic>, CheckKind) {
        if enabled() {
            if let Some(answer) = self.incremental(text) {
                return answer;
            }
        }
        (self.full(text), CheckKind::Full)
    }

    /// [`Self::check`] for the buffer of the file at `path`. A member of a
    /// `kara.toml` package is checked with its package
    /// ([`crate::check_source_at`]), whole each time.
    pub fn check_at(
        &mut self,
        text: &str,
        path: Option<&std::path::Path>,
    ) -> Vec<PlaygroundDiagnostic> {
        match path {
            Some(p)
                if super::run_check_cmds::package_root_of_member(&p.to_string_lossy())
                    .is_some() =>
            {
                self.last = None;
                super::editor::check_source_at(text, p)
            }
            _ => self.check(text),
        }
    }

    /// [`Self::full`] for the buffer of the file at `path`, as
    /// [`Self::check_at`] treats a package member.
    pub fn full_at(
        &mut self,
        text: &str,
        path: Option<&std::path::Path>,
    ) -> Vec<PlaygroundDiagnostic> {
        match path {
            Some(p)
                if super::run_check_cmds::package_root_of_member(&p.to_string_lossy())
                    .is_some() =>
            {
                self.last = None;
                super::editor::check_source_at(text, p)
            }
            _ => self.full(text),
        }
    }

    /// A full check of `text`, which becomes the last result.
    pub fn full(&mut self, text: &str) -> Vec<PlaygroundDiagnostic> {
        let (diags, effects) = super::editor::check_with_effects(text);
        self.last = outline(text).map(|outline| Snapshot {
            text: text.to_string(),
            diags: diags.clone(),
            outline,
            effects,
        });
        diags
    }

    fn incremental(&mut self, text: &str) -> Option<(Vec<PlaygroundDiagnostic>, CheckKind)> {
        let last = self.last.as_ref()?;
        if last.text == text {
            return Some((last.diags.clone(), CheckKind::Unchanged));
        }
        let new = outline(text)?;
        let old = &last.outline;
        if new.bodies.len() != old.bodies.len() || new.names != old.names {
            return None;
        }
        // Everything outside the bodies is as it was, and so is each
        // body's place in the order.
        let (ot, nt) = (last.text.as_str(), text);
        let (mut op, mut np) = (0, 0);
        let mut edited = Vec::new();
        for (i, (ob, nb)) in old.bodies.iter().zip(&new.bodies).enumerate() {
            if ot.get(op..ob.inner.0)? != nt.get(np..nb.inner.0)? || ob.key != nb.key {
                return None;
            }
            if ot[ob.inner.0..ob.inner.1] != nt[nb.inner.0..nb.inner.1] {
                edited.push(i);
            }
            (op, np) = (ob.inner.1, nb.inner.1);
        }
        if ot.get(op..)? != nt.get(np..)? {
            return None;
        }
        // An edited body names the same items it did.
        for &i in &edited {
            let (ob, nb) = (&old.bodies[i], &new.bodies[i]);
            if nb.fixed {
                return None;
            }
            let named = |t: &str, b: &Body| -> HashSet<String> {
                words(&t[b.inner.0..b.inner.1])
                    .into_iter()
                    .filter(|w| new.names.contains(*w))
                    .map(str::to_string)
                    .collect()
            };
            if named(ot, ob) != named(nt, nb) {
                return None;
            }
        }
        // A body the edited ones may call keeps inferring what it did: it
        // stays, or is emptied to a body that infers the same
        // ([`fill_for`]). Any other body cannot reach them and is emptied.
        let edges = call_edges(nt, &new);
        let reached = reach(&edges, &edited);
        // A body that may call back into an edited one is in a recursion
        // with it, which the effect checker reports on the group: it stays.
        let mut back: Vec<Vec<usize>> = vec![Vec::new(); edges.len()];
        for (i, out) in edges.iter().enumerate() {
            for &j in out {
                back[j].push(i);
            }
        }
        let callers = reach(&back, &edited);
        let fill = |i: usize, b: &Body| -> Option<&'static [u8]> {
            if edited.contains(&i) || (reached.contains(&i) && callers.contains(&i)) {
                None
            } else if reached.contains(&i) {
                fill_for(last.effects.get(&b.key).map(String::as_str))
            } else {
                Some(b"loop {}")
            }
        };
        let (focused, emptied_bodies) = match emptied(nt, &new, fill) {
            Some((t, done)) => (Some(t), done),
            None => (None, Vec::new()),
        };
        let filled: Vec<usize> = emptied_bodies
            .into_iter()
            .filter(|i| reached.contains(i))
            .collect();
        if std::env::var("KARAC_LSP_INCREMENTAL_DEBUG").is_ok() {
            eprintln!(
                "[incremental] edited {:?}: {} of {} bodies reachable, {} of them emptied",
                edited
                    .iter()
                    .map(|&i| &new.bodies[i].key)
                    .collect::<Vec<_>>(),
                reached.len(),
                new.bodies.len(),
                filled.len()
            );
        }
        let t0 = std::time::Instant::now();
        let (fdiags, feffects) =
            super::editor::check_with_effects(focused.as_deref().unwrap_or(nt));
        if std::env::var("KARAC_LSP_INCREMENTAL_DEBUG").is_ok() {
            eprintln!(
                "[incremental] focused check {:.0} ms",
                t0.elapsed().as_secs_f64() * 1e3
            );
        }
        // An edited function infers the effects it did, and so does every
        // emptied one, so no caller's inference moved.
        for &i in edited.iter().chain(&filled) {
            let key = &new.bodies[i].key;
            if feffects.get(key)? != last.effects.get(key)? {
                return None;
            }
        }
        // Old offsets to new: each edited body before an offset moves it by
        // that body's change in length.
        let shift = |at: usize| -> usize {
            let mut d: isize = 0;
            for &i in &edited {
                let (ob, nb) = (&old.bodies[i], &new.bodies[i]);
                if ob.inner.1 <= at {
                    d += (nb.inner.1 - nb.inner.0) as isize - (ob.inner.1 - ob.inner.0) as isize;
                }
            }
            (at as isize + d) as usize
        };
        let in_edited = |bodies: &[Body], at: usize| {
            edited
                .iter()
                .any(|&i| bodies[i].item.0 <= at && at < bodies[i].item.1)
        };
        let mut diags: Vec<PlaygroundDiagnostic> = Vec::new();
        for d in &last.diags {
            if in_edited(&old.bodies, d.offset) {
                continue;
            }
            let mut d = d.clone();
            let end = shift(d.offset + d.length);
            d.offset = shift(d.offset);
            d.length = end.saturating_sub(d.offset);
            for f in &mut d.fixes {
                f.offset = shift(f.offset);
            }
            diags.push(d);
        }
        diags.extend(
            fdiags
                .into_iter()
                .filter(|d| in_edited(&new.bodies, d.offset)),
        );
        let index = LineStarts::new(nt);
        for d in &mut diags {
            (d.line, d.column) = index.line_col(nt, d.offset);
        }
        let effects = last.effects.clone();
        self.last = Some(Snapshot {
            text: nt.to_string(),
            diags: diags.clone(),
            outline: new,
            effects,
        });
        Some((diags, CheckKind::Incremental))
    }
}

/// 1-based line and column (in characters) of a byte offset, as
/// [`PlaygroundDiagnostic`] counts them.
struct LineStarts(Vec<usize>);

impl LineStarts {
    fn new(text: &str) -> Self {
        let mut v = vec![0];
        v.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self(v)
    }

    fn line_col(&self, text: &str, at: usize) -> (usize, usize) {
        let line = self.0.partition_point(|&s| s <= at).max(1);
        let start = self.0[line - 1];
        let col = text
            .get(start..at.min(text.len()))
            .map_or(0, |s| s.chars().count());
        (line, col + 1)
    }
}
