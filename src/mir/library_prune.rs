//! Leave out the parts of the appended Kāra library a program cannot reach.
//!
//! The MIR pipeline appends the whole library (`lower::library_source`) to
//! every program and then resolves, typechecks and ownership-checks all of
//! it, which was most of a small program's `karac build --mir` time. This
//! pass runs on the parsed program, before any of that, and drops the
//! library methods whose NAME the program never mentions, directly or
//! through a part of the library it keeps, and the impls of library types
//! it never names.
//!
//! Reachability is by word, not by resolved name, so it over-keeps: a
//! program that says `len` anywhere (a comment included) keeps every
//! library method called `len`. What it must never do is drop something
//! reached without its name being written, so these are always kept:
//!
//! - every name in [`ALWAYS_KEPT`], the methods the compiler calls for an
//!   operator, a `for` loop, `?`, an f-string, a derive or a drop;
//! - a trait method without a default body, and every impl's definition of
//!   it (dropping one would leave an impl incomplete);
//! - a method of an impl of a trait the library does not define, since
//!   whether that trait requires it is not known here;
//! - every free function (the compiler calls some by name: `parse_result_int`
//!   decides what `i64.parse` returns) and every item that is not an impl
//!   or a trait (types, consts), which cost little to check;
//! - an impl whose target is not a type the library defines.
//!
//! Spans are left as they are: only whole items and methods are removed.
//! `KARAC_MIR_PRUNE_LIBRARY=0` keeps the whole library.

use crate::ast::{ImplItem, Item, Program, TraitItem, TypeKind};
use rustc_hash::{FxHashMap, FxHashSet};

/// Names the compiler reaches without the program spelling them.
const ALWAYS_KEPT: &[&str] = &[
    // Comparison, hashing, copying, dropping and printing.
    "eq",
    "ne",
    "cmp",
    "partial_cmp",
    "lt",
    "le",
    "gt",
    "ge",
    "min",
    "max",
    "hash",
    // A table's hash glue calls its hasher's (B-2026-10-25-31).
    "write",
    "finish",
    "clone",
    "cloned",
    "drop",
    "fmt",
    "to_string",
    "default",
    // Operators.
    "add",
    "sub",
    "mul",
    "div",
    "rem",
    "neg",
    "not",
    "index",
    "index_mut",
    "contains",
    "range",
    "range_inclusive",
    "Range",
    "RangeInclusive",
    "RangeFrom",
    "Ordering",
    // `for`, `?`, conversions and collecting.
    "iter",
    "iter_mut",
    "into_iter",
    "next",
    "next_back",
    "from",
    "into",
    "try_from",
    "try_into",
    "from_iter",
    "extend",
    "collect",
    "unwrap",
    "expect",
    "ok",
    "err",
    "map_err",
    "len",
    "is_empty",
    "new",
    "push",
    "get",
    "get_mut",
    "insert",
    "Option",
    "Some",
    "None",
    "Result",
    "Ok",
    "Err",
    "IoError",
    "AllocError",
    // Named in the compiler's own source as strings, so possibly reached by a
    // desugaring rather than by the program's text.
    "and",
    "or",
    "chars",
    "clear",
    "concat",
    "count",
    "enumerate",
    "find",
    "first",
    "last",
    "join",
    "keys",
    "values",
    "lines",
    "pop",
    "remove",
    "retain",
    "reverse",
    "split",
    "split_whitespace",
    "strip_prefix",
    "strip_suffix",
    "sum",
    "swap",
    "swap_remove",
    "take",
    "trim",
    "trim_start",
    "trim_end",
    "truncate",
    "unwrap_or",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    // Mutex: the MIR interpreter and coroutine lowering name these (d3124030ef).
    "acquire",
    "release",
    "lock",
    // What a `select` stands for (`crate::select_desugar`), written after
    // this pass reads the program.
    "__closed",
    "try_recv",
    "try_send",
    // The builder recognises a `?` on `Result.context(msg)` by name, to put
    // `msg` in the error return trace (B-2026-10-25-44). It calls nothing,
    // and a program that writes `.context(` keeps it anyway.
    "context",
];

pub fn enabled() -> bool {
    std::env::var("KARAC_MIR_PRUNE_LIBRARY").as_deref() != Ok("0")
}

/// The identifier-shaped words of `text`.
fn words(text: &str, out: &mut FxHashSet<String>) {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            if !out.contains(&text[start..i]) {
                out.insert(text[start..i].to_string());
            }
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}

fn last_segment(ty: &crate::ast::TypeExpr) -> Option<&str> {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.last().map(String::as_str),
        _ => None,
    }
}

/// One method of a library impl or trait, and the text it covers.
struct Method {
    name: String,
    text: (usize, usize),
    /// Kept whatever the program says (see the module docs).
    pinned: bool,
    kept: bool,
}

/// One library item.
struct Piece {
    /// The text before the first method (all of it for a non-container).
    head: (usize, usize),
    kind: Kind,
    kept: bool,
    methods: Vec<Method>,
}

enum Kind {
    /// An impl of a type the library defines, kept when that name is reached.
    ImplOf(String),
    /// A type the library defines: always left in, but what it names counts
    /// only once the type itself is reached.
    Type(String),
    /// Kept as a frame; its methods are judged one by one.
    Always,
}

/// The inherent library methods [`prune`] left out, by rendered impl target
/// (`Option[T]`): the typechecker still counts them as restating a baked
/// impl (`TypeChecker::pruned_library_methods`).
pub type Pruned = std::collections::HashMap<String, std::collections::HashSet<String>>;

/// Remove the library items and methods `src[..user_len]` cannot reach.
pub fn prune(program: &mut Program, src: &str, user_len: usize) -> Pruned {
    let mut pruned = Pruned::new();
    // The library's items of the kinds judged here, in text order.
    let lib: Vec<usize> = (0..program.items.len())
        .filter(|&i| item_offset(&program.items[i]).is_some_and(|o| o >= user_len))
        .collect();
    if lib.is_empty() {
        return pruned;
    }
    let starts: Vec<usize> = lib
        .iter()
        .map(|&i| item_offset(&program.items[i]).unwrap_or(user_len))
        .collect();
    if starts.windows(2).any(|w| w[0] > w[1]) {
        return pruned;
    }
    let lib_items = || lib.iter().map(|&i| &program.items[i]);
    let library_types: FxHashSet<&str> = lib_items()
        .filter_map(|it| match it {
            Item::StructDef(s) => Some(s.name.as_str()),
            Item::EnumDef(e) => Some(e.name.as_str()),
            _ => None,
        })
        .collect();
    // For each library trait, its methods with a default body: an impl's
    // definition of one of those may go when the name is not reached.
    let mut defaults: FxHashMap<&str, FxHashSet<&str>> = FxHashMap::default();
    for it in lib_items() {
        if let Item::TraitDef(t) = it {
            let set = defaults.entry(t.name.as_str()).or_default();
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    if m.body.is_some() {
                        set.insert(m.name.as_str());
                    }
                }
            }
        }
    }

    let mut pieces: Vec<Piece> = Vec::new();
    for (k, it) in lib_items().enumerate() {
        let start = starts[k];
        let end = starts.get(k + 1).copied().unwrap_or(src.len()).max(start);
        let mut method_spans: Vec<(String, usize, bool)> = Vec::new();
        let kind = match it {
            // The compiler calls several by name (`parse_result_int` decides
            // what `i64.parse` returns), and they are few.
            Item::Function(_) => Kind::Always,
            Item::ImplBlock(imp) => {
                let trait_defaults = imp
                    .trait_name
                    .as_ref()
                    .map(|p| defaults.get(p.segments.last().map_or("", String::as_str)));
                for ii in &imp.items {
                    if let ImplItem::Method(m) = ii {
                        let pinned = match trait_defaults {
                            // An inherent method.
                            None => false,
                            // A trait the library does not define.
                            Some(None) => true,
                            Some(Some(d)) => !d.contains(m.name.as_str()),
                        };
                        method_spans.push((m.name.clone(), m.span.offset, pinned));
                    }
                }
                match last_segment(&imp.target_type) {
                    Some(t) if library_types.contains(t) => Kind::ImplOf(t.to_string()),
                    _ => Kind::Always,
                }
            }
            Item::StructDef(d) => Kind::Type(d.name.clone()),
            Item::EnumDef(d) => Kind::Type(d.name.clone()),
            Item::TraitDef(t) => {
                for ti in &t.items {
                    if let TraitItem::Method(m) = ti {
                        method_spans.push((m.name.clone(), m.span.offset, m.body.is_none()));
                    }
                }
                Kind::Always
            }
            _ => Kind::Always,
        };
        // A method's text runs to the next method, the last to the item's end.
        let ordered = method_spans
            .windows(2)
            .all(|w| w[0].1 <= w[1].1 && w[0].1 >= start);
        if !ordered || method_spans.last().is_some_and(|m| m.1 > end) {
            // Not laid out as expected: keep the item whole.
            pieces.push(Piece {
                head: (start, end),
                kind: Kind::Always,
                kept: false,
                methods: Vec::new(),
            });
            continue;
        }
        let head_end = method_spans.first().map_or(end, |m| m.1);
        let methods = method_spans
            .iter()
            .enumerate()
            .map(|(j, (name, off, pinned))| Method {
                name: name.clone(),
                text: (*off, method_spans.get(j + 1).map_or(end, |n| n.1)),
                pinned: *pinned,
                kept: false,
            })
            .collect();
        pieces.push(Piece {
            head: (start, head_end),
            kind,
            kept: false,
            methods,
        });
    }

    let mut reached: FxHashSet<String> = ALWAYS_KEPT.iter().map(|s| s.to_string()).collect();
    words(&src[..user_len.min(src.len())], &mut reached);
    let text = |r: (usize, usize)| src.get(r.0..r.1).unwrap_or("");
    loop {
        let mut changed = false;
        for p in pieces.iter_mut() {
            if !p.kept {
                p.kept = match &p.kind {
                    Kind::ImplOf(n) | Kind::Type(n) => reached.contains(n),
                    Kind::Always => true,
                };
                if p.kept {
                    changed = true;
                    words(text(p.head), &mut reached);
                }
            }
            if !p.kept {
                continue;
            }
            for m in p.methods.iter_mut() {
                if !m.kept && (m.pinned || reached.contains(&m.name)) {
                    m.kept = true;
                    changed = true;
                    words(text(m.text), &mut reached);
                }
            }
        }
        if !changed {
            break;
        }
    }

    if std::env::var("KARAC_MIR_PRUNE_DEBUG").as_deref() == Ok("1") {
        for p in &pieces {
            let kept: Vec<&str> = p
                .methods
                .iter()
                .filter(|m| m.kept && p.kept)
                .map(|m| m.name.as_str())
                .collect();
            eprintln!(
                "[prune] {} {:?} kept {}/{} {:?}",
                if p.kept { "KEEP" } else { "DROP" },
                text((p.head.0, p.head.0 + 40.min(p.head.1 - p.head.0)))
                    .lines()
                    .next()
                    .unwrap_or(""),
                kept.len(),
                p.methods.len(),
                kept
            );
        }
    }
    let mut drop_item: FxHashSet<usize> = FxHashSet::default();
    for (p, &i) in pieces.iter().zip(&lib) {
        if let Item::ImplBlock(imp) = &program.items[i] {
            if imp.trait_name.is_none() && p.methods.iter().any(|m| !(m.kept && p.kept)) {
                pruned
                    .entry(crate::formatter::render_type_expr(&imp.target_type))
                    .or_default()
                    .extend(
                        p.methods
                            .iter()
                            .filter(|m| !(m.kept && p.kept))
                            .map(|m| m.name.clone()),
                    );
            }
        }
        if !p.kept && !matches!(p.kind, Kind::Type(_)) {
            drop_item.insert(i);
            continue;
        }
        if p.methods.iter().all(|m| m.kept) {
            continue;
        }
        let keep: FxHashSet<usize> = p
            .methods
            .iter()
            .filter(|m| m.kept)
            .map(|m| m.text.0)
            .collect();
        match &mut program.items[i] {
            Item::ImplBlock(imp) => imp.items.retain(|ii| match ii {
                ImplItem::Method(m) => keep.contains(&m.span.offset),
                _ => true,
            }),
            Item::TraitDef(t) => t.items.retain(|ti| match ti {
                TraitItem::Method(m) => keep.contains(&m.span.offset),
                _ => true,
            }),
            _ => {}
        }
    }
    let mut i = 0;
    program.items.retain(|_| {
        i += 1;
        !drop_item.contains(&(i - 1))
    });
    pruned
}

fn item_offset(it: &Item) -> Option<usize> {
    Some(match it {
        Item::Function(f) => f.span.offset,
        Item::StructDef(s) => s.span.offset,
        Item::EnumDef(e) => e.span.offset,
        Item::TraitDef(t) => t.span.offset,
        Item::ImplBlock(i) => i.span.offset,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// A library method the MIR builder or backend names in a string literal
    /// may be called without the program spelling it, so pruning must never
    /// drop it: every such name is in [`ALWAYS_KEPT`].
    #[test]
    fn library_methods_named_by_the_compiler_are_always_kept() {
        let mut defined: FxHashSet<String> = FxHashSet::default();
        for (_, text) in crate::prelude::LIBRARY_SOURCES {
            let mut rest = *text;
            while let Some(i) = rest.find("fn ") {
                let name: String = rest[i + 3..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                // A free function (`fn` at the start of a line) is always
                // kept, so only methods count.
                let at_line_start = text[..text.len() - rest.len() + i]
                    .rsplit('\n')
                    .next()
                    .is_some_and(|l| l.trim_start() == l && (l.is_empty() || l == "pub "));
                if !name.is_empty() && !at_line_start {
                    defined.insert(name);
                }
                rest = &rest[i + 3..];
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![root.join("typed_hir.rs")];
        rust_files(&root.join("mir"), &mut files);
        rust_files(&root.join("backend/mir"), &mut files);
        let kept: FxHashSet<&str> = ALWAYS_KEPT.iter().copied().collect();
        let mut missing: Vec<String> = Vec::new();
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            // Every `"ident"` in the file, wherever it sits.
            let b = src.as_bytes();
            for (i, _) in src.match_indices('"') {
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if j > i + 1 && j < b.len() && b[j] == b'"' {
                    let lit = &src[i + 1..j];
                    if defined.contains(lit)
                        && !kept.contains(lit)
                        && !missing.iter().any(|m| m == lit)
                    {
                        missing.push(lit.to_string());
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "library methods the compiler names but ALWAYS_KEPT lacks: {missing:?}"
        );
    }
}
