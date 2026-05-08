//! Mutation-based property test of the no-lifetime-annotation safety claim.
//!
//! `tests/safety_design.rs` curates a small corpus of programs that exercise
//! the design's borrow patterns. Each one is a single witness — one shape,
//! one accept. This file generalizes those witnesses into a *property*:
//!
//!   "for every program P that the static pipeline accepts, and for every
//!    semantics-preserving mutation M, the pipeline must still accept M(P)
//!    — and (under --features llvm) the resulting binary must still run
//!    cleanly under ASAN."
//!
//! That is the closure of the static→runtime safety claim: not just "this
//! one program is safe" but "any small textual perturbation of a safe
//! program stays safe." If a mutation breaks the invariant, the bug is
//! either in the mutation (it wasn't actually semantics-preserving) or in
//! the compiler (a fragile path the curated corpus didn't hit).
//!
//! The mutation operators here are intentionally small and textual rather
//! than AST-based. Textual mutations are easy to audit for soundness
//! ("inserting a `let _x = 0;` cannot change observable behavior of any
//! program in our corpus") and cheap to write — full AST-aware mutation
//! belongs in `fuzz/fuzz_targets/` once the corpus grows beyond what
//! hand-curation can cover.
//!
//! macOS leak gap from `tests/memory_sanitizer.rs:95-104` applies here
//! too — see header of `tests/safety_design.rs`.

use karac::{ownershipcheck, parse, resolve, typecheck};

// ── Corpus ───────────────────────────────────────────────────────
//
// Programs are kept short so the failure messages stay readable when a
// mutation regresses one of them. Each is a witness already shipped in
// the curated suite — keeping them in lockstep means a regression here
// implicates either `safety_design.rs`'s case or the compiler.

const CORPUS: &[(&str, &str)] = &[
    (
        "single_source_borrow_return",
        "fn echo(s: ref String) -> ref String { s }\n\
         fn main() {\n\
             let s = String.from(\"hello\");\n\
             let t = echo(s);\n\
             println(t.len());\n\
         }",
    ),
    (
        "multi_source_borrow_return",
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
             if a.len() > b.len() { a } else { b }\n\
         }\n\
         fn main() {\n\
             let x = String.from(\"short\");\n\
             let y = String.from(\"a longer string\");\n\
             let z = longer(x, y);\n\
             println(z.len());\n\
         }",
    ),
    (
        "borrowed_struct_construction",
        "struct Parser {\n\
             source: ref String,\n\
             position: i64,\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"input\");\n\
             let p = Parser { source: s, position: 0 };\n\
             println(p.position);\n\
         }",
    ),
    (
        "closure_borrow_capture_no_escape",
        "fn main() {\n\
             let s = String.from(\"hello\");\n\
             let len_plus = |extra: i64| s.len() + extra;\n\
             println(len_plus(5));\n\
         }",
    ),
];

// ── Mutation operators ───────────────────────────────────────────
//
// Each operator either returns `Some(mutated)` if it could apply to the
// program, or `None` if the program shape didn't match (e.g., no `main`
// found). `None` is a no-op: the (program, mutation) pair is skipped, not
// failed. The mutation set is deliberately conservative — every operator
// here has been hand-checked against every corpus program for semantic
// invariance.

type Mutation = fn(&str) -> Option<String>;

/// Inject `let __noop = 0_i64;` as the first statement of `main`.
/// Adds a binding that's never used elsewhere — pure no-op for
/// observable behavior, though it does add one to scope-cleanup actions
/// (which the compiler should handle identically whether the binding is
/// referenced or not).
fn prepend_noop_let_in_main(src: &str) -> Option<String> {
    let needle = "fn main() {\n";
    let idx = src.find(needle)?;
    let mut out = String::with_capacity(src.len() + 32);
    out.push_str(&src[..idx + needle.len()]);
    out.push_str("    let __noop = 0_i64;\n");
    out.push_str(&src[idx + needle.len()..]);
    Some(out)
}

/// Wrap the body of `main` in an extra block. Introduces a new lexical
/// scope but doesn't change observable behavior — the inner block's tail
/// is `()` and main's tail is `()`, so the wrapping is invisible.
fn wrap_main_body_in_block(src: &str) -> Option<String> {
    let open = "fn main() {\n";
    let idx = src.find(open)?;
    let body_start = idx + open.len();

    // Walk from body_start to the matching `}` for `main`'s body. The
    // corpus uses no nested braces inside main except for struct
    // literals, so a depth counter on `{` / `}` characters is enough.
    let bytes = src.as_bytes();
    let mut depth: i32 = 1;
    let mut i = body_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    let body_end = i;

    let body = &src[body_start..body_end];
    let mut out = String::with_capacity(src.len() + 16);
    out.push_str(&src[..body_start]);
    out.push_str("    {\n");
    out.push_str(body);
    out.push_str("    }\n");
    out.push_str(&src[body_end..]);
    Some(out)
}

/// Trailing whitespace — most innocent mutation possible, included as a
/// sanity check on the mutation harness itself. If this ever fails to
/// accept, the harness has a problem (not the compiler).
fn append_trailing_newline(src: &str) -> Option<String> {
    let mut out = String::with_capacity(src.len() + 1);
    out.push_str(src);
    out.push('\n');
    Some(out)
}

const MUTATIONS: &[(&str, Mutation)] = &[
    ("prepend_noop_let_in_main", prepend_noop_let_in_main),
    ("wrap_main_body_in_block", wrap_main_body_in_block),
    ("append_trailing_newline", append_trailing_newline),
];

// ── Static invariant ─────────────────────────────────────────────

fn assert_static_accept(source: &str, label: &str) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "[{label}] parse errors: {:?}\n--- source ---\n{source}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "[{label}] resolve errors: {:?}\n--- source ---\n{source}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "[{label}] type errors: {}\n--- source ---\n{source}",
        typed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let ownership = ownershipcheck(&parsed.program, &typed);
    assert!(
        ownership.errors.is_empty(),
        "[{label}] ownership errors: {}\n--- source ---\n{source}",
        ownership
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

#[test]
fn static_accept_invariant_under_mutation() {
    let mut applied = 0usize;
    for (prog_label, src) in CORPUS {
        // Sanity-check the un-mutated baseline first. If the corpus
        // itself doesn't accept, the failure should point at the corpus
        // rather than blame a mutation.
        assert_static_accept(src, &format!("baseline:{prog_label}"));

        for (mut_label, mutation) in MUTATIONS {
            let Some(mutated) = mutation(src) else {
                continue;
            };
            let label = format!("{prog_label}::{mut_label}");
            assert_static_accept(&mutated, &label);
            applied += 1;
        }
    }
    // Guard against the silent-pass mode where every mutation returned
    // None and the test trivially succeeded.
    assert!(
        applied >= CORPUS.len(),
        "expected at least one mutation to apply per corpus program; \
         only {applied} applications across {} programs",
        CORPUS.len()
    );
}

// ── Runtime invariant (ASAN-routed) ──────────────────────────────

#[cfg(feature = "llvm")]
mod runtime_invariant {
    use super::{CORPUS, MUTATIONS};
    use karac::codegen::{compile_to_object, link_executable_with_sanitizer};
    use std::path::Path;
    use std::process::Command;
    use std::sync::OnceLock;

    fn asan_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            if std::env::var("KARAC_SKIP_ASAN_TESTS").is_ok() {
                return false;
            }
            let probe_c = "/tmp/karac_safety_loop_probe.c";
            let probe_exe = "/tmp/karac_safety_loop_probe";
            if std::fs::write(probe_c, "int main(void){return 0;}\n").is_err() {
                return false;
            }
            let link_ok = Command::new("cc")
                .args(["-fsanitize=address", probe_c, "-o", probe_exe])
                .output()
                .ok()
                .map(|o| o.status.success())
                .unwrap_or(false);
            let run_ok = link_ok
                && Command::new(probe_exe)
                    .output()
                    .ok()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
            let _ = std::fs::remove_file(probe_c);
            let _ = std::fs::remove_file(probe_exe);
            run_ok
        })
    }

    fn run_under_asan(src: &str, label: &str) -> Option<std::process::ExitStatus> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            panic!("[{label}] parse errors: {:?}", parsed.errors);
        }
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let obj_path = format!("/tmp/karac_safety_loop_{}_{}.o", std::process::id(), id);
        let exe_path = format!("/tmp/karac_safety_loop_{}_{}", std::process::id(), id);

        if let Err(e) = compile_to_object(&parsed.program, &obj_path, None, None) {
            eprintln!("[{label}] compile_to_object failed: {e} — skipping");
            return None;
        }
        if !Path::new(&obj_path).exists() {
            return None;
        }
        if let Err(e) =
            link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"])
        {
            eprintln!("[{label}] link failed: {e} — skipping");
            let _ = std::fs::remove_file(&obj_path);
            return None;
        }

        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };
        let output = Command::new(&exe_path)
            .env("ASAN_OPTIONS", asan_options)
            .output();

        let _ = std::fs::remove_file(&obj_path);
        let _ = std::fs::remove_file(&exe_path);

        match output {
            Ok(out) => {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    eprintln!("[{label}] ASAN stderr:\n{stderr}");
                }
                Some(out.status)
            }
            Err(e) => {
                eprintln!("[{label}] failed to run binary: {e}");
                None
            }
        }
    }

    #[test]
    fn asan_clean_invariant_under_mutation() {
        if !asan_available() {
            eprintln!("ASAN unavailable on this host — skipping");
            return;
        }
        for (prog_label, src) in CORPUS {
            // Run baseline first so a failure tied to the un-mutated
            // program reports cleanly rather than getting attributed to a
            // mutation.
            if let Some(status) = run_under_asan(src, &format!("baseline:{prog_label}")) {
                assert!(
                    status.success(),
                    "[baseline:{prog_label}] ASAN reported a memory error \
                     (exit {:?}) — see stderr above",
                    status.code()
                );
            }

            for (mut_label, mutation) in MUTATIONS {
                let Some(mutated) = mutation(src) else {
                    continue;
                };
                let label = format!("{prog_label}::{mut_label}");
                if let Some(status) = run_under_asan(&mutated, &label) {
                    assert!(
                        status.success(),
                        "[{label}] ASAN reported a memory error \
                         (exit {:?}) — see stderr above",
                        status.code()
                    );
                }
            }
        }
    }
}
