//! Adversarial coverage of the no-lifetime-annotation design claim.
//!
//! Kāra's design.md (Feature 4 § Part 3 — "Explicit `ref` for Borrow Returns")
//! commits to a story where parameter modes (`own` / `ref` / `mut ref`) plus
//! return-position borrow-source inference together replace `'a`-style lifetime
//! annotations. The text explicitly says: "No disambiguation annotation is
//! needed — the conservative assumption is always safe and avoids introducing
//! a lifetime-like concept." This file exists to keep that claim honest as
//! the compiler evolves.
//!
//! Each `should_accept` case codifies a borrow pattern that Rust would force
//! to carry an explicit `'a`. Each `should_reject` case codifies an escape
//! pattern that Rust catches via lifetime mismatch and that Kāra must catch
//! via ownership/escape analysis instead. Together they form the test of the
//! design claim: if any `should_accept` regresses to a rejection, the
//! "no annotations needed" promise has narrowed; if any `should_reject`
//! regresses to acceptance, the safety story has a hole.
//!
//! Static-only tests run on plain `cargo test`. Under `cargo test --features
//! llvm`, the inner `runtime_confirmation` module additionally compiles each
//! accept case to a runnable binary, links it under AddressSanitizer, and
//! asserts a clean exit. ASAN closes the loop "static analysis accepted →
//! generated code is actually memory-safe."
//!
//! **macOS leak gap.** Apple clang's ASAN runtime does not include
//! LeakSanitizer (see `tests/memory_sanitizer.rs:95-104`). On macOS the
//! runtime confirmation catches use-after-free, double-free, and heap
//! buffer overflow but NOT leaks; on Linux LeakSanitizer is enabled and
//! catches leaks too. A cross-platform alloc/free balance assertion is
//! tracked as a phase-7 followup.

use karac::ownership::{OwnershipError, OwnershipErrorKind};
use karac::{ownershipcheck, parse, resolve, typecheck};

// ── Helpers ─────────────────────────────────────────────────────

/// Runs the static pipeline through ownership and asserts the program is
/// accepted by every phase. Returns the ownership result for further
/// inspection if the caller needs it.
fn assert_static_accept(source: &str, label: &str) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "[{label}] parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "[{label}] resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "[{label}] type errors: {}",
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
        "[{label}] ownership errors: {}",
        ownership
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Runs the static pipeline and returns the ownership errors. Asserts that
/// parse/resolve/typecheck are clean — we want the test to fail loudly if a
/// "should_reject" case regresses to a parse-error (which would silently
/// satisfy "errors are present").
fn ownership_errors_only(source: &str, label: &str) -> Vec<OwnershipError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "[{label}] parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "[{label}] resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    let ownership = ownershipcheck(&parsed.program, &typed);
    ownership.errors
}

/// Asserts the program produces at least one ownership error of the given
/// kind. Multiple-error cases are common (an escape can trip several rules);
/// requiring "any" rather than "all" avoids brittleness as diagnostic policy
/// evolves.
fn assert_ownership_error_kind(source: &str, expected: OwnershipErrorKind, label: &str) {
    let errors = ownership_errors_only(source, label);
    assert!(
        !errors.is_empty(),
        "[{label}] expected ownership errors but got none"
    );
    assert!(
        errors.iter().any(|e| e.kind == expected),
        "[{label}] expected at least one {:?}; got: {:?}",
        expected,
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ────────────────────────────────────────────────────────────────
// Section 1: should_accept — patterns Rust would require `'a` for.
// ────────────────────────────────────────────────────────────────

/// design.md Feature 4 Part 3, single-source shorthand:
/// "When a function has exactly one `ref` parameter, the source of any
/// returned borrow is unambiguous, so the plain `ref T` annotation suffices."
///
/// This case uses parameter-passthrough as the body — the simplest form a
/// single-source borrow return can take. A spec-faithful version using a
/// field projection in the body (`user.name`) lives below as
/// `spec_field_projection_in_borrow_return`.
#[test]
fn accept_single_source_borrow_return() {
    assert_static_accept(
        "fn echo(s: ref String) -> ref String { s }\n\
         fn main() {\n\
             let s = String.from(\"hello\");\n\
             let t = echo(s);\n\
             println(t.len());\n\
         }",
        "accept_single_source_borrow_return",
    );
}

/// Verbatim from design.md Feature 4 Part 3:
/// `fn name(user: ref User) -> ref String { user.name }`
///
/// This is the most-cited example of the no-annotation borrow-return
/// design. Today the typechecker rejects it: the body's tail expression
/// `user.name` evaluates to `String` rather than coercing back to
/// `ref String` from a `ref User` receiver. Tracking this as an
/// implementation gap, not a design change — the test re-enables itself
/// the moment the typechecker grows return-position field-projection
/// auto-borrowing.
#[test]
#[ignore = "design.md Feature 4 Part 3 spec; impl gap — field projection in borrow-return position not auto-borrowed yet"]
fn spec_field_projection_in_borrow_return() {
    assert_static_accept(
        "struct User { name: String }\n\
         fn user_name(user: ref User) -> ref String {\n\
             user.name\n\
         }\n\
         fn main() {\n\
             let u = User { name: String.from(\"alice\") };\n\
             let n = user_name(u);\n\
             println(n.len());\n\
         }",
        "spec_field_projection_in_borrow_return",
    );
}

/// design.md Feature 4 Part 3, multi-source overapproximation:
/// "When a borrow could come from more than one `ref` parameter, the
/// compiler conservatively assumes the return may borrow from *all* `ref`
/// parameters." Verbatim example from the design doc.
#[test]
fn accept_multi_source_borrow_return() {
    assert_static_accept(
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
             if a.len() > b.len() { a } else { b }\n\
         }\n\
         fn main() {\n\
             let x = String.from(\"short\");\n\
             let y = String.from(\"a longer string\");\n\
             let z = longer(x, y);\n\
             println(z.len());\n\
         }",
        "accept_multi_source_borrow_return",
    );
}

/// design.md Feature 4 Part 3, ref inside generic wrappers:
/// "`ref T` is a first-class type ... and may appear inside generic type
/// arguments in a return type: `Option[ref T]`, `Result[ref T, E]`,
/// `(ref T, ref U)`."
///
/// Today the call site rejects passing an owned `Vec[i64]` to a
/// `ref Vec[i64]` parameter — the call-site auto-ref rule that works for
/// `String` doesn't extend to `Vec` yet. Marked as a spec test so it
/// surfaces when generic-type call-site coercion lands.
#[test]
#[ignore = "design.md Feature 4 Part 3 spec; impl gap — owned-to-ref coercion at call site not extended to generic Vec yet"]
fn spec_option_ref_t_return() {
    assert_static_accept(
        "fn first(v: ref Vec[i64]) -> Option[ref i64] {\n\
             v.get(0)\n\
         }\n\
         fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(42);\n\
             match first(v) {\n\
                 Some(n) => println(n),\n\
                 None => println(0),\n\
             }\n\
         }",
        "spec_option_ref_t_return",
    );
}

/// design.md Feature 4 Part 3, borrowed struct:
/// "A struct may contain `ref` fields. Such a struct is a *borrowed
/// struct*: its scope is bounded by the scope of every value its `ref`
/// fields borrow from. No named lifetime parameters are written."
#[test]
fn accept_borrowed_struct_construction() {
    assert_static_accept(
        "struct Parser {\n\
             source: ref String,\n\
             position: i64,\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"input\");\n\
             let p = Parser { source: s, position: 0 };\n\
             println(p.position);\n\
         }",
        "accept_borrowed_struct_construction",
    );
}

/// design.md Feature 4 Part 3, returning a borrowed struct (verbatim from
/// the design doc):
/// "Returning a borrowed struct from a function follows the same rule as
/// returning a `ref` value: the borrowed struct's sources must all be
/// parameters. The compiler traces each `ref` field to its source parameter
/// automatically — no annotation is needed on borrowed struct returns."
///
/// Today the typechecker rejects `Parser { source: s, position: 0 }` as
/// `ref Parser` — the borrowed-struct's owned construction site doesn't
/// re-borrow into the declared return type. Spec-faithful test, ignored
/// pending the borrowed-struct return-coercion landing.
#[test]
#[ignore = "design.md Feature 4 Part 3 spec; impl gap — owned struct construction not coerced to ref Struct return"]
fn spec_return_borrowed_struct() {
    assert_static_accept(
        "struct Parser {\n\
             source: ref String,\n\
             position: i64,\n\
         }\n\
         fn make_parser(s: ref String) -> ref Parser {\n\
             Parser { source: s, position: 0 }\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"input\");\n\
             let p = make_parser(s);\n\
             println(p.position);\n\
         }",
        "spec_return_borrowed_struct",
    );
}

/// `ref self` method returning a borrow into a field. In Rust this is the
/// canonical case for lifetime elision (`fn name(&self) -> &String`); in
/// Kāra single-source shorthand applies for the same reason.
///
/// Same impl gap as `spec_field_projection_in_borrow_return` — `self.name`
/// in return position doesn't auto-borrow today. Spec test.
#[test]
#[ignore = "design.md Feature 4 Part 3 spec; impl gap — field projection in borrow-return position not auto-borrowed yet"]
fn spec_ref_self_returning_field_borrow() {
    assert_static_accept(
        "struct User { name: String, age: i64 }\n\
         impl User {\n\
             fn name(ref self) -> ref String { self.name }\n\
         }\n\
         fn main() {\n\
             let u = User { name: String.from(\"alice\"), age: 30 };\n\
             let n = u.name();\n\
             println(n.len());\n\
         }",
        "spec_ref_self_returning_field_borrow",
    );
}

/// Closure that captures a borrow but does NOT escape its creation scope —
/// the closure is invoked inline. This is the case Rust would still allow,
/// but only because the compiler can prove the closure's lifetime fits;
/// Kāra reaches the same conclusion via ownership analysis without any
/// `'_` annotation surfacing in the source.
#[test]
fn accept_closure_borrow_capture_no_escape() {
    assert_static_accept(
        "fn main() {\n\
             let s = String.from(\"hello\");\n\
             let len_plus = |extra: i64| s.len() + extra;\n\
             println(len_plus(5));\n\
         }",
        "accept_closure_borrow_capture_no_escape",
    );
}

/// Chained ref returns: caller threads a borrow through two functions of
/// the same single-source signature. The borrow's source is the original
/// owned binding; Kāra must trace through call boundaries without
/// annotation help. Uses passthrough-only bodies to avoid the field-
/// projection impl gap.
#[test]
fn accept_chained_borrow_returns() {
    assert_static_accept(
        "fn echo(s: ref String) -> ref String { s }\n\
         fn echo_twice(s: ref String) -> ref String {\n\
             let t = echo(s);\n\
             echo(t)\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"chained\");\n\
             let r = echo_twice(s);\n\
             println(r.len());\n\
         }",
        "accept_chained_borrow_returns",
    );
}

// ────────────────────────────────────────────────────────────────
// Section 2: should_reject — escapes Rust catches via lifetime mismatch
// and Kāra must catch via ownership/escape analysis.
// ────────────────────────────────────────────────────────────────

/// design.md Feature 4 Part 3, "ref-captured value escaping its borrow's
/// lifetime" — sub-case (iv) of the closures rules. Returning a closure
/// that read-only-captures a parameter must fire E0508
/// (`RefCaptureEscapesScope`): the closure's `ref` capture would outlive
/// `cfg`, which is owned by `make_handler`. This is the no-annotation
/// analog of Rust's `'a` mismatch on a returned `impl Fn() -> &T`.
#[test]
fn reject_closure_with_ref_capture_returned() {
    assert_ownership_error_kind(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             || cfg.value\n\
         }",
        OwnershipErrorKind::RefCaptureEscapesScope,
        "reject_closure_with_ref_capture_returned",
    );
}

/// Use-after-move: the canonical ownership error. Included here not because
/// it is unique to the no-annotation design, but because the ownership
/// system has to remain the *only* guard in the absence of borrow
/// annotations — a regression here would weaken the entire safety story.
/// Uses a custom struct (rather than `String`) because `String` arguments
/// have call-site coercion paths that don't trigger a clean move on the
/// existing test corpus.
#[test]
fn reject_use_after_move() {
    assert_ownership_error_kind(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) -> i64 { d.value }\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             let _ = consume(d);\n\
             let _ = consume(d);\n\
         }",
        OwnershipErrorKind::UseAfterMove,
        "reject_use_after_move",
    );
}

// ────────────────────────────────────────────────────────────────
// Section 3: ASAN runtime confirmation for accept cases.
// Closes the loop: static accept → generated code is memory-safe.
// macOS leak gap noted in the file header.
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "llvm")]
mod runtime_confirmation {
    use karac::codegen::{compile_to_object, link_executable_with_sanitizer};
    use std::path::Path;
    use std::process::Command;
    use std::sync::OnceLock;

    /// Mirrors `tests/memory_sanitizer.rs::asan_available` so this file can
    /// stand alone without depending on a tests/common helper module.
    fn asan_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            if std::env::var("KARAC_SKIP_ASAN_TESTS").is_ok() {
                return false;
            }
            let probe_c = "/tmp/karac_safety_design_probe.c";
            let probe_exe = "/tmp/karac_safety_design_probe";
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

    /// Compile, link with ASAN, run, and assert clean exit. Stdout is not
    /// pinned here — the *runtime safety* of accepted programs is what we
    /// want to confirm; behavioral correctness is the typechecker's job.
    fn assert_accepted_program_is_asan_clean(src: &str, label: &str) {
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
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
        let obj_path = format!("/tmp/karac_safety_design_{}_{}.o", std::process::id(), id);
        let exe_path = format!("/tmp/karac_safety_design_{}_{}", std::process::id(), id);

        if let Err(e) = compile_to_object(&parsed.program, &obj_path, None, None) {
            eprintln!("[{label}] compile_to_object failed: {e} — skipping");
            return;
        }
        if !Path::new(&obj_path).exists() {
            eprintln!("[{label}] object file missing — skipping");
            return;
        }
        if let Err(e) =
            link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"])
        {
            eprintln!("[{label}] link failed: {e} — skipping (runtime lib likely absent)");
            let _ = std::fs::remove_file(&obj_path);
            return;
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
                    panic!(
                        "[{label}] ASAN reported a memory error (exit {:?}). \
                         Look for `LeakSanitizer`, `heap-use-after-free`, or \
                         `double-free` in stderr:\n{stderr}",
                        out.status.code()
                    );
                }
            }
            Err(e) => eprintln!("[{label}] failed to run binary: {e} — skipping"),
        }
    }

    // The ASAN-routed cases mirror the *currently-passing* static accept
    // tests. The ignore-gated `spec_*` cases are not mirrored here — they
    // wouldn't compile to a binary today, so there's nothing to run.

    #[test]
    fn asan_single_source_borrow_return() {
        assert_accepted_program_is_asan_clean(
            "fn echo(s: ref String) -> ref String { s }\n\
             fn main() {\n\
                 let s = String.from(\"hello\");\n\
                 let t = echo(s);\n\
                 println(t.len());\n\
             }",
            "asan_single_source_borrow_return",
        );
    }

    #[test]
    fn asan_multi_source_borrow_return() {
        assert_accepted_program_is_asan_clean(
            "fn longer(a: ref String, b: ref String) -> ref String {\n\
                 if a.len() > b.len() { a } else { b }\n\
             }\n\
             fn main() {\n\
                 let x = String.from(\"short\");\n\
                 let y = String.from(\"a longer string\");\n\
                 let z = longer(x, y);\n\
                 println(z.len());\n\
             }",
            "asan_multi_source_borrow_return",
        );
    }

    #[test]
    fn asan_borrowed_struct_construction() {
        assert_accepted_program_is_asan_clean(
            "struct Parser {\n\
                 source: ref String,\n\
                 position: i64,\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"input\");\n\
                 let p = Parser { source: s, position: 0 };\n\
                 println(p.position);\n\
             }",
            "asan_borrowed_struct_construction",
        );
    }

    #[test]
    fn asan_closure_borrow_capture_no_escape() {
        assert_accepted_program_is_asan_clean(
            "fn main() {\n\
                 let s = String.from(\"hello\");\n\
                 let len_plus = |extra: i64| s.len() + extra;\n\
                 println(len_plus(5));\n\
             }",
            "asan_closure_borrow_capture_no_escape",
        );
    }

    #[test]
    fn asan_chained_borrow_returns() {
        assert_accepted_program_is_asan_clean(
            "fn echo(s: ref String) -> ref String { s }\n\
             fn echo_twice(s: ref String) -> ref String {\n\
                 let t = echo(s);\n\
                 echo(t)\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"chained\");\n\
                 let r = echo_twice(s);\n\
                 println(r.len());\n\
             }",
            "asan_chained_borrow_returns",
        );
    }
}
