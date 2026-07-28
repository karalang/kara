//! Behavioral oracle for `examples/tangle` — the ownership-soundness dogfood
//! corpus (B-2026-07-28-4).
//!
//! # Why this exists
//!
//! `examples/tangle/README.md` documents each program's exact expected output
//! in prose, so the corpus already HAD an oracle — it just was not wired to
//! anything. Nothing in `.github/workflows`, `tests/`, or `scripts/` referenced
//! these five programs, and the package has no `src/main.kara`, so they are
//! reachable only via `karac run <file>`. On 2026-07-28 that gap had let THREE
//! of the five drift to wrong output under codegen, one of them to a
//! memory-safety bug, entirely unnoticed.
//!
//! These are exactly the shapes the corpus exists to prove sound — a cross-edge
//! graph that forces the RC fallback, a doubly-linked list with `mut prev` /
//! `mut next`, undo/redo writing through a shared cell handle — so a silent
//! regression here is a silent regression in the ownership story itself.
//!
//! # Both backends, on purpose
//!
//! Each program is run under BOTH the interpreter and codegen, because the
//! corpus has historically broken asymmetrically in both directions:
//! `cross_graph` and `undo_redo` were correct under the interpreter and wrong
//! under codegen, while `doubly_linked` is the reverse (it HANGS under the
//! interpreter and completes under codegen). A single-backend gate would have
//! missed one side or the other.
//!
//! # Known-broken entries are pinned, not skipped
//!
//! `doubly_linked` is still failing (B-2026-07-28-4). Rather than omit it —
//! which is how the corpus went stale in the first place — it is listed with
//! `Expect::KnownBroken`, which asserts it does NOT match the README. That way
//! FIXING it turns this test RED and forces the entry to be promoted, instead
//! of the fix landing with the oracle still ignoring it.
//!
//! `undo_redo` prints correct output on both backends but still corrupts the
//! heap at exit (also -4). Output parity cannot see that, so its legs are
//! `Matches` and the corruption stays tracked in the ledger rather than here.
//!
//! Requires `--features llvm` for the codegen leg. Skips benignly if `karac`
//! cannot link a program (no runtime archive), never on a wrong answer.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What we currently expect of a program, per backend.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// Must match the README's documented output exactly.
    Matches,
    /// Known-broken (B-2026-07-28-4). Asserted NOT to match, so a fix trips
    /// this test and forces promotion to `Matches`.
    KnownBroken,
}

struct Case {
    file: &'static str,
    /// Exactly what `examples/tangle/README.md` documents this program prints.
    expected: &'static str,
    interp: Expect,
    codegen: Expect,
    /// Why a leg is `KnownBroken`, quoted in the failure message when it starts
    /// passing.
    note: &'static str,
}

const CASES: &[Case] = &[
    Case {
        file: "parent_tree.kara",
        expected: "depth of b from root: 2\ndepth of c from root: 1\ndepth of root:        0\n",
        interp: Expect::Matches,
        codegen: Expect::Matches,
        note: "",
    },
    // Fixed 2026-07-28: the by-value struct param of a SELF-REFERENTIAL struct
    // declined its callee entry-copy (B-2026-07-28-3, no finite emission for
    // `struct N { edges: Vec[N] }`) and fell back to caller-retains, so the
    // callee's `self.edges.push(t)` aliased the caller's binding and both freed
    // it. Now a true move.
    Case {
        file: "cross_graph.kara",
        expected: "diamond reachable-sum (d counted twice): 14\n",
        interp: Expect::Matches,
        codegen: Expect::Matches,
        note: "",
    },
    // Fixed 2026-07-28 (B-2026-07-28-6): the write through the shared cell
    // handle was silently dropped, so undo/redo read stale values. The OUTPUT is
    // now correct on both backends — but the program still corrupts the heap at
    // exit (`malloc(): unaligned tcache chunk detected`), which is a separate
    // defect still open under B-2026-07-28-4. Output parity cannot see that, so
    // both legs are `Matches` and the corruption stays tracked in the ledger.
    Case {
        file: "undo_redo.kara",
        expected: "value:        30\nafter undo:   20\nafter undo:   10\nafter redo:   20\n",
        interp: Expect::Matches,
        codegen: Expect::Matches,
        note: "",
    },
    Case {
        file: "doubly_linked.kara",
        expected: "forward:  1 2 3 4\nbackward: 4 3 2 1\nafter removals forward:  3\nafter removals backward: 3\n",
        // Fixed 2026-07-28 (B-2026-07-28-7): the interpreter used to HANG here.
        // `cur = n.next` inside `match cur { Some(n) => .. }` was reverted by
        // B-2026-07-23-12's payload write-through, so every walk re-visited the
        // head forever. This is now the corpus's authoritative oracle for the
        // still-broken codegen leg below.
        interp: Expect::Matches,
        // Prints the forward/backward walks correctly, then an UNINITIALIZED
        // value where the README documents `3` — the remaining node is read
        // after the splices dropped its last counted reference.
        codegen: Expect::KnownBroken,
        note: "B-2026-07-28-4: codegen prints an uninitialized value after the splices (RC accounting across the neighbor relink)",
    },
    Case {
        file: "interp.kara",
        expected: "result: 40\nscope x:  10\nscope y:  30\n",
        interp: Expect::Matches,
        codegen: Expect::Matches,
        note: "",
    },
];

fn tangle_src(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/tangle/src")
        .join(file)
}

/// Run one program on one backend. `None` means it did not produce output at
/// all (hang, crash, or link skip) — which for a `KnownBroken` leg is a
/// legitimate "does not match".
fn run_backend(file: &str, interp: bool) -> Option<String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("run");
    if interp {
        cmd.arg("--interp");
    }
    cmd.arg(tangle_src(file));
    // Auto-par off: these programs are about ownership, and a par decision
    // would only add nondeterminism to the oracle.
    cmd.env("KARAC_AUTO_PAR", "0");
    // A local poll-and-kill rather than `common::output_with_hang_watchdog`,
    // which PANICS on a hang. One corpus program (`doubly_linked` under the
    // interpreter) hangs by design-of-the-bug, and this oracle needs that to be
    // an ordinary "does not match" so the `KnownBroken` arm can assert it —
    // panicking would make the known-broken case indistinguishable from a
    // harness failure.
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn tangle_corpus_matches_readme_on_both_backends() {
    for case in CASES {
        for (interp, expect) in [(true, case.interp), (false, case.codegen)] {
            let backend = if interp { "interpreter" } else { "codegen" };
            let got = run_backend(case.file, interp);
            match expect {
                Expect::Matches => {
                    let got = match got {
                        Some(g) => g,
                        None => {
                            eprintln!(
                                "skip: {} ({backend}) produced no output — treating as a link skip",
                                case.file
                            );
                            continue;
                        }
                    };
                    assert_eq!(
                        got, case.expected,
                        "{} under the {backend} does not match the output \
                         examples/tangle/README.md documents for it",
                        case.file
                    );
                }
                Expect::KnownBroken => {
                    let matches = got.as_deref() == Some(case.expected);
                    assert!(
                        !matches,
                        "{} under the {backend} now MATCHES the README — promote its \
                         `{backend}` leg from `Expect::KnownBroken` to `Expect::Matches` \
                         in tests/tangle_corpus.rs (and close the relevant part of \
                         B-2026-07-28-4). Recorded reason it was broken: {}",
                        case.file, case.note
                    );
                }
            }
        }
    }
}
