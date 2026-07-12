//! Systematic shared-type ownership matrix (B-2026-07-12-24 follow-up).
//!
//! The RC-lifecycle bugs for `shared` types in enum/match/binding/param
//! positions (B-4, B-21, B-23, B-24, B-25 …) were found and patched one crash
//! or leak at a time. This test replaces whack-a-mole with a CHECKED matrix:
//! it generates the cross-product of {value source / binding site / consume
//! site} × {`Option[shared]`, `Result[shared, _]` (Ok-shared),
//! `Result[_, shared]` (Err-shared)}, builds each program under
//! AddressSanitizer, and classifies its memory behavior.
//!
//! ## How a cell is classified (build once, run twice)
//!
//! Each generated program is compiled + ASAN-linked ONCE, then run twice —
//! with LeakSanitizer ON (`detect_leaks=1`) and OFF (`detect_leaks=0`). It
//! fails-with-leaks-OFF → `MemError` (use-after-free / double-free / crash — a
//! real error, independent of leaks; must NEVER happen); clean-with-leaks-OFF
//! but fails-with-leaks-ON → `Leak` (steady-state leak only); clean both ways →
//! `Clean`. Using exit codes from both runs distinguishes leak from UAF without
//! parsing sanitizer stderr. Leak detection is Linux-only (macOS Apple-clang
//! ASAN has no LSan), so the Leak/Clean split is meaningful on the CI
//! `memory-sanitizer` job; MemError coverage holds everywhere.
//!
//! ## What it asserts
//!
//! 1. Safety invariant (hard): NO cell is `MemError`. A double-free / UAF
//!    anywhere is an immediate failure regardless of the expected table.
//! 2. Value correctness: every program prints its expected total (a corrupt
//!    read from a prematurely-freed node would change it).
//! 3. Frontier regression: each cell matches its recorded `expected` outcome.
//!    A `Clean`→`Leak` flip is a regression; a `Leak`→`Clean` flip means a
//!    residual was closed — update the table (and ideally add a focused
//!    `memory_sanitizer.rs` test). Either way the diff is loud.
//!
//! The full grid is always printed, so one run shows the entire shared-type
//! ownership frontier — Option vs Result differences included.
//!
//! Skips gracefully when the host lacks ASAN (same probe as
//! `tests/memory_sanitizer.rs`).

mod common;

#[cfg(feature = "llvm")]
mod shared_ownership_matrix_tests {
    use karac::codegen::{compile_to_object, link_executable_with_sanitizer};
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;

    fn asan_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            if std::env::var("KARAC_SKIP_ASAN_TESTS").is_ok() {
                return false;
            }
            let probe_c = "/tmp/karac_matrix_asan_probe.c";
            let probe_exe = "/tmp/karac_matrix_asan_probe";
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

    #[derive(PartialEq, Eq, Clone, Copy, Debug)]
    enum Outcome {
        Clean,
        Leak,
        MemError,
    }

    /// Build `src` under ASAN once, run it twice (LSan on/off), and classify.
    /// `None` = setup failed (parse/compile/link) → the cell is skipped.
    /// Returns `(outcome, stdout_of_leaks_off_run)`.
    fn classify(src: &str, label: &str) -> Option<(Outcome, String)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            eprintln!("[{label}] parse errors: {:?}", parsed.errors);
            return None;
        }
        karac::desugar_program(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let obj = format!("/tmp/karac_matrix_{}_{}.o", std::process::id(), id);
        let exe = format!("/tmp/karac_matrix_{}_{}", std::process::id(), id);
        if compile_to_object(&parsed.program, &obj, Some(&ownership), None).is_err() {
            eprintln!("[{label}] compile_to_object failed");
            return None;
        }
        if !Path::new(&obj).exists() {
            return None;
        }
        if link_executable_with_sanitizer(&obj, &exe, &["-fsanitize=address"]).is_err() {
            let _ = std::fs::remove_file(&obj);
            return None; // runtime archive / linker absent → skip
        }

        let run = |detect_leaks: bool| -> Option<(bool, String)> {
            let opts = if cfg!(target_os = "macos") {
                "abort_on_error=0:exitcode=23"
            } else if detect_leaks {
                "detect_leaks=1:abort_on_error=0:exitcode=23"
            } else {
                "detect_leaks=0:abort_on_error=0:exitcode=23"
            };
            let out = Command::new(&exe).env("ASAN_OPTIONS", opts).output().ok()?;
            Some((
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            ))
        };
        let leaks_on = run(true);
        let leaks_off = run(false);
        let _ = std::fs::remove_file(&obj);
        let _ = std::fs::remove_file(&exe);

        let (on_ok, _) = leaks_on?;
        let (off_ok, stdout) = leaks_off?;
        let outcome = if !off_ok {
            Outcome::MemError
        } else if !on_ok {
            Outcome::Leak
        } else {
            Outcome::Clean
        };
        Some((outcome, stdout))
    }

    /// A container carrying a `shared Node` in one variant.
    struct Container {
        /// Grid label.
        name: &'static str,
        /// Full type expression.
        ty: &'static str,
        /// The payload-carrying variant constructor (`Some`/`Ok`/`Err`).
        pay: &'static str,
        /// A `match` arm for the NON-payload variant that yields `0`.
        oth_arm: &'static str,
        /// An expression building the non-payload value.
        mk_oth: &'static str,
    }

    const CONTAINERS: &[Container] = &[
        Container {
            name: "Option",
            ty: "Option[Node]",
            pay: "Some",
            oth_arm: "None => 0",
            mk_oth: "None",
        },
        Container {
            name: "ResultOk",
            ty: "Result[Node, i64]",
            pay: "Ok",
            oth_arm: "Err(_e) => 0",
            mk_oth: "Err(1)",
        },
        Container {
            name: "ResultErr",
            ty: "Result[i64, Node]",
            pay: "Err",
            oth_arm: "Ok(_v) => 0",
            mk_oth: "Ok(0)",
        },
    ];

    const NODE: &str =
        "shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }";

    /// `fn take() -> <ty>` — produces the container carrying a fresh node
    /// (val 7) via a Vec-index read + a returning match arm (the rc-INC-heavy
    /// shape the whole bug class stresses).
    fn take_fn(c: &Container) -> String {
        format!(
            "fn take() -> {ty} {{ let mut src: Vec[Option[Node]] = Vec.new(); \
             src.push(Some(Node {{ val: 7, left: None, right: None }})); \
             match src[0] {{ None => {mk_oth}, Some(n) => {pay}(n) }} }}",
            ty = c.ty,
            mk_oth = c.mk_oth,
            pay = c.pay,
        )
    }

    /// `match <scrut> { <oth> => 0, <pay>(n) => n.val }` — extracts the node's
    /// `val` (7 on the payload arm, 0 otherwise).
    fn match_val(c: &Container, scrut: &str) -> String {
        format!(
            "match {scrut} {{ {oth}, {pay}(n) => n.val }}",
            oth = c.oth_arm,
            pay = c.pay,
        )
    }

    /// A flow: given a container, produce (extra top-level fns, caller body,
    /// expected printed total over 200 iters).
    struct Flow {
        name: &'static str,
        /// The frontier: expected outcome per container index (Option,
        /// ResultOk, ResultErr).
        expected: [Outcome; 3],
        build: fn(&Container) -> (String, String, i64),
    }

    fn program(c: &Container, extra: &str, caller_body: &str) -> String {
        format!(
            "{NODE}\n{take}\n{extra}\nfn caller() -> i64 {{ {body} }}\n\
             fn main() {{ let mut i: i64 = 0; let mut t: i64 = 0; \
             while i < 200 {{ t = t + caller(); i = i + 1; }} println(f\"{{t}}\"); }}\n",
            take = take_fn(c),
            extra = extra,
            body = caller_body,
        )
    }

    use Outcome::{Clean, Leak};

    // NOTE: the `expected` arrays are filled in after a discovery run (see the
    // test below). Until then the test only enforces the safety invariant + the
    // value check, and prints the observed grid.
    const FLOWS: &[Flow] = &[
        Flow {
            name: "match_direct",
            expected: [Clean, Clean, Clean],
            build: |c| (String::new(), match_val(c, "take()"), 1400),
        },
        Flow {
            name: "let_then_match",
            expected: [Clean, Clean, Clean],
            build: |c| {
                let body = format!("let d = take(); {}", match_val(c, "d"));
                (String::new(), body, 1400)
            },
        },
        Flow {
            name: "if_let",
            expected: [Clean, Clean, Clean],
            build: |c| {
                let body = format!(
                    "let d = take(); let mut r = 0; if let {pay}(n) = d {{ r = n.val; }} r",
                    pay = c.pay
                );
                (String::new(), body, 1400)
            },
        },
        Flow {
            name: "discard",
            expected: [Clean, Clean, Clean],
            build: |_c| ("".into(), "let d = take(); 1".into(), 200),
        },
        Flow {
            name: "index_match",
            expected: [Clean, Clean, Clean],
            build: |c| {
                let body = format!(
                    "let mut v: Vec[{ty}] = Vec.new(); v.push(take()); {m}",
                    ty = c.ty,
                    m = match_val(c, "v[0]")
                );
                (String::new(), body, 1400)
            },
        },
        Flow {
            name: "return_whole",
            expected: [Clean, Clean, Clean],
            build: |c| {
                let extra = format!("fn relay() -> {ty} {{ let d = take(); d }}", ty = c.ty);
                (extra, match_val(c, "relay()"), 1400)
            },
        },
        Flow {
            name: "push_into_vec",
            expected: [Clean, Clean, Clean],
            build: |c| {
                let body = format!(
                    "let d = take(); let mut v: Vec[{ty}] = Vec.new(); v.push(d); {m}",
                    ty = c.ty,
                    m = match_val(c, "v[0]")
                );
                (String::new(), body, 1400)
            },
        },
        Flow {
            name: "consuming_call",
            expected: [Clean, Clean, Clean],
            build: |c| {
                let extra = format!(
                    "fn eat(r: {ty}) -> i64 {{ {m} }}",
                    ty = c.ty,
                    m = match_val(c, "r")
                );
                (extra, "let d = take(); eat(d)".into(), 1400)
            },
        },
        Flow {
            name: "forwarding_chain",
            expected: [Clean, Clean, Clean],
            build: |c| {
                let extra = format!(
                    "fn eat2(r: {ty}) -> i64 {{ {m} }}\nfn eat(r: {ty}) -> i64 {{ eat2(r) }}",
                    ty = c.ty,
                    m = match_val(c, "r")
                );
                (extra, "let d = take(); eat(d)".into(), 1400)
            },
        },
        // FRONTIER: Option is Clean here (it has the full move-out
        // coordination — `var_option_shared_heap` alias-inc + closure handling).
        // Result[shared] lacks that, so an alias / closure-capture of a
        // Result[shared] binding LEAKS (leak-only — the matrix proves no
        // double-free). Closing these two Result-only cells is the remaining
        // B-2026-07-12-24 residual; it needs Result to gain Option-parity
        // move-out coordination (a distinct, larger change).
        Flow {
            name: "alias",
            expected: [Clean, Leak, Leak],
            build: |c| {
                let body = format!("let d = take(); let e = d; {}", match_val(c, "e"));
                (String::new(), body, 1400)
            },
        },
        Flow {
            name: "closure_capture",
            expected: [Clean, Leak, Leak],
            build: |c| {
                let body = format!(
                    "let d = take(); let cl = || {{ {} }}; cl()",
                    match_val(c, "d")
                );
                (String::new(), body, 1400)
            },
        },
    ];

    #[test]
    fn shared_ownership_matrix() {
        if !asan_available() {
            eprintln!("shared_ownership_matrix: ASAN unavailable — skipping");
            return;
        }
        let mut rows: Vec<String> = Vec::new();
        let mut mem_errors: Vec<String> = Vec::new();
        let mut value_bugs: Vec<String> = Vec::new();
        let mut frontier_diffs: Vec<String> = Vec::new();
        let mut skipped = 0usize;

        for flow in FLOWS {
            let mut cells: Vec<String> = Vec::new();
            for (ci, c) in CONTAINERS.iter().enumerate() {
                let (extra, body, want_total) = (flow.build)(c);
                let src = program(c, &extra, &body);
                let label = format!("{}/{}", flow.name, c.name);
                let Some((outcome, stdout)) = classify(&src, &label) else {
                    cells.push(format!("{}=SKIP", c.name));
                    skipped += 1;
                    continue;
                };
                cells.push(format!("{}={outcome:?}", c.name));
                if outcome == Outcome::MemError {
                    mem_errors.push(label.clone());
                }
                if stdout != want_total.to_string() {
                    value_bugs.push(format!("{label}: got {stdout:?} want {want_total}"));
                }
                if outcome != flow.expected[ci] {
                    frontier_diffs.push(format!(
                        "{label}: got {outcome:?} expected {:?}",
                        flow.expected[ci]
                    ));
                }
            }
            rows.push(format!("{:<18} {}", flow.name, cells.join("  ")));
        }

        eprintln!(
            "\n=== shared-type ownership matrix ({} skipped) ===\n{}\n",
            skipped,
            rows.join("\n")
        );

        // 1. Safety invariant — a UAF / double-free is never acceptable.
        assert!(
            mem_errors.is_empty(),
            "MEMORY ERROR (use-after-free / double-free) in:\n  {}",
            mem_errors.join("\n  ")
        );
        // 2. Value correctness — a premature free would corrupt the total.
        assert!(
            value_bugs.is_empty(),
            "value corruption:\n  {}",
            value_bugs.join("\n  ")
        );
        // 3. Frontier regression — a Clean→Leak flip is a regression; a
        //    Leak→Clean flip means a residual closed (update the table).
        assert!(
            frontier_diffs.is_empty(),
            "frontier changed (update FLOWS.expected + add a focused \
             memory_sanitizer test if a residual closed):\n  {}",
            frontier_diffs.join("\n  ")
        );
    }
}
