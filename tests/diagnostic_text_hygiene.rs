//! Guard against rustfmt freezing continuation indentation into a message.
//!
//! # The recurring failure
//!
//! A `\`-continued Rust string literal is supposed to drop the newline *and*
//! the following indentation, which is what lets a long diagnostic be written
//! across several source lines and still render as one sentence. rustfmt in
//! this repo sometimes rejoins such a literal onto ONE line first — at which
//! point the indentation is ordinary spaces inside the string, and nothing
//! strips it. The shipped message then reads
//!
//! ```text
//! `gpu.matmul` requires a rank-2 left                      operand, found rank 3
//! ```
//!
//! B-2026-08-21-45 found six of these across the compiler and the runtime. The
//! class bit three separate times while fixing B-2026-08-21-41 alone.
//!
//! # Why `cargo fmt --check` cannot catch it
//!
//! It is not a formatting violation. The source **is** correctly formatted —
//! rustfmt produced it and is idempotent on it. The STRING is what is wrong,
//! and no formatter has an opinion about string contents. Nor is it visible in
//! review: the corrupted literal and an intact one look alike in a diff unless
//! you count spaces. `src/codegen/provider.rs` carries both spellings of
//! nearly the same message, one rejoined and one not.
//!
//! # How this test separates corruption from deliberate alignment
//!
//! Help text, `--flag <desc>` tables and report printers legitimately contain
//! long runs of spaces, so a bare "message has many spaces" grep is useless —
//! it fires on ~80 innocent sites. Two conditions together pick out exactly the
//! rejoins, with no false positives across `src/`, `runtime/src/` and `hash/src`:
//!
//! 1. **A run of [`MIN_RUN`]+ spaces mid-line in the RENDERED string.** The
//!    literal is decoded the way rustc does — crucially, `\` + newline eats the
//!    newline and the following indentation — so an intact continuation
//!    produces nothing and only a rejoined one leaves spaces behind. A run at
//!    the start of a rendered line is deliberate indentation and is ignored.
//! 2. **The spaces sit on a source line past [`MAX_SOURCE_LINE`].** This is the
//!    signature of the rejoin itself: rustfmt cannot split a string literal, so
//!    when it collapses a continued one it emits a line far over `max_width`.
//!    Hand-written aligned tables never need such a line — every legitimate
//!    site measured 10–72 columns, every corrupted one 120–262.
//!
//! # If this test fires
//!
//! Rebuild the message with `concat!(...)`, which is folded at compile time and
//! so cannot pick up indentation. Two cases need a different remedy:
//!
//! * **`format!` with implicit captures** (`{name}` rather than `{}`) — those
//!   require a literal format string and do not survive `concat!`. Switch to
//!   positional arguments, as `src/codegen/method_call.rs` does.
//! * **A string in ATTRIBUTE position**, e.g. `#[allow(.., reason = "..")]` —
//!   `concat!` is not expanded there at all. Collapse the run in place and keep
//!   the literal on one line; with no continuation left there is nothing to
//!   rejoin.
//!
//! If a hit is genuinely deliberate alignment that happens to need a very long
//! source line, prefer restructuring the literal over widening the thresholds:
//! every relaxation here re-opens the class this test exists to close.

use std::path::{Path, PathBuf};

/// Shortest interior space run treated as suspicious.
///
/// Corrupted runs measured 18–34 spaces (the continuation's indent depth, so in
/// practice at least two nesting levels). The widest innocent run on an
/// over-long source line was 7, in a WGSL template's column alignment.
const MIN_RUN: usize = 8;

/// rustfmt's default `max_width`. A line past this holds a literal rustfmt gave
/// up on splitting — which is exactly how the rejoin presents.
const MAX_SOURCE_LINE: usize = 100;

/// Crate source roots whose string literals reach a user as text.
///
/// `tests/` is deliberately excluded: its literals are embedded `.kara` fixture
/// programs, where a run of spaces inside a one-line program is ordinary.
const SCANNED_ROOTS: &[&str] = &["src", "runtime/src", "hash/src"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// One decoded string literal: rendered bytes, plus each byte's source offset.
struct Literal {
    bytes: Vec<u8>,
    offsets: Vec<usize>,
}

/// Length of the raw-string token starting at `i`, if one does.
fn raw_string_len(src: &[u8], i: usize) -> Option<usize> {
    if i > 0 && (src[i - 1].is_ascii_alphanumeric() || src[i - 1] == b'_') {
        return None;
    }
    let mut j = i;
    if src.get(j) == Some(&b'b') {
        j += 1;
    }
    if src.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let hash_start = j;
    while src.get(j) == Some(&b'#') {
        j += 1;
    }
    let hashes = j - hash_start;
    if src.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    // Closing delimiter is `"` followed by the same number of `#`.
    while j < src.len() {
        if src[j] == b'"'
            && src[j + 1..]
                .iter()
                .take(hashes)
                .filter(|c| **c == b'#')
                .count()
                == hashes
        {
            return Some(j + 1 + hashes - i);
        }
        j += 1;
    }
    Some(src.len() - i)
}

/// Length of the char-literal token at `i`, or 1 if it is a lifetime tick.
fn char_or_lifetime_len(src: &[u8], i: usize) -> usize {
    if src.get(i + 1) == Some(&b'\\') {
        // `'\n'`, `'\''`, `'\u{2014}'` — scan for the closing tick.
        let start = (i + 2).min(src.len());
        let window = &src[start..(i + 14).min(src.len())];
        return match window.iter().position(|c| *c == b'\'') {
            Some(k) => k + 3,
            None => 1,
        };
    }
    if src.get(i + 2) == Some(&b'\'') {
        return 3; // 'x'
    }
    1 // lifetime
}

/// Parse hex digits to a byte, for `\xNN` and ASCII-range `\u{..}`. Non-ASCII
/// scalars return `None` and are stood in for by a non-space placeholder, which
/// is all the scan needs of them.
fn hex_byte(digits: Option<&[u8]>) -> Option<u8> {
    let text = std::str::from_utf8(digits?).ok()?;
    let value = u32::from_str_radix(text, 16).ok()?;
    (value < 0x80).then_some(value as u8)
}

/// Decode every ordinary (non-raw) string literal in `src`, skipping comments,
/// raw strings and char literals so their contents never reach the scan.
fn string_literals(src: &[u8]) -> Vec<Literal> {
    let n = src.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if src[i] == b'/' && src.get(i + 1) == Some(&b'/') {
            i = match src[i..].iter().position(|c| *c == b'\n') {
                Some(k) => i + k + 1,
                None => n,
            };
            continue;
        }
        if src[i] == b'/' && src.get(i + 1) == Some(&b'*') {
            let (mut depth, mut j) = (1usize, i + 2);
            while j < n && depth > 0 {
                if src[j..].starts_with(b"/*") {
                    depth += 1;
                    j += 2;
                } else if src[j..].starts_with(b"*/") {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            i = j;
            continue;
        }
        if let Some(len) = raw_string_len(src, i) {
            i += len;
            continue;
        }
        if src[i] == b'\'' {
            i += char_or_lifetime_len(src, i);
            continue;
        }
        if src[i] == b'"' {
            let mut lit = Literal {
                bytes: Vec::new(),
                offsets: Vec::new(),
            };
            let mut j = i + 1;
            while j < n && src[j] != b'"' {
                if src[j] != b'\\' {
                    lit.bytes.push(src[j]);
                    lit.offsets.push(j);
                    j += 1;
                    continue;
                }
                let esc = src.get(j + 1).copied().unwrap_or(b'\0');
                let (byte, next) = match esc {
                    // Line continuation: newline AND following indentation are
                    // dropped. This is the case that must NOT leave spaces.
                    b'\n' => {
                        j += 2;
                        while j < n && (src[j] == b' ' || src[j] == b'\t') {
                            j += 1;
                        }
                        continue;
                    }
                    b'n' => (b'\n', j + 2),
                    b't' => (b'\t', j + 2),
                    b'r' => (b'\r', j + 2),
                    b'0' => (b'\0', j + 2),
                    // `\x20` IS a space; decoding it to a placeholder would
                    // misread an indented rendered line as mid-sentence text.
                    b'x' => (hex_byte(src.get(j + 2..j + 4)).unwrap_or(b'?'), j + 4),
                    b'u' => {
                        let close = src[j..].iter().position(|c| *c == b'}').map(|k| j + k + 1);
                        let digits = close.and_then(|e| src.get(j + 3..e - 1));
                        (hex_byte(digits).unwrap_or(b'?'), close.unwrap_or(j + 2))
                    }
                    other => (other, j + 2),
                };
                lit.bytes.push(byte);
                lit.offsets.push(j);
                j = next;
            }
            out.push(lit);
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

struct Finding {
    file: String,
    line: usize,
    run: usize,
    source_cols: usize,
    excerpt: String,
}

/// Line number (1-based) and column width of the source line holding `off`.
fn line_at(src: &[u8], off: usize) -> (usize, usize) {
    let start = src[..off]
        .iter()
        .rposition(|c| *c == b'\n')
        .map_or(0, |k| k + 1);
    let end = src[off..]
        .iter()
        .position(|c| *c == b'\n')
        .map_or(src.len(), |k| off + k);
    let cols = String::from_utf8_lossy(&src[start..end]).chars().count();
    (src[..off].iter().filter(|c| **c == b'\n').count() + 1, cols)
}

fn scan_source(label: &str, text: &str) -> Vec<Finding> {
    let src = text.as_bytes();
    let mut findings = Vec::new();
    for lit in string_literals(src) {
        let mut k = 0;
        while k < lit.bytes.len() {
            if lit.bytes[k] != b' ' {
                k += 1;
                continue;
            }
            let start = k;
            while k < lit.bytes.len() && lit.bytes[k] == b' ' {
                k += 1;
            }
            if k - start < MIN_RUN {
                continue;
            }
            // A run opening a rendered line is deliberate indentation.
            let line_start = lit.bytes[..start]
                .iter()
                .rposition(|c| *c == b'\n')
                .map_or(0, |p| p + 1);
            if lit.bytes[line_start..start]
                .iter()
                .all(|c| *c == b' ' || *c == b'\t')
            {
                continue;
            }
            let (line, cols) = line_at(src, lit.offsets[start]);
            if cols <= MAX_SOURCE_LINE {
                continue;
            }
            let lo = start.saturating_sub(30);
            let hi = (k + 30).min(lit.bytes.len());
            findings.push(Finding {
                file: label.to_string(),
                line,
                run: k - start,
                source_cols: cols,
                excerpt: String::from_utf8_lossy(&lit.bytes[lo..hi]).replace('\n', "\\n"),
            });
        }
    }
    findings
}

fn collect_rs(dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, into);
        } else if path.extension().is_some_and(|e| e == "rs") {
            into.push(path);
        }
    }
}

#[test]
fn no_shipped_message_carries_frozen_continuation_indentation() {
    let root = repo_root();
    let mut files = Vec::new();
    for rel in SCANNED_ROOTS {
        collect_rs(&root.join(rel), &mut files);
    }
    assert!(
        files.len() > 100,
        concat!(
            "expected to scan the whole compiler; found only {} files under {:?} ",
            "— has a source root moved?"
        ),
        files.len(),
        SCANNED_ROOTS
    );

    let mut findings = Vec::new();
    files.sort();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let label = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        findings.extend(scan_source(&label, &text));
    }

    if findings.is_empty() {
        return;
    }
    let mut report = format!(
        concat!(
            "{} message(s) carry a run of {}+ spaces mid-sentence, on a source line ",
            "past {} columns.\n\nThis is rustfmt having rejoined a `\\`-continued ",
            "string literal and kept the continuation indentation as real spaces ",
            "(B-2026-08-21-45). Rebuild each with `concat!(...)`; see this file's ",
            "module docs for the two cases that need a different remedy.\n\n"
        ),
        findings.len(),
        MIN_RUN,
        MAX_SOURCE_LINE
    );
    for f in &findings {
        report.push_str(&format!(
            "  {}:{}  ({} spaces, source line {} cols)\n      ...{}...\n",
            f.file, f.line, f.run, f.source_cols, f.excerpt
        ));
    }
    panic!("{report}");
}

/// The guard above passes when the tree is clean, which is also what it would
/// do if the detector silently stopped detecting. Pin the detector itself
/// against a synthetic rejoin so it cannot rot into a no-op.
#[test]
fn detector_fires_on_a_synthetic_rejoin_and_not_on_its_intact_twin() {
    // 34 spaces mid-sentence on a >100-column line: the shape rustfmt produces.
    let corrupted = format!(
        concat!(
            "fn f() {{ return Err(format!(\"discriminant: enum value has unexpected",
            "{}representation and some more text to clear the column threshold\")) }}\n"
        ),
        " ".repeat(34)
    );
    let hits = scan_source("synthetic.rs", &corrupted);
    assert_eq!(
        hits.len(),
        1,
        "detector missed a synthetic rejoin: {corrupted}"
    );
    assert_eq!(hits[0].run, 34);

    // The same message written as an intact `\`-continuation: rustc drops the
    // newline and the indentation, so nothing is left to find.
    let intact = "fn f() -> &'static str {\n    \"discriminant: enum value has unexpected \\\n     representation and some more text to clear the column threshold\"\n}\n";
    assert!(
        scan_source("synthetic.rs", intact).is_empty(),
        "detector fired on an intact `\\`-continuation"
    );

    // Deliberate column alignment on a short source line stays silent.
    let aligned =
        "fn h() -> &'static str {\n    \"    --output=json           Structured JSON\"\n}\n";
    assert!(
        scan_source("synthetic.rs", aligned).is_empty(),
        "detector fired on deliberate help-text alignment"
    );

    // Contents of comments, raw strings and char literals are never scanned.
    let ignored = format!(
        concat!(
            "// a comment with{}spaces on a very long line padded out past the threshold\n",
            "fn g() {{ let _ = r\"raw{}string on a very long line padded past the threshold\"; }}\n"
        ),
        " ".repeat(20),
        " ".repeat(20)
    );
    assert!(
        scan_source("synthetic.rs", &ignored).is_empty(),
        "detector reached into a comment or raw string"
    );
}
