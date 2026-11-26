//! The library modules a program imports before it uses their items:
//! `std.regex`, `std.json` (docs/library/README.md; design.md, "everything
//! else is under `std.*`"; B-2026-10-21-56) and the deferred `std.cli`
//! (deferred.md; B-2026-11-08-29). Their items stay globals of the
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
    // library/README.md § The prelude: these are imported from their
    // `std.*` module, never in the prelude; the compiler kept them in scope
    // anyway (review ruling 2026-11-26, step 1: the names README's module
    // table assigns).
    (
        "std.sync",
        &[
            "Atomic",
            "Mutex",
            "Sender",
            "Receiver",
            "SendError",
            "TaskGroup",
            "TaskHandle",
            // Step 2: the names the module table did not list.
            "Semaphore",
            "SemaphoreError",
            "MemoryOrdering",
            "RateLimiter",
            "BoundedChannel",
            "OnFull",
            "ChannelError",
            "IsolatingGroup",
            "TaskFailed",
            "Channel",
            "OnceLock",
            "OnceCell",
            "AlreadySetError",
            "LazyLock",
        ],
    ),
    (
        "std.hash",
        &["SipHash13BuildHasher", "FxBuildHasher", "StableHash"],
    ),
    (
        "std.io",
        &[
            "File",
            "OpenOptions",
            "SeekFrom",
            "BufReader",
            "BufWriter",
            "Metadata",
            "LinesIter",
            "StdinLines",
            "StdoutProvider",
            "StderrProvider",
            "StdinProvider",
            "FileSystemProvider",
        ],
    ),
    (
        "std.net",
        &[
            "TcpListener",
            "TcpStream",
            "TcpError",
            "Client",
            "Request",
            "RequestBuilder",
            "Response",
            "HttpError",
            "Server",
            "WebSocket",
            "TlsConfig",
            "TlsListener",
            "TlsStream",
            "TlsError",
        ],
    ),
    ("std.time", &["ClockProvider"]),
    ("std.env", &["EnvProvider"]),
    ("std.random", &["RandomProvider"]),
    ("std.cmp", &["F32", "F64"]),
    ("std.arena", &["Arena", "ArenaRef", "ArenaCheckpoint"]),
    ("std.intern", &["Symbol", "Interner"]),
    // Experimental modules (library/README.md).
    ("std.simd", &["Vector"]),
    ("std.gpu", &["GpuBuffer"]),
    ("std.embedded", &["VolatileCell", "CriticalSectionGuard"]),
    (
        "std.tracing",
        &[
            "Span",
            "LogEvent",
            "SpanField",
            "NoOpExporter",
            "StdoutExporter",
            "Log",
        ],
    ),
    ("std.protobuf", &["ProtoBuf", "ProtoReader"]),
    ("std.encoding", &["DecodeError"]),
    (
        "std.runtime",
        &["TaskInfo", "ParBlockInfo", "WaitTarget", "Runtime"],
    ),
    (
        "std.process",
        &[
            "Command",
            "Stdio",
            "Child",
            "ChildStdin",
            "ChildStdout",
            "ChildStderr",
            "ExitStatus",
        ],
    ),
    ("std.regex", &["Regex", "Match", "Captures", "RegexError"]),
    ("std.json", &["Json", "JsonError"]),
    // deferred.md § std.cli: `import std.cli`. The row types the parser
    // stores (`ArgEntry`, `FlagEntry`, `ParsedValue`) are its items too, so
    // a program's own type of one of those names shadows nothing it can see
    // unimported (B-2026-11-08-29).
    (
        "std.cli",
        &[
            "Parser",
            "Arg",
            "Args",
            "CliError",
            "Subcommand",
            "SubcommandResult",
            "ArgEntry",
            "FlagEntry",
            "ParsedValue",
        ],
    ),
];

/// Names the library's own source uses that are no item a program can name
/// (the review's prelude ruling, 2026-11-26): each with the spelling a
/// program writes instead, when there is one.
pub const INTERNAL_LIBRARY_NAMES: &[(&str, Option<&str>)] = &[
    // `Hash` impls feed their bytes to it; a program hashes through `Hasher`.
    ("KeyByteSink", None),
    // `std.process`'s environment pairs; a program reads `env.var(name)`.
    ("EnvVar", None),
    // The compiler's name for the borrowed string view, which programs
    // spell `Str` (strings.md).
    ("StringSlice", Some("Str")),
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
/// nor a bare use of such a variant's name (a pattern `Match(n)`), and
/// neither is a field or method of that name.
pub fn missing_library_imports(src: &str) -> Vec<TypeError> {
    let parsed = crate::parse_shared(src);
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
            // `effect resource Log;`: a program's own resource of a library
            // item's name, as its own type is.
            Item::EffectResource(r) => {
                declared.insert(r.name.clone());
            }
            Item::EffectGroup(g) => {
                declared.insert(g.name.clone());
            }
            Item::UnionDef(u) => {
                declared.insert(u.name.clone());
            }
            Item::TraitAlias(t) => {
                declared.insert(t.name.clone());
            }
            Item::MarkerTrait(t) => {
                declared.insert(t.name.clone());
            }
            Item::ConstDecl(c) => {
                declared.insert(c.name.clone());
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
    let mut internal: Vec<TypeError> = Vec::new();
    let mut in_import = false;
    for (i, t) in tokens.iter().enumerate() {
        match &t.token {
            Token::Import => in_import = true,
            Token::Semicolon => in_import = false,
            Token::Identifier { name, raw: false } if !in_import => {
                // A bare use of a variant's name is the variant (a pattern
                // `Match(n)`, a constructor), unless a path goes on from it.
                let bare_variant = variants.iter().any(|(_, v)| v == name)
                    && tokens.get(i + 1).is_none_or(|n| n.token != Token::Dot);
                if i > 0 && tokens[i - 1].token == Token::Dot
                    || declared.contains(name)
                    || variant_names.contains(&t.span.offset)
                    || bare_variant
                {
                    continue;
                }
                if let Some((_, instead)) = INTERNAL_LIBRARY_NAMES
                    .iter()
                    .find(|(n, _)| *n == name.as_str())
                {
                    internal.push(internal_name_error(name, *instead, t.span));
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
        return internal;
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
                     imports before it uses it: `{import}` ({})",
                if module == "std.cli" {
                    "deferred.md § std.cli"
                } else {
                    "docs/library/README.md"
                }
            ),
            span,
            kind: TypeErrorKind::RemovedInCore,
            lint_name: None,
            fix_it: Some(fix_it.clone()),
            class: None,
            expected: None,
            got: None,
            help: None,
        })
        .chain(internal)
        .collect()
}

/// A use of an [`INTERNAL_LIBRARY_NAMES`] name: refused, with the rename to
/// the spelling a program writes when there is one.
fn internal_name_error(name: &str, instead: Option<&str>, span: Span) -> TypeError {
    let message = match instead {
        Some(to) => format!(
            "`{name}` is internal to the library and no program names it: write `{to}` \
             (docs/library/README.md)"
        ),
        None => format!(
            "`{name}` is internal to the library and no program names it \
             (docs/library/README.md)"
        ),
    };
    TypeError {
        message,
        span,
        kind: TypeErrorKind::RemovedInCore,
        lint_name: None,
        fix_it: instead.map(|to| FixIt {
            span,
            replacement: to.to_string(),
        }),
        class: None,
        expected: None,
        got: None,
        help: None,
    }
}

/// The offset just past the newline that ends the line holding `at`, or the
/// end of `src`.
pub(crate) fn line_end(src: &str, at: usize) -> usize {
    match src.get(at..).and_then(|rest| rest.find('\n')) {
        Some(i) => at + i + 1,
        None => src.len(),
    }
}

/// The start of the first line that is neither blank nor a `//` comment: a
/// program's opening comment stays above the imports a fix adds.
pub(crate) fn above_first_item(src: &str) -> usize {
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
