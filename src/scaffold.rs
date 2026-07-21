//! `karac init` project scaffolding (CR-36).
//!
//! Writes a fixed template into a target directory: `kara.toml`, the entry
//! file (`src/main.kara` or `src/lib.kara`), a starter `_test.kara` alongside
//! it, a title-only `README.md`, and a one-line `.gitignore`. See
//! `docs/design.md § Package System § Project Scaffolding` for the full spec
//! and `brainstorming/brainstorming_v42.md` for the resolution trail.
//!
//! Design invariants enforced here:
//!
//! - **Directory name = package name = root module name.** The caller passes
//!   an already-validated package name; the template interpolates it verbatim.
//! - **Collision rule.** The three *entry* files (`kara.toml`,
//!   `src/main.kara`, `src/lib.kara`) are collision-checked; `--force`
//!   overrides the abort for this trio. The remaining scaffolded files
//!   (`README.md`, `src/*_test.kara`, `.gitignore`) are never overwritten —
//!   if one already exists we leave it alone, even under `--force`. This
//!   matches the `git init` convention where `.gitignore` / `README.md` may
//!   already have been written by the user or another tool, and silently
//!   clobbering them is worse than skipping.
//! - **No VCS side effects.** `.gitignore` is pure text; no `git init`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The template flavor to scaffold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    /// `src/main.kara` + `src/main_test.kara` with a hello-world entry point.
    /// Aliased as the `--cli` flavor in `karac new`'s CLI surface — the
    /// existing scaffold is exactly a CLI tool skeleton.
    Bin,
    /// `src/lib.kara` + `src/lib_test.kara` with a starter `pub fn add`.
    Lib,
    /// Phase-8 line 63 — backend HTTP server skeleton. `src/main.kara`
    /// binds `std.http`'s `Server.serve` on `127.0.0.1:8080` and dispatches
    /// every request manually on `req.path()` (per the v1 routing answer
    /// in phase-8 line 15) with a `/health` endpoint already wired. The
    /// default flavor for `karac new` — see line 63 for the
    /// "default-being-backend" positioning. Companion `_test.kara`
    /// carries a placeholder until a `Request` mock for unit-testing
    /// handlers lands.
    Backend,
}

/// Options driving a scaffold operation.
#[derive(Debug, Clone, Copy)]
pub struct ScaffoldOpts {
    pub template: Template,
    /// When `true`, pre-existing `kara.toml` / `src/main.kara` / `src/lib.kara`
    /// are overwritten instead of aborting the scaffold. Does not affect the
    /// skip-if-exists files (`README.md`, `src/*_test.kara`, `.gitignore` —
    /// never overwritten) or the positional-form non-empty directory check
    /// (which is performed by the caller before scaffolding begins).
    pub force: bool,
}

/// Fatal errors surfaced by `karac init`. Each variant carries enough context
/// for the CLI to render a structured diagnostic; none are recovered from.
#[derive(Debug)]
pub enum ScaffoldError {
    /// Package / directory name is not `[a-z][a-z0-9_]*`. `suggestion` is
    /// populated when a mechanical rewrite (hyphens → underscores, lowercase)
    /// would produce a valid name.
    InvalidName {
        value: String,
        suggestion: Option<String>,
    },
    /// Package name collides with a Kāra reserved keyword.
    ReservedKeyword { value: String },
    /// The positional form `karac init <name>` was given an existing,
    /// non-empty directory. There is no `--force` override for this form by
    /// design — the intent "create this directory" is incompatible with
    /// scaffolding into an existing workspace.
    TargetDirNotEmpty { path: PathBuf },
    /// An entry file (`kara.toml` / `src/main.kara` / `src/lib.kara`) already
    /// exists and `--force` was not passed.
    Collision {
        path: PathBuf,
        file_kind: &'static str,
    },
    /// Generic filesystem error, with the offending path for the diagnostic.
    Io { path: PathBuf, error: String },
}

impl ScaffoldError {
    /// Short machine-readable tag for `--output=json` diagnostics. No new
    /// diagnostic codes were introduced by CR-36 — the CLI maps these tags to
    /// a plain `"scaffold"` phase.
    pub fn tag(&self) -> &'static str {
        match self {
            ScaffoldError::InvalidName { .. } => "invalid_name",
            ScaffoldError::ReservedKeyword { .. } => "reserved_keyword",
            ScaffoldError::TargetDirNotEmpty { .. } => "target_dir_not_empty",
            ScaffoldError::Collision { .. } => "collision",
            ScaffoldError::Io { .. } => "io",
        }
    }
}

impl std::fmt::Display for ScaffoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScaffoldError::InvalidName { value, suggestion } => {
                write!(
                    f,
                    "project name `{value}` is not a valid Kāra package name\n  note: package names must match [a-z][a-z0-9_]* (snake_case, no hyphens)"
                )?;
                if let Some(s) = suggestion {
                    write!(f, "\n  help: did you mean `{s}`?")?;
                }
                Ok(())
            }
            ScaffoldError::ReservedKeyword { value } => write!(
                f,
                "project name `{value}` is a reserved Kāra keyword\n  help: pick a different name",
            ),
            ScaffoldError::TargetDirNotEmpty { path } => write!(
                f,
                "target directory `{}` already exists and is not empty\n  help: to scaffold into an existing directory, `cd` into it and run `karac init` with no positional argument",
                path.display(),
            ),
            ScaffoldError::Collision { path, file_kind } => write!(
                f,
                "`{}` already exists ({})\n  help: pass `--force` to overwrite",
                path.display(),
                file_kind,
            ),
            ScaffoldError::Io { path, error } => write!(
                f,
                "failed to write `{}`: {}",
                path.display(),
                error,
            ),
        }
    }
}

/// Kāra reserved keywords. Source of truth is the lexer's keyword match in
/// `src/lexer.rs#scan_identifier`; we duplicate a subset here because depending
/// on the lexer from scaffold would force the project to parse just to validate
/// a directory name. Drift is mitigated by a unit test that spot-checks a few
/// keywords are actually rejected (and the canonical list is short — syntax
/// changes ship with an update to this list in the same CR).
const RESERVED_KEYWORDS: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "trait",
    "impl",
    "mod",
    "use",
    "const",
    "type",
    "distinct",
    "pub",
    "private",
    "if",
    "else",
    "match",
    "while",
    "for",
    "in",
    "loop",
    "return",
    "break",
    "continue",
    "defer",
    "errdefer",
    "asm",
    "global_asm",
    "let",
    "mut",
    "own",
    "ref",
    "weak",
    "lock",
    "effect",
    "resource",
    "verb",
    "reads",
    "writes",
    "sends",
    "receives",
    "allocates",
    "panics",
    "blocks",
    "suspends",
    "with",
    "transparent",
    "stable",
    "seq",
    "par",
    "yield",
    "as",
    "where",
    "dyn",
    "requires",
    "ensures",
    "invariant",
    "unsafe",
    "extern",
    "shared",
    "layout",
    "group",
    "true",
    "false",
    "providers",
    "alias",
    "independent",
    "self",
    "Self",
];

/// Validate a package name against `[a-z][a-z0-9_]*` and the reserved-keyword
/// list. Returns `Ok(())` on success; on failure the returned
/// `InvalidName.suggestion` is populated when a mechanical rewrite would
/// produce a valid name (hyphens → underscores, lowercase).
pub fn validate_package_name(name: &str) -> Result<(), ScaffoldError> {
    if name.is_empty() {
        return Err(ScaffoldError::InvalidName {
            value: name.to_string(),
            suggestion: None,
        });
    }

    if RESERVED_KEYWORDS.contains(&name) {
        return Err(ScaffoldError::ReservedKeyword {
            value: name.to_string(),
        });
    }

    let first = name.as_bytes()[0];
    let first_ok = first.is_ascii_lowercase();
    let body_ok = name
        .bytes()
        .skip(1)
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');

    if first_ok && body_ok {
        return Ok(());
    }

    let suggestion = build_name_suggestion(name);
    Err(ScaffoldError::InvalidName {
        value: name.to_string(),
        suggestion,
    })
}

/// Derive a snake_case suggestion from an invalid name. Returns `None` if no
/// mechanical fix produces a valid identifier (e.g., name starts with a digit
/// with no letters to promote, or contains non-ASCII characters).
fn build_name_suggestion(name: &str) -> Option<String> {
    let lowered: String = name
        .chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    // If the lowercased/hyphen-replaced form is a pure-ASCII identifier, offer
    // it. Otherwise give up — we'd rather the user pick a new name than see an
    // awkward machine-mangled suggestion.
    if lowered.is_empty() {
        return None;
    }
    let first = lowered.as_bytes()[0];
    if !first.is_ascii_lowercase() {
        return None;
    }
    let body_ok = lowered
        .bytes()
        .skip(1)
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !body_ok {
        return None;
    }
    if RESERVED_KEYWORDS.contains(&lowered.as_str()) {
        return None;
    }
    if lowered == name {
        // Shouldn't happen (caller already rejected `name` as invalid), but
        // guard against suggesting the identical string.
        return None;
    }
    Some(lowered)
}

/// Prepare the target directory for the positional `karac init <name>` form.
/// Creates `./<name>/` if absent; returns `TargetDirNotEmpty` if it already
/// exists with any entries. An empty existing directory is accepted — same
/// rule cargo uses for `cargo init`.
pub fn prepare_new_target_dir(target: &Path) -> Result<(), ScaffoldError> {
    match fs::metadata(target) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(ScaffoldError::TargetDirNotEmpty {
                    path: target.to_path_buf(),
                });
            }
            if !dir_is_empty(target)? {
                return Err(ScaffoldError::TargetDirNotEmpty {
                    path: target.to_path_buf(),
                });
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(target).map_err(|e| ScaffoldError::Io {
                path: target.to_path_buf(),
                error: e.to_string(),
            })
        }
        Err(e) => Err(ScaffoldError::Io {
            path: target.to_path_buf(),
            error: e.to_string(),
        }),
    }
}

fn dir_is_empty(path: &Path) -> Result<bool, ScaffoldError> {
    let mut entries = fs::read_dir(path).map_err(|e| ScaffoldError::Io {
        path: path.to_path_buf(),
        error: e.to_string(),
    })?;
    Ok(entries.next().is_none())
}

/// Scaffold a project at `target_dir` with `package_name` (already validated
/// by `validate_package_name`). Writes five files per the template table in
/// design.md § Project Scaffolding.
pub fn scaffold_project(
    target_dir: &Path,
    package_name: &str,
    opts: ScaffoldOpts,
) -> Result<(), ScaffoldError> {
    let manifest_path = target_dir.join("kara.toml");
    let (entry_rel, entry_body, test_rel, test_body): (&str, String, &str, String) =
        match opts.template {
            Template::Bin => (
                "src/main.kara",
                bin_main_body(),
                "src/main_test.kara",
                bin_main_test_body(),
            ),
            Template::Lib => (
                "src/lib.kara",
                lib_body(),
                "src/lib_test.kara",
                lib_test_body(),
            ),
            Template::Backend => (
                "src/main.kara",
                backend_main_body(),
                "src/main_test.kara",
                backend_main_test_body(),
            ),
        };
    let entry_path = target_dir.join(entry_rel);
    let test_path = target_dir.join(test_rel);
    let readme_path = target_dir.join("README.md");
    let gitignore_path = target_dir.join(".gitignore");

    if !opts.force {
        check_no_collision(&manifest_path, "package manifest")?;
        check_no_collision(&entry_path, "entry file")?;
        // Only check the opposite-template entry file when we have reason to:
        // a stray `src/lib.kara` shouldn't block a `--bin` scaffold. The
        // resolution in brainstorming_v42.md covers `src/main.kara` OR
        // `src/lib.kara` because a package is one or the other, never both —
        // scaffolding a bin into a dir that already holds lib.kara is still a
        // collision the user should know about. Backend mirrors Bin (also
        // owns `src/main.kara`) so the "other" file is `src/lib.kara`.
        let other_entry = match opts.template {
            Template::Bin | Template::Backend => target_dir.join("src/lib.kara"),
            Template::Lib => target_dir.join("src/main.kara"),
        };
        check_no_collision(&other_entry, "entry file")?;
    }

    fs::create_dir_all(target_dir.join("src")).map_err(|e| ScaffoldError::Io {
        path: target_dir.join("src"),
        error: e.to_string(),
    })?;

    write_file(&manifest_path, &manifest_template(package_name))?;
    write_file(&entry_path, &entry_body)?;

    // README, the companion test file, and `.gitignore` are never
    // overwritten — skip if present regardless of --force. Silently
    // clobbering a user's README (or a `.gitignore` that `git init` already
    // wrote) is worse than leaving the stock template off the disk.
    write_file_unless_exists(&test_path, &test_body)?;
    write_file_unless_exists(&readme_path, &readme_template(package_name))?;
    write_file_unless_exists(&gitignore_path, gitignore_template())?;

    Ok(())
}

fn write_file_unless_exists(path: &Path, contents: &str) -> Result<(), ScaffoldError> {
    if path.exists() {
        return Ok(());
    }
    write_file(path, contents)
}

fn check_no_collision(path: &Path, file_kind: &'static str) -> Result<(), ScaffoldError> {
    if path.exists() {
        Err(ScaffoldError::Collision {
            path: path.to_path_buf(),
            file_kind,
        })
    } else {
        Ok(())
    }
}

fn write_file(path: &Path, contents: &str) -> Result<(), ScaffoldError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ScaffoldError::Io {
            path: parent.to_path_buf(),
            error: e.to_string(),
        })?;
    }
    fs::write(path, contents).map_err(|e| ScaffoldError::Io {
        path: path.to_path_buf(),
        error: e.to_string(),
    })
}

// ── Templates ───────────────────────────────────────────────────

fn manifest_template(package_name: &str) -> String {
    format!(
        "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n"
    )
}

fn bin_main_body() -> String {
    "fn main() {\n    println(\"Hello, world!\")\n}\n".to_string()
}

fn bin_main_test_body() -> String {
    "test \"placeholder\" {\n    assert_eq(1 + 1, 2);\n}\n".to_string()
}

fn lib_body() -> String {
    "/// Adds two integers.\npub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n".to_string()
}

fn lib_test_body() -> String {
    "test \"add sums two integers\" {\n    assert_eq(add(2, 3), 5);\n}\n".to_string()
}

/// Phase-8 line 63 backend scaffold — `src/main.kara` skeleton.
///
/// What ships: `std.http` `Server.serve_ws` on `127.0.0.1:8080`,
/// manual dispatch on `req.path()` per the v1 routing answer
/// (phase-8 line 15), `/health` endpoint returning `200 OK / ok`,
/// a `/ws` WebSocket echo route (the handler's `Response { status:
/// 101 }` is the upgrade signal — the runtime completes the RFC
/// 6455 handshake and runs `on_ws` on the upgraded connection),
/// 404 fall-through for unknown paths. Effects declared
/// (`sends(Network) receives(Network)`) so the program
/// effectchecks cleanly.
///
/// The "listening" announcement is emitted through `std.tracing`'s
/// `StdoutExporter` (a structured `LogEvent.info`), so a fresh project
/// shows the tracing idiom from day one rather than a bare `println`.
///
/// Still deferred: per-handler `std.tracing` spans — the
/// ambient/registered exporter that would let a handler emit without
/// threading an exporter value is a separate deferred item; the scaffold
/// emits one startup event through an explicit `StdoutExporter`, the v1
/// caller-plumbed idiom.
fn backend_main_body() -> String {
    "// Backend HTTP server skeleton, generated by `karac new`.\n\
     //\n\
     // Bind on 127.0.0.1:8080 and serve:\n\
     //   - `/health` -> `200 OK / ok`\n\
     //   - `/ws`     -> WebSocket echo (returning status 101 from `handle`\n\
     //                  accepts the upgrade; the runtime completes the\n\
     //                  RFC 6455 handshake and runs `on_ws` on the socket)\n\
     //   - anything else -> 404\n\
     // Replace `handle` / `on_ws` with your own logic as the project grows.\n\
     //\n\
     // To run: `karac build` then execute the produced binary. The server\n\
     // listens until the process is killed.\n\
     \n\
     fn handle(req: Request) -> Response {\n    \
     match req.path() {\n        \
     \"/health\" => Response { status: 200, body: \"ok\" },\n        \
     \"/ws\" => Response { status: 101, body: \"\" },\n        \
     _ => Response { status: 404, body: \"not found\" },\n    \
     }\n\
     }\n\
     \n\
     fn on_ws(ws: WebSocket) {\n    \
     let mut buf: Array[u8, 4096] = [0u8; 4096];\n    \
     loop {\n        \
     match ws.recv_text(mut buf) {\n            \
     Result.Ok(n) => {\n                \
     if n == 0 { break; }\n                \
     match ws.send_text(buf[0..n]) {\n                    \
     Result.Ok(_) => {}\n                    \
     Result.Err(_) => { break; }\n                \
     }\n            \
     }\n            \
     Result.Err(_) => { break; }\n        \
     }\n    \
     }\n\
     }\n\
     \n\
     fn main() with sends(Network) receives(Network) {\n    \
     let addr: String = \"127.0.0.1:8080\";\n    \
     let tracer = StdoutExporter {};\n    \
     tracer.export_event(LogEvent.info(\"Listening on http://127.0.0.1:8080\"));\n    \
     let _result = Server.serve_ws(addr, handle, on_ws);\n\
     }\n"
    .to_string()
}

/// Phase-8 line 63 backend scaffold — `src/main_test.kara`
/// placeholder. The generated test is a no-op `assert_eq(1 + 1, 2)`
/// because the interpreter currently has no way to construct a
/// synthetic `Request` value for unit-testing handler bodies (the
/// runtime's `KaracHttpRequest` is hyper-owned and only exists
/// during real request dispatch). Joins a proper handler-test
/// surface once `Request.new_for_testing` or equivalent lands.
fn backend_main_test_body() -> String {
    "test \"placeholder — handler tests await a Request mock\" {\n    \
     assert_eq(1 + 1, 2);\n\
     }\n"
    .to_string()
}

fn readme_template(package_name: &str) -> String {
    format!("# {package_name}\n")
}

fn gitignore_template() -> &'static str {
    "/dist/\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_snake_case() {
        assert!(validate_package_name("hello").is_ok());
        assert!(validate_package_name("hello_world").is_ok());
        assert!(validate_package_name("my_app_2").is_ok());
        assert!(validate_package_name("a").is_ok());
    }

    #[test]
    fn validate_rejects_empty() {
        let err = validate_package_name("").unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::InvalidName {
                suggestion: None,
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_hyphen_with_suggestion() {
        let err = validate_package_name("my-app").unwrap_err();
        match err {
            ScaffoldError::InvalidName { value, suggestion } => {
                assert_eq!(value, "my-app");
                assert_eq!(suggestion, Some("my_app".to_string()));
            }
            other => panic!("expected InvalidName, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_uppercase_with_suggestion() {
        let err = validate_package_name("MyApp").unwrap_err();
        match err {
            ScaffoldError::InvalidName {
                suggestion: Some(s),
                ..
            } => {
                assert_eq!(s, "myapp");
            }
            other => panic!("expected InvalidName with suggestion, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_leading_digit_no_suggestion() {
        let err = validate_package_name("0foo").unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::InvalidName {
                suggestion: None,
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_reserved_keyword() {
        for kw in &["fn", "let", "mut", "pub", "mod", "if"] {
            let err = validate_package_name(kw).unwrap_err();
            assert!(
                matches!(err, ScaffoldError::ReservedKeyword { .. }),
                "expected ReservedKeyword for `{kw}`, got {err:?}",
            );
        }
    }

    #[test]
    fn validate_rejects_non_ascii() {
        let err = validate_package_name("café").unwrap_err();
        assert!(matches!(err, ScaffoldError::InvalidName { .. }));
    }

    #[test]
    fn manifest_template_contains_expected_keys() {
        let t = manifest_template("hello");
        assert!(t.contains("name = \"hello\""));
        assert!(t.contains("version = \"0.1.0\""));
        assert!(t.contains("authors = []"));
        assert!(t.contains("edition = \"2026\""));
        assert!(t.contains("[dependencies]"));
    }

    // ── Phase-8 line 63 backend scaffold ────────────────────────────

    /// Backend `src/main.kara` shape — the four anchors the runtime
    /// surface guarantees are stable today. If any of these break,
    /// the scaffolded project would fail to compile.
    #[test]
    fn backend_main_body_carries_required_anchors() {
        let body = backend_main_body();
        // std.http server entry point — the WS-upgrade variant since the
        // phase-8 line-170 hook landed (2026-07-21).
        assert!(
            body.contains("Server.serve_ws("),
            "backend main must call Server.serve_ws; body was:\n{body}"
        );
        // The /ws echo route: the 101 upgrade-signal arm + the ws handler.
        assert!(
            body.contains("\"/ws\"") && body.contains("status: 101"),
            "backend main must wire the /ws 101 arm; body was:\n{body}"
        );
        assert!(
            body.contains("fn on_ws(ws: WebSocket)"),
            "backend main must define the on_ws handler; body was:\n{body}"
        );
        // Manual dispatch on req.path() per v1 routing answer (line 15).
        assert!(
            body.contains("req.path()"),
            "backend main must dispatch on req.path(); body was:\n{body}"
        );
        // /health endpoint per the doc spec.
        assert!(
            body.contains("\"/health\""),
            "backend main must wire a /health route; body was:\n{body}"
        );
        // Effect declarations — Server.serve carries sends/receives(Network),
        // so main must declare them or effectcheck will reject the call.
        assert!(
            body.contains("sends(Network)") && body.contains("receives(Network)"),
            "backend main must declare sends(Network) receives(Network); body was:\n{body}"
        );
        // std.tracing emission idiom — the startup announcement goes
        // through StdoutExporter.export_event(LogEvent.info(...)), not a
        // bare println, so new projects see the tracing surface day one.
        assert!(
            body.contains("StdoutExporter {}") && body.contains("LogEvent.info("),
            "backend main must emit the listening announcement via std.tracing; body was:\n{body}"
        );
    }

    /// Backend `src/main_test.kara` is a placeholder until a `Request`
    /// mock for unit-testing handlers lands. Pin the placeholder shape
    /// so the surface stays "valid test syntax that always passes" —
    /// `karac test` on a fresh project must succeed out of the box.
    #[test]
    fn backend_test_body_is_a_passing_placeholder() {
        let body = backend_main_test_body();
        assert!(
            body.starts_with("test \""),
            "expected `test \"...\" {{ ... }}` shape"
        );
        assert!(body.contains("assert_eq(1 + 1, 2)"));
    }

    /// `scaffold_project(target, name, Backend)` writes the expected
    /// files to disk with the expected bodies. End-to-end check on
    /// the routing logic in `scaffold_project` (Backend → main.kara
    /// rather than lib.kara, opposite-entry collision check against
    /// `src/lib.kara`).
    #[test]
    fn scaffold_backend_writes_main_kara_and_manifest() {
        let dir = tempdir();
        let opts = ScaffoldOpts {
            template: Template::Backend,
            force: false,
        };
        scaffold_project(dir.path(), "my_api", opts).expect("scaffold");
        let manifest = fs::read_to_string(dir.path().join("kara.toml")).expect("manifest");
        assert!(manifest.contains("name = \"my_api\""));
        let main = fs::read_to_string(dir.path().join("src/main.kara")).expect("main.kara");
        assert!(main.contains("Server.serve_ws("));
        assert!(main.contains("/health"));
        let test = fs::read_to_string(dir.path().join("src/main_test.kara")).expect("test");
        assert!(test.starts_with("test \""));
        let readme = fs::read_to_string(dir.path().join("README.md")).expect("README");
        assert!(readme.contains("# my_api"));
        assert!(
            !dir.path().join("src/lib.kara").exists(),
            "lib.kara must not be written"
        );
    }

    /// Backend scaffold collides with a pre-existing `src/lib.kara`
    /// (same precedent the Bin template has — a package is one entry
    /// shape or the other, never both).
    #[test]
    fn scaffold_backend_rejects_when_lib_kara_already_exists() {
        let dir = tempdir();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.kara"), "// pre-existing\n").unwrap();
        let err = scaffold_project(
            dir.path(),
            "my_api",
            ScaffoldOpts {
                template: Template::Backend,
                force: false,
            },
        )
        .expect_err("should collide with lib.kara");
        assert!(matches!(err, ScaffoldError::Collision { .. }));
    }

    fn tempdir() -> TempDirHandle {
        use std::sync::atomic::{AtomicU64, Ordering};
        // Process-wide monotonic counter so two calls landing in the same
        // clock tick under parallel test execution never collide on a path.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("karac_scaffold_{pid}_{nanos}_{seq}"));
        fs::create_dir_all(&path).expect("tempdir create");
        TempDirHandle { path }
    }

    struct TempDirHandle {
        path: PathBuf,
    }

    impl TempDirHandle {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDirHandle {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
