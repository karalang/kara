//! Book-snippet test harness — compile every fenced code block in The Kāra
//! Book with the in-tree compiler.
//!
//! The book (`docs/book/src`, published at karalang.org/book/) teaches the
//! language by example. Its code blocks are a promise: this is what the
//! compiler accepts and does. Without a test, that promise drifts silently —
//! a breaking language change lands, the book still says the old thing, and
//! nobody notices until a reader hits the wall. This harness is the
//! katas-are-bug-finders discipline applied to the book: every snippet is a
//! test case, and book drift fails the build.
//!
//! It runs under the default `cargo test --all` job (no `--features llvm`
//! needed) — the same job CI already runs — so no workflow wiring is required.
//!
//! ## Fence-info vocabulary
//!
//! A ```` ```kara ```` fence's info string selects how the block is exercised;
//! annotations are comma-separated after `kara`:
//!
//! | Fence | Meaning |
//! |---|---|
//! | ```` ```kara ```` | Must pass the full **static** pipeline (parse → desugar → resolve → typecheck → effect → ownership), the same gate `karac check` / the LSP use. Not executed. |
//! | ```` ```kara,run ```` | Must additionally **execute** cleanly on the interpreter — every phase clean *and* no runtime error (`run_playground(...).ok`). |
//! | ```` ```kara,norun ```` | Static-only, and explicitly *never* executed even though it may have a `main` (e.g. it would block on I/O or loop forever). Same gate as plain `kara`; the annotation documents intent. |
//! | ```` ```kara,ignore ```` | Skipped entirely — an illustrative fragment that is not a standalone compilable unit (a bare signature, a `// ...`-elided body, a snippet that references names defined elsewhere in the chapter's prose). |
//!
//! Non-`kara` fences (```` ```text ````, ```` ```toml ````, ```` ```rust ````,
//! …) are ignored.
//!
//! ## Auto-wrap (mdbook-style)
//!
//! A teaching book shows bare statement sequences — `let x = 5; println(x);` —
//! in prose about variables, without a `fn main` wrapper. At module scope those
//! are rejected (`let x = …` introduces a Const-class binding, so a lowercase
//! name is `E_MODULE_BINDING_NAMING`). Rather than mass-`ignore` every such
//! fragment (which would gut the gate) or pad the rendered page with a `main`
//! on every snippet, the harness follows `mdbook test`'s convention: if a block
//! fails the static check as written **and** it declares no `fn main`, it is
//! retried wrapped in `fn main() { … }`. A block passes if **either** form is
//! clean. Item-only blocks (a bare `struct`/`fn`/`impl`) pass as-is (items
//! can't nest in a `fn` body, so the wrapped retry can't rescue — nor does it
//! need to). Genuine fragments (`...`-elided bodies, names defined only in the
//! surrounding prose) fail both forms and are annotated `kara,ignore`.
//!
//! ## v1 boundary
//!
//! `kara,run` asserts the program *executes cleanly*, not that its stdout
//! matches a pinned expected value — the book currently pairs no code block
//! with an output block, and threading `// out:` markers into 150 prose
//! snippets would pollute the rendered page. Output-pinning (via a companion
//! ```` ```text ```` output block or an out-of-band expectations table) is a
//! deliberate follow-on; the drift gate that matters most — "every snippet
//! still compiles, and the runnable ones still run" — is what v1 delivers.

use karac::{check_source, run_playground};
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// How a `kara` block is exercised, selected by its fence-info annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Plain `kara` — full static pipeline must pass; not executed.
    Typecheck,
    /// `kara,run` — static pipeline + clean interpreter execution.
    Run,
    /// `kara,norun` — static-only, explicitly never executed.
    NoRun,
    /// `kara,ignore` — skipped (illustrative fragment).
    Ignore,
}

/// One fenced `kara` block extracted from a chapter.
struct Snippet {
    chapter: String,
    /// 1-based line of the opening fence, so a failure points at the source.
    line: usize,
    class: Class,
    code: String,
}

fn book_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/book/src")
}

/// Classify a fence info string. Returns `None` for a non-`kara` fence (which
/// the extractor then skips as an opaque block). The first comma-separated
/// token is the language; the rest are annotations.
fn classify(info: &str) -> Option<Class> {
    let mut parts = info.split(',').map(str::trim);
    if parts.next()? != "kara" {
        return None;
    }
    let mut class = Class::Typecheck;
    for ann in parts {
        match ann {
            "ignore" => return Some(Class::Ignore), // ignore wins outright
            "run" => class = Class::Run,
            "norun" => class = Class::NoRun,
            _ => {} // unknown / empty annotation — keep the default
        }
    }
    Some(class)
}

/// Fence scanner state. Fence length is tracked so a longer fence (```` ````
/// used in appendix-d to *show* ```` ``` ```` examples) contains the shorter
/// one as content rather than closing on it.
enum State {
    Outside,
    InKara {
        fence_len: usize,
        class: Class,
        start: usize,
        code: String,
    },
    /// A non-`kara` fenced block whose content must be skipped so a
    /// ```` ```text ```` block containing ```` ```kara ````-looking text is
    /// not misparsed.
    InOther {
        fence_len: usize,
    },
}

/// Count leading backticks on a line (fences in this book start at column 0).
fn leading_backticks(line: &str) -> usize {
    line.bytes().take_while(|&b| b == b'`').count()
}

/// Extract every `kara` fenced block from one chapter's markdown.
fn extract(chapter: &str, md: &str) -> Vec<Snippet> {
    let mut out = Vec::new();
    let mut state = State::Outside;

    for (idx, line) in md.lines().enumerate() {
        let lineno = idx + 1;
        let bt = leading_backticks(line);
        let after = line[bt..].trim();
        // A closing fence is a run of >= the opening length with nothing after.
        let is_bare_fence = bt >= 3 && after.is_empty();

        match &mut state {
            State::Outside => {
                if bt >= 3 {
                    match classify(after) {
                        Some(class) => {
                            state = State::InKara {
                                fence_len: bt,
                                class,
                                start: lineno,
                                code: String::new(),
                            };
                        }
                        None => state = State::InOther { fence_len: bt },
                    }
                }
            }
            State::InKara {
                fence_len,
                class,
                start,
                code,
            } => {
                if is_bare_fence && bt >= *fence_len {
                    out.push(Snippet {
                        chapter: chapter.to_string(),
                        line: *start,
                        class: *class,
                        code: std::mem::take(code),
                    });
                    state = State::Outside;
                } else {
                    code.push_str(line);
                    code.push('\n');
                }
            }
            State::InOther { fence_len } => {
                if is_bare_fence && bt >= *fence_len {
                    state = State::Outside;
                }
            }
        }
    }
    out
}

/// Does this block declare its own entry point? If so, the auto-wrap retry is
/// skipped (wrapping a `fn main` inside another `fn main` is nonsense).
fn declares_main(code: &str) -> bool {
    code.lines()
        .map(str::trim_start)
        .any(|l| l.starts_with("fn main(") || l.starts_with("pub fn main("))
}

/// The mdbook-style wrapped form: the block as the body of a synthetic `main`.
fn wrapped(code: &str) -> String {
    format!("fn main() {{\n{code}\n}}")
}

/// Run the static gate with the auto-wrap fallback. Returns the diagnostics of
/// the block *as written* on failure (the form the reader sees), or an empty
/// vec if either the bare or the wrapped form is clean.
fn static_check(code: &str) -> Vec<karac::PlaygroundDiagnostic> {
    let bare = check_source(code);
    if bare.is_empty() {
        return bare;
    }
    if !declares_main(code) {
        let wrapped_diags = check_source(&wrapped(code));
        if wrapped_diags.is_empty() {
            return Vec::new();
        }
    }
    bare
}

/// Render a static-diagnostic failure with enough context to locate and read
/// the offending block without opening the file.
fn format_static_failure(s: &Snippet, diags: &[karac::PlaygroundDiagnostic]) -> String {
    let shown = diags
        .iter()
        .map(|d| format!("    [{}] {} (block line {})", d.phase, d.message, d.line))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}:{} ({:?}) — {} diagnostic(s):\n{}\n  --- snippet ---\n{}",
        s.chapter,
        s.line,
        s.class,
        diags.len(),
        shown,
        indent(&s.code),
    )
}

fn indent(code: &str) -> String {
    code.lines()
        .map(|l| format!("  | {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn book_snippets_compile() {
    let dir = book_src_dir();
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read book dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();

    let mut snippets = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let md = fs::read_to_string(f).unwrap_or_else(|e| panic!("read {name}: {e}"));
        snippets.extend(extract(&name, &md));
    }

    // Guard against a silent extraction regression that would make this test
    // pass vacuously. The book carries ~150 `kara` blocks; if the extractor
    // suddenly finds almost none, the fence scanner is broken, not the book.
    assert!(
        snippets.len() >= 140,
        "extractor found only {} kara blocks across {} chapters — the fence \
         scanner is likely broken (expected ~150)",
        snippets.len(),
        files.len()
    );

    let mut failures = Vec::new();
    let (mut checked, mut ran, mut ignored) = (0usize, 0usize, 0usize);

    for s in &snippets {
        match s.class {
            Class::Ignore => ignored += 1,
            Class::Typecheck | Class::NoRun => {
                checked += 1;
                let diags = static_check(&s.code);
                if !diags.is_empty() {
                    failures.push(format_static_failure(s, &diags));
                }
            }
            Class::Run => {
                checked += 1;
                ran += 1;
                // Execute the form that has an entry point: the block as
                // written if it declares `main`, else the auto-wrapped body.
                let src = if declares_main(&s.code) {
                    s.code.clone()
                } else {
                    wrapped(&s.code)
                };
                match catch_unwind(AssertUnwindSafe(|| run_playground(&src))) {
                    Ok(r) if r.ok => {}
                    Ok(r) => failures.push(format_static_failure(s, &r.diagnostics)),
                    Err(_) => failures.push(format!(
                        "{}:{} (Run) — interpreter PANICKED\n  --- snippet ---\n{}",
                        s.chapter,
                        s.line,
                        indent(&s.code)
                    )),
                }
            }
        }
    }

    eprintln!(
        "book snippets: {} chapters, {checked} checked ({ran} run), {ignored} ignored",
        files.len()
    );

    assert!(
        failures.is_empty(),
        "\n{} book snippet(s) failed to compile/run. Fix the compiler, update \
         the book, or annotate the fence (`kara,ignore` for illustrative \
         fragments, `kara,run` to execute, `kara,norun` for static-only):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
