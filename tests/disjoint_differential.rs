//! Differential harness for the auto-par **indexed-write fan-out** lowering —
//! the GATE sub-slice of "Auto-parallel loops over provably-disjoint indexed
//! writes" (`docs/implementation_checklist/phase-6-runtime.md`).
//!
//! ## Why this exists
//!
//! Wrong disjointness on indexed writes is a **silent miscompile**, never a
//! perf regression: the program keeps running and quietly produces different
//! bytes. That is the failure class `phase-7-codegen.md`'s ILP/noalias entry
//! documents at length via rustc's multi-year `-Zmutable-noalias` saga, and it
//! is not a hypothetical here — landing the lowering surfaced three real
//! defects, every one of them found by hand:
//!
//! 1. a tag-key collision that fanned out a loop nothing had proven,
//! 2. an inverted range that fanned out over ~2^64 iterations
//!    (`B-2026-07-31-8`),
//! 3. a hash-container target reported as a disjoint fan-out.
//!
//! Three by hand in one slice is the argument for a generator.
//!
//! ## The oracle is ORDER-SENSITIVE, deliberately
//!
//! Every generated program folds a **position-weighted digest over the whole
//! output buffer** (`d = d*131 + buf[i]`, every `i`) and prints it. A
//! single-element spot-check would pass under reordering *and* under a worker
//! writing the right value into the wrong slot — which is precisely how this
//! lowering fails. The digest catches both.
//!
//! ## The A/B lever
//!
//! The baseline compiles the same AST with **no `ConcurrencyAnalysis`**, which
//! leaves codegen with no auto-par tags at all. That is a strictly stronger
//! baseline than the user-facing `KARAC_AUTO_PAR=0` (which keeps the sequential
//! tabulate rewrite), and it avoids mutating a process-global env var from a
//! multi-threaded test binary. `auto_par_env_lever_disables_fanout` covers the
//! env lever itself once, so the spelling the checklist names stays pinned.
//!
//! ## Not silently vacuous
//!
//! A generator that drifts until nothing qualifies would pass every assertion
//! while testing nothing. `differential_corpus_actually_exercises_fanout`
//! asserts a floor on how many corpus programs really emit a fan-out worker,
//! and the fuzz test reports the rate it achieved.

mod common;

#[cfg(feature = "llvm")]
mod disjoint_differential_tests {
    use std::path::PathBuf;
    use std::sync::Once;

    // ── Runtime archive (soft-skip when absent) ──────────────────

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

    #[allow(static_mut_refs)]
    fn runtime_path() -> Option<PathBuf> {
        RUNTIME_BUILT.call_once(|| {
            let output = std::process::Command::new("cargo")
                .args(["build", "-p", "karac-runtime", "--release"])
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("target/release/libkarac_runtime.a");
                    if p.exists() {
                        unsafe {
                            RUNTIME_PATH = Some(p);
                        }
                    }
                }
            }
        });
        unsafe { RUNTIME_PATH.clone() }
    }

    // ── Deterministic PRNG ──────────────────────────────────────

    /// xorshift64*, so a failing run is reproducible from its seed alone. A
    /// fuzz harness whose corpus cannot be replayed is a bug report you cannot
    /// act on.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Rng(seed | 1)
        }
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        /// Uniform-ish in `[0, n)`.
        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n.max(1)
        }
        /// Uniform-ish in `[lo, hi]`.
        fn range(&mut self, lo: i64, hi: i64) -> i64 {
            lo + self.below((hi - lo + 1) as u64) as i64
        }
        fn chance(&mut self, one_in: u64) -> bool {
            self.below(one_in) == 0
        }
    }

    // ── Shape space ─────────────────────────────────────────────

    /// The structural axes the generator varies. Each is a dimension the proof
    /// or the lowering reasons about, so a bug in any of them shows up as a
    /// digest mismatch.
    #[derive(Debug, Clone)]
    struct Shape {
        /// Loop nesting depth, 1–3. Depth 3 is the shape whose *inner*
        /// coefficients are themselves symbolic (`(z*H + y)*W + x`).
        depth: u8,
        /// Outer loop's lower bound. Non-zero exercises the `lo`-shift path in
        /// the worker.
        lo: i64,
        /// Emit the loop with its bounds INVERTED (`for y in h..lo`), which
        /// runs zero iterations sequentially. Generated deliberately: the
        /// unclamped `iter_total` defect (`B-2026-07-31-8`) lived exactly here,
        /// and a generator whose `lo` is always below `h` never reaches it —
        /// measured, by removing the clamp and watching only the seeded case
        /// fail.
        inverted: bool,
        /// Loop-invariant offset the whole tiling sits at.
        base: i64,
        /// Outer trip count and inner dimensions, passed as *parameters* so the
        /// strides are symbolic atoms rather than folded literals.
        dims: (i64, i64, i64),
        /// Wrap the write in an `if`, narrowing the footprint per iteration.
        conditional: bool,
        /// Spell the innermost loop as a counted `while` rather than a `for`.
        inner_while: bool,
        /// Write a second, independently-strided target in the same loop.
        two_targets: bool,
        /// Build a per-iteration `Vec` and copy through it — exercises the
        /// worker's per-iteration cleanup frame.
        temp_vec: bool,
        /// Per-element work. Straddles the cost gate: low values are expected
        /// to decline, which must be just as output-identical.
        work: i64,
        /// Buffer prefill, so slots the loop never writes are still observable
        /// in the digest.
        fill: i64,
    }

    fn gen_shape(rng: &mut Rng) -> Shape {
        let depth = rng.range(1, 3) as u8;
        // `temp_vec` needs an inner dimension to fill, so it only applies at
        // depth >= 2; the same for a counted-`while` inner loop.
        let temp_vec = depth >= 2 && rng.chance(4);
        Shape {
            depth,
            lo: if rng.chance(3) { rng.range(1, 3) } else { 0 },
            inverted: rng.chance(8),
            base: if rng.chance(3) { rng.range(1, 5) } else { 0 },
            dims: (rng.range(8, 40), rng.range(1, 6), rng.range(1, 4)),
            conditional: !temp_vec && rng.chance(3),
            inner_while: depth >= 2 && !temp_vec && rng.chance(2),
            two_targets: !temp_vec && rng.chance(4),
            temp_vec,
            work: if rng.chance(4) {
                rng.range(1, 4)
            } else {
                rng.range(60, 260)
            },
            fill: rng.range(0, 9),
        }
    }

    /// Total buffer length the shape's index expression can reach. Sized
    /// exactly so a correct program never goes out of bounds — a bounds panic
    /// would be observationally identical on both legs and therefore pass the
    /// differential while testing nothing past the panic.
    fn buffer_len(s: &Shape) -> i64 {
        let (h, w, c) = s.dims;
        s.base
            + match s.depth {
                1 => h,
                2 => h * w,
                _ => h * w * c,
            }
    }

    // ── Rendering ───────────────────────────────────────────────

    const HEAVY: &str = "\
fn heavy(v: i64, work: i64) -> i64 {
    let mut a: i64 = v % 1000003;
    let mut t: i64 = 0;
    while t < work { a = (a * 1103515245 + 12345) % 2147483647; t = t + 1; }
    a
}
";

    /// The index expression, in the canonical row-major form the proof is built
    /// for: `base + ((y*w + x)*c + k)`.
    fn index_expr(s: &Shape) -> String {
        let core = match s.depth {
            1 => "y".to_string(),
            2 => "y * w + x".to_string(),
            _ => "(y * w + x) * c + k".to_string(),
        };
        if s.base == 0 {
            core
        } else {
            format!("{} + {core}", s.base)
        }
    }

    /// The value written — a function of every loop variable in scope, so a
    /// worker running the wrong iteration produces a different byte.
    fn value_expr(s: &Shape) -> String {
        let arg = match s.depth {
            1 => "y * 7",
            2 => "y * 7 + x * 3",
            _ => "y * 7 + x * 3 + k",
        };
        format!("heavy({arg}, work)")
    }

    fn render_program(s: &Shape) -> String {
        let (h, w, c) = s.dims;
        let len = buffer_len(s);
        let idx = index_expr(s);
        let val = value_expr(s);

        // Innermost write, optionally guarded.
        let write = if s.conditional {
            let guard = match s.depth {
                1 => "y % 3 != 0",
                _ => "(y + x) % 3 != 0",
            };
            format!("if {guard} {{ out[{idx}] = {val}; }}")
        } else {
            format!("out[{idx}] = {val};")
        };

        // Loop nest, innermost outward.
        let mut body = write;
        if s.depth >= 3 {
            body = format!("for k in 0..c {{ {body} }}");
        }
        if s.depth >= 2 {
            body = if s.inner_while {
                format!("let mut x: i64 = 0;\n            while x < w {{ {body} x = x + 1; }}")
            } else {
                format!("for x in 0..w {{ {body} }}")
            };
        }
        if s.two_targets {
            body = format!("{body}\n            out2[y] = heavy(y * 11, work);");
        }
        if s.temp_vec {
            // Route the row through a per-iteration `Vec`, then copy it out.
            body = format!(
                "let mut tmp: Vec[i64] = Vec.new();\n            \
                 let mut x: i64 = 0;\n            \
                 while x < w {{ tmp.push(heavy(y * 7 + x * 3, work)); x = x + 1; }}\n            \
                 let mut j: i64 = 0;\n            \
                 while j < w {{ out[{base}y * w + j] = tmp[j]; j = j + 1; }}",
                base = if s.base == 0 {
                    String::new()
                } else {
                    format!("{} + ", s.base)
                }
            );
        }

        let params = if s.two_targets {
            "h: i64, w: i64, c: i64, work: i64, out: mut Slice[i64], out2: mut Slice[i64]"
        } else {
            "h: i64, w: i64, c: i64, work: i64, out: mut Slice[i64]"
        };
        // An inverted shape writes the bounds backwards, so the loop runs zero
        // iterations and every slot keeps its prefill.
        let (lo_txt, hi_txt) = if s.inverted {
            ("h".to_string(), s.lo.max(1).to_string())
        } else {
            (s.lo.to_string(), "h".to_string())
        };
        let kernel = format!(
            "fn kernel({params}) {{\n        for y in {lo_txt}..{hi_txt} {{\n            {body}\n        }}\n    }}\n",
        );

        let mut main = String::new();
        main.push_str("fn main() {\n");
        main.push_str(&format!("    let h: i64 = {h};\n"));
        main.push_str(&format!("    let w: i64 = {w};\n"));
        main.push_str(&format!("    let c: i64 = {c};\n"));
        main.push_str(&format!("    let work: i64 = {};\n", s.work));
        main.push_str(&format!(
            "    let mut buf: Vec[i64] = Vec.filled({len}, {});\n",
            s.fill
        ));
        if s.two_targets {
            main.push_str(&format!(
                "    let mut buf2: Vec[i64] = Vec.filled({h}, {});\n",
                s.fill
            ));
            main.push_str("    kernel(h, w, c, work, mut buf, mut buf2);\n");
        } else {
            main.push_str("    kernel(h, w, c, work, mut buf);\n");
        }
        // Position-folded digest over the WHOLE buffer — the order-sensitive
        // oracle. A spot-check would not distinguish a reordering.
        main.push_str("    let mut d: i64 = 0;\n    let mut i: i64 = 0;\n");
        main.push_str(&format!(
            "    while i < {len} {{ d = (d * 131 + buf[i]) % 1000000007; i = i + 1; }}\n"
        ));
        if s.two_targets {
            main.push_str("    let mut j: i64 = 0;\n");
            main.push_str(&format!(
                "    while j < {h} {{ d = (d * 137 + buf2[j]) % 1000000007; j = j + 1; }}\n"
            ));
        }
        main.push_str("    println(f\"{d}\");\n}\n");

        format!("{HEAVY}\n    {kernel}\n{main}").replace("\n    fn kernel", "\nfn kernel")
    }

    // ── Compile / run ───────────────────────────────────────────

    /// Compile and run `src`. `fanout` selects the leg: with the analysis
    /// threaded in, codegen may emit a fan-out; without it there are no
    /// auto-par tags at all and every loop lowers sequentially.
    ///
    /// Returns `(stdout, emitted_fanout_worker)`.
    fn build_and_run(src: &str, fanout: bool) -> Option<(String, bool)> {
        use karac::codegen::{compile_to_ir, compile_to_object, link_executable};
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let rt = runtime_path()?;
        std::env::set_var("KARAC_RUNTIME", &rt);

        let mut parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "generated program failed to parse — the GENERATOR is broken, not the \
             compiler:\n{src}\nerrors: {:?}",
            parsed.errors
        );
        // Mirror the real CLI pipeline: desugar runs between parse and resolve.
        // It matters here because the query leg of the query/binary agreement
        // check goes through the actual `karac` binary, and a pipeline that
        // skips a stage the CLI runs would make the two disagree for reasons
        // that have nothing to do with fan-out.
        karac::desugar_program(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        assert!(
            typed.errors.is_empty(),
            "generated program failed to typecheck — the GENERATOR is broken:\n{src}\n\
             errors: {:?}",
            typed.errors
        );
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        let analysis = fanout
            .then(|| karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed)));

        let emitted = match &analysis {
            Some(a) => {
                let ir = compile_to_ir(&parsed.program, Some(&ownership), Some(a))
                    .expect("compile_to_ir failed");
                let workers = worker_definitions(&ir);
                let proven = proven_loop_count(a);
                // STRUCTURAL INVARIANT, checked on every case. Codegen may
                // decline a proven loop on cost, so workers <= proven; it may
                // never emit MORE workers than there are proven loops. The
                // tag-key collision violated exactly this — one proven loop,
                // two workers — and it is invisible to the digest, because the
                // runtime's fork-depth cap makes the spurious inner fan-out run
                // inline and produce the right answer anyway. Output comparison
                // alone cannot see that class; this can.
                assert!(
                    workers <= proven,
                    "emitted {workers} fan-out workers for {proven} proven loop(s) — a tag \
                     was applied to a loop it does not name:\n{src}"
                );
                // QUERY ↔ BINARY AGREEMENT. `karac query concurrency` promises
                // `fanned_out` describes the emitted binary, and a surface that
                // says "yes" where codegen says "no" is the defect
                // B-2026-07-29-29 was filed for and B-2026-07-29-33 tightened.
                // Neither the digest nor the worker count sees a query that
                // lies — the binary is right either way — so it is asserted
                // directly, against the real CLI.
                if let Some(reported) = query_reported_fanouts(src) {
                    assert_eq!(
                        reported, workers,
                        "`karac query concurrency` reports {reported} fanned-out loop(s) but \
                         codegen emitted {workers} worker(s) — the query surface disagrees \
                         with its own binary:\n{src}"
                    );
                }
                workers > 0
            }
            None => false,
        };

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tag = if fanout { "par" } else { "seq" };
        let obj = format!("/tmp/karac_diff_{}_{id}_{tag}.o", std::process::id());
        let exe = format!("/tmp/karac_diff_{}_{id}_{tag}", std::process::id());

        if let Err(e) =
            compile_to_object(&parsed.program, &obj, Some(&ownership), analysis.as_ref())
        {
            panic!("codegen failed ({tag} leg):\n{src}\nerror: {e}");
        }
        super::common::link_or_skip(link_executable(&obj, &exe))?;
        let out = super::common::output_with_hang_watchdog(
            std::process::Command::new(&exe),
            std::time::Duration::from_secs(90),
        )?;
        let _ = std::fs::remove_file(&obj);
        let _ = std::fs::remove_file(&exe);
        Some((
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            emitted,
        ))
    }

    /// Run both legs and assert byte-identical output. Returns whether the
    /// fan-out leg actually emitted a worker, so callers can measure coverage.
    fn assert_legs_agree(src: &str, label: &str) -> Option<bool> {
        let (par, emitted) = build_and_run(src, true)?;
        let (seq, _) = build_and_run(src, false)?;
        assert_eq!(
            par, seq,
            "FAN-OUT CHANGED THE PROGRAM'S OUTPUT ({label})\n\
             fan-out digest: {par}\nsequential digest: {seq}\nprogram:\n{src}"
        );
        assert!(
            !par.is_empty(),
            "both legs produced no output ({label}) — the digest never printed, so \
             this case proves nothing:\n{src}"
        );
        Some(emitted)
    }

    // ── Fuzz ────────────────────────────────────────────────────

    /// Read a `u64` knob, accepting decimal or a `0x` prefix.
    ///
    /// The hex arm is load-bearing, not a nicety: the fuzz failure message
    /// prints its seed with `{:#x}`, so the value a developer copies back is
    /// hex. A decimal-only parse silently fell through to the DEFAULT seed and
    /// reproduced a different corpus — a reproduction knob that quietly
    /// reproduces the wrong thing is worse than no knob.
    fn env_u64(name: &str, default: u64) -> u64 {
        let Ok(raw) = std::env::var(name) else {
            return default;
        };
        let raw = raw.trim();
        let parsed = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            Some(hex) => u64::from_str_radix(hex, 16),
            None => raw.parse(),
        };
        parsed.unwrap_or_else(|_| panic!("{name}: cannot parse {raw:?} as a number"))
    }

    #[test]
    fn differential_fuzz_fanout_matches_sequential() {
        // Default sized for CI (each case is two builds, two links, two runs).
        // `KARAC_DISJOINT_FUZZ_CASES` / `_SEED` crank it up for a real fuzzing
        // session; the seed makes any failure replayable.
        let cases = env_u64("KARAC_DISJOINT_FUZZ_CASES", 24);
        let seed = env_u64("KARAC_DISJOINT_FUZZ_SEED", 0x5EED_1D15_0117_0000);
        let mut rng = Rng::new(seed);

        let mut ran = 0u64;
        let mut fanned = 0u64;
        for i in 0..cases {
            let shape = gen_shape(&mut rng);
            let src = render_program(&shape);
            let label = format!("seed={seed:#x} case={i} shape={shape:?}");
            let Some(emitted) = assert_legs_agree(&src, &label) else {
                // Runtime archive missing — soft-skip the whole test, matching
                // the rest of the E2E suite.
                eprintln!("skipping differential fuzz: runtime archive unavailable");
                return;
            };
            ran += 1;
            fanned += u64::from(emitted);
        }
        assert!(ran > 0, "no cases ran");
        eprintln!(
            "differential fuzz: {ran} cases, {fanned} emitted a fan-out worker (seed {seed:#x})"
        );
        // A generator that drifts until nothing qualifies would pass every
        // assertion above while testing nothing. Fail loudly instead.
        assert!(
            fanned * 4 >= ran,
            "only {fanned}/{ran} generated programs emitted a fan-out — the corpus has \
             drifted away from the shape under test and is close to vacuous (seed {seed:#x})"
        );
    }

    // ── Adversarial: shapes that MUST decline ───────────────────
    //
    // The happy-path corpus above proves the lowering is faithful where it
    // fires. These are the other half of the gate: shapes where two iterations
    // genuinely DO touch the same slot, so a proof that were even slightly too
    // permissive would fan them out and corrupt the buffer.
    //
    // Each asserts the strong, deterministic property — no worker emitted —
    // with the digest comparison as the backstop. Checking only the digest
    // would be flaky here: an overlapping fan-out races, and a race can happen
    // to produce the sequential answer.

    /// `(source, why it must decline)`. Dimensions are chosen so the overlap is
    /// real, not merely unproven.
    /// How many fan-out workers the module defines.
    fn worker_definitions(ir: &str) -> usize {
        ir.lines()
            .filter(|l| l.starts_with("define") && l.contains("@__karac_disjoint_worker_"))
            .count()
    }

    /// How many loops the analysis proved disjoint, across every function.
    fn proven_loop_count(analysis: &karac::concurrency::ConcurrencyAnalysis) -> usize {
        analysis
            .function_decisions
            .values()
            .flat_map(|fc| fc.disjoint_write_loops.iter())
            .filter(|d| d.proven())
            .count()
    }

    /// How many loops `karac query concurrency` reports as `fanned_out` for
    /// `src`, via the real CLI — the same surface a developer reads.
    ///
    /// Counted by substring rather than parsed: the field is emitted by one
    /// `format!` with no whitespace, so `"fanned_out":true` is exact, and a
    /// dependency-free check keeps this harness runnable anywhere the compiler
    /// builds.
    fn query_reported_fanouts(src: &str) -> Option<usize> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/karac_diff_q_{}_{id}.kara", std::process::id());
        std::fs::write(&path, src).ok()?;
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["query", "concurrency", &path])
            .output()
            .ok()?;
        let _ = std::fs::remove_file(&path);
        let text = String::from_utf8_lossy(&out.stdout);
        Some(text.matches("\"fanned_out\":true").count())
    }

    /// Analysis verdict for a function's OUTERMOST disjoint-write candidate:
    /// `(proven, gate)`. Records are pushed outer-first by the walk, so index 0
    /// is the outer loop.
    fn outer_loop_verdict(src: &str, func: &str) -> Option<(bool, String)> {
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed));
        let fc = analysis.function_decisions.get(func)?;
        let d = fc.disjoint_write_loops.first()?;
        Some((d.proven(), d.tag().to_string()))
    }

    fn adversarial_cases() -> Vec<(String, &'static str, &'static str)> {
        /// Assemble one case: a kernel body, the `kernel(...)` call arguments,
        /// and the buffer length the digest folds over.
        fn case(sig: &str, body: &str, call: &str, len: i64, extra_main: &str) -> String {
            format!(
                "{HEAVY}\n                 fn kernel({sig}) {{\n    {body}\n}}\n\n                 fn main() {{\n                 {extra_main}                 \x20   let mut buf: Vec[i64] = Vec.filled({len}, 4);\n                 \x20   kernel({call});\n                 \x20   let mut d: i64 = 0;\n                 \x20   let mut i: i64 = 0;\n                 \x20   while i < {len} {{ d = (d * 131 + buf[i]) % 1000000007; i = i + 1; }}\n                 \x20   println(f\"{{d}}\");\n                 }}\n"
            )
        }

        vec![
            (
                // The inner loop ranges WIDER than the stride (`x in 0..w2`
                // against a stride of `w`, called with w2 > w), so iteration y
                // runs past y+1's base and the two share slots.
                //
                // Note what this case is NOT: `out[y*w + x + 1]` with
                // `x in 0..w`. That reads like an overlap and is not one — the
                // `+1` is a loop-INVARIANT base that shifts every iteration's
                // range by the same amount, so the tiling still partitions.
                // The first draft of this case made exactly that mistake and
                // the harness caught it, which is the point of writing the
                // adversarial half against the arithmetic rather than intuition.
                case(
                    "h: i64, w: i64, w2: i64, work: i64, out: mut Slice[i64]",
                    "for y in 0..h { for x in 0..w2 { out[y * w + x] = heavy(y * 7 + x, work); } }",
                    "16, 4, 6, 200, mut buf",
                    200,
                    "",
                ),
                "inner range wider than the stride",
                "footprint_overlap",
            ),
            (
                // Constant stride 4 against a runtime inner bound: overlaps
                // whenever `w > 4`, and it is called with 9.
                case(
                    "h: i64, w: i64, work: i64, out: mut Slice[i64]",
                    "for y in 0..h { for x in 0..w { out[y * 4 + x] = heavy(y * 7 + x, work); } }",
                    "16, 9, 200, mut buf",
                    200,
                    "",
                ),
                "constant stride narrower than the runtime inner bound",
                "footprint_overlap",
            ),
            (
                // Integer division folds two iterations onto one row.
                case(
                    "h: i64, w: i64, work: i64, out: mut Slice[i64]",
                    "for y in 0..h { for x in 0..w { out[(y / 2) * w + x] = heavy(y * 7 + x, work); } }",
                    "16, 4, 200, mut buf",
                    200,
                    "",
                ),
                "division folds distinct iterations onto one row",
                "non_affine_index",
            ),
            (
                // Modulo wraps the row index.
                case(
                    "h: i64, w: i64, work: i64, out: mut Slice[i64]",
                    "for y in 0..h { for x in 0..w { out[(y % 5) * w + x] = heavy(y * 7 + x, work); } }",
                    "16, 4, 200, mut buf",
                    200,
                    "",
                ),
                "modulo wraps distinct iterations onto one row",
                "non_affine_index",
            ),
            (
                // A stencil reading the row the previous iteration wrote — a
                // genuine loop-carried dependency, and the one shape here whose
                // sequential answer NO parallel schedule reproduces.
                case(
                    "h: i64, w: i64, work: i64, out: mut Slice[i64]",
                    "for y in 1..h { for x in 0..w { out[y * w + x] = out[(y - 1) * w + x] + heavy(x, work); } }",
                    "16, 4, 200, mut buf",
                    200,
                    "",
                ),
                "reads the slot a previous iteration wrote",
                "reads_written_target",
            ),
            (
                // Indirect indexing — explicitly out of scope, and this index
                // table maps every iteration to slot 0.
                case(
                    "h: i64, work: i64, idx: ref Slice[i64], out: mut Slice[i64]",
                    "for y in 0..h { out[idx[y]] = heavy(y * 7, work); }",
                    "16, 200, tbl, mut buf",
                    200,
                    "    let mut tbl: Vec[i64] = Vec.filled(16, 0);\n",
                ),
                "indirect index",
                "indirect_index",
            ),
        ]
    }

    #[test]
    fn adversarial_overlapping_shapes_decline_the_outer_loop() {
        for (src, why, expected_gate) in adversarial_cases() {
            // The property under test is about the OUTER loop, not about the
            // whole program. When the outer loop declines, the walk recurses
            // and an INNER loop may legitimately fan out — for a fixed `y`,
            // distinct `x` really are disjoint, even in a shape whose `y`
            // iterations overlap. Asserting "no worker anywhere" would flag
            // that correct behavior, which the first draft of this test did.
            let (proven, gate) =
                outer_loop_verdict(&src, "kernel").expect("no candidate loop found");
            assert!(
                !proven,
                "a shape whose outer iterations OVERLAP was proven disjoint ({why}) — \
                 two `y` values write the same slot, so fanning this out is a live data \
                 race:\n{src}"
            );
            assert_eq!(
                gate, expected_gate,
                "declined for the wrong reason ({why}) — a decline that fires on an \
                 unrelated obligation would still pass a bare `!proven` check while the \
                 obligation meant to catch this shape had silently stopped working:\n{src}"
            );
            // Output equality is the backstop: it holds whether the decline was
            // for the right reason or not, and covers any inner-loop fan-out
            // the recursion did emit.
            if assert_legs_agree(&src, why).is_none() {
                eprintln!("skipping adversarial cases: runtime archive unavailable");
                return;
            }
        }
    }

    // ── Seeded regressions: the three defects found by hand ─────

    #[test]
    fn differential_regression_nested_loop_sharing_parents_line() {
        // Defect 1. Under `(stmt_index, loop_line)` keying the inner loop
        // inherited the outer's tag and fanned out — over iterations that all
        // write the SAME slot. The outer proof says nothing about two `x`
        // values being distinct.
        let src = concat!(
            "fn heavy(v: i64, work: i64) -> i64 {\n",
            "    let mut a: i64 = v % 1000003;\n",
            "    let mut t: i64 = 0;\n",
            "    while t < work { a = (a * 1103515245 + 12345) % 2147483647; t = t + 1; }\n",
            "    a\n",
            "}\n",
            "fn kernel(h: i64, w: i64, work: i64, out: mut Slice[i64]) {\n",
            "    for y in 0..h { for x in 0..w { out[y * w] = heavy(x, work); } }\n",
            "}\n",
            "fn main() {\n",
            "    let mut buf: Vec[i64] = Vec.filled(64, 3);\n",
            "    kernel(8, 8, 200, mut buf);\n",
            "    let mut d: i64 = 0;\n",
            "    let mut i: i64 = 0;\n",
            "    while i < 64 { d = (d * 131 + buf[i]) % 1000000007; i = i + 1; }\n",
            "    println(f\"{d}\");\n",
            "}\n",
        );
        let _ = assert_legs_agree(src, "nested-loop-shares-parent-line");
    }

    #[test]
    fn differential_regression_inverted_range() {
        // Defect 2 (`B-2026-07-31-8`). `for y in 5..3` runs zero iterations
        // sequentially; an unclamped negative `iter_total` in a `u64`
        // descriptor field fanned out over ~2^64.
        let src = concat!(
            "fn heavy(v: i64, work: i64) -> i64 {\n",
            "    let mut a: i64 = v % 1000003;\n",
            "    let mut t: i64 = 0;\n",
            "    while t < work { a = (a * 1103515245 + 12345) % 2147483647; t = t + 1; }\n",
            "    a\n",
            "}\n",
            "fn kernel(lo: i64, hi: i64, w: i64, work: i64, out: mut Slice[i64]) {\n",
            "    for y in lo..hi { for x in 0..w { out[y * w + x] = heavy(y * 3 + x, work); } }\n",
            "}\n",
            "fn main() {\n",
            "    let mut buf: Vec[i64] = Vec.filled(64, 7);\n",
            "    kernel(5, 3, 8, 200, mut buf);\n",
            "    let mut d: i64 = 0;\n",
            "    let mut i: i64 = 0;\n",
            "    while i < 64 { d = (d * 131 + buf[i]) % 1000000007; i = i + 1; }\n",
            "    println(f\"{d}\");\n",
            "}\n",
        );
        let _ = assert_legs_agree(src, "inverted-range");
    }

    #[test]
    fn differential_regression_hash_container_target() {
        // Defect 3. `m[i] = v` on a `Map[i64, V]` is spelled like an element
        // store but is a hash insert. Codegen declined it, so the binary was
        // never wrong — this pins that it stays declined AND stays correct.
        let src = concat!(
            "fn heavy(v: i64, work: i64) -> i64 {\n",
            "    let mut a: i64 = v % 1000003;\n",
            "    let mut t: i64 = 0;\n",
            "    while t < work { a = (a * 1103515245 + 12345) % 2147483647; t = t + 1; }\n",
            "    a\n",
            "}\n",
            "fn kernel(n: i64, work: i64, m: mut ref Map[i64, i64]) {\n",
            "    for i in 0..n { m[i] = heavy(i, work); }\n",
            "}\n",
            "fn main() {\n",
            "    let mut m: Map[i64, i64] = Map.new();\n",
            "    kernel(64, 200, mut m);\n",
            "    let mut d: i64 = 0;\n",
            "    let mut i: i64 = 0;\n",
            "    while i < 64 { d = (d * 131 + m[i]) % 1000000007; i = i + 1; }\n",
            "    println(f\"{d}\");\n",
            "}\n",
        );
        let Some(emitted) = assert_legs_agree(src, "hash-container-target") else {
            return;
        };
        assert!(
            !emitted,
            "a hash container must never receive an indexed-write fan-out"
        );
    }

    // ── Coverage + the env lever ────────────────────────────────

    #[test]
    fn differential_corpus_actually_exercises_fanout() {
        // Guards the fuzz test against becoming vacuous by a different route:
        // here the shapes are FIXED and known-qualifying, so a zero means the
        // lowering stopped firing entirely rather than that the generator
        // drifted.
        let shapes = [
            Shape {
                depth: 2,
                lo: 0,
                inverted: false,
                base: 0,
                dims: (32, 4, 1),
                conditional: false,
                inner_while: false,
                two_targets: false,
                temp_vec: false,
                work: 200,
                fill: 0,
            },
            Shape {
                depth: 3,
                lo: 2,
                inverted: false,
                base: 3,
                dims: (24, 3, 2),
                conditional: true,
                inner_while: true,
                two_targets: false,
                temp_vec: false,
                work: 200,
                fill: 5,
            },
        ];
        let mut any = false;
        for (i, s) in shapes.iter().enumerate() {
            let src = render_program(s);
            let Some(emitted) = assert_legs_agree(&src, &format!("fixed shape {i}")) else {
                return;
            };
            any |= emitted;
        }
        assert!(
            any,
            "no fixed known-qualifying shape emitted a fan-out — the lowering is not \
             firing at all and every differential assertion is vacuous"
        );
    }

    #[test]
    fn auto_par_env_lever_disables_fanout() {
        // The checklist names `KARAC_AUTO_PAR=0` as the A/B lever, so it gets a
        // pin — but through a SUBPROCESS, never `std::env::set_var`.
        //
        // The first version set the variable in-process around one
        // `compile_to_ir`. Rust runs tests in threads of one process, so that
        // briefly disabled auto-par for every other test compiling at the same
        // instant, and three of them failed with "query says 1, codegen emitted
        // 0" — the exact race this file's module docs claim to avoid. Passing
        // the variable to a child process scopes it correctly and tests the
        // real user-facing lever end to end.
        let Some(rt) = runtime_path() else {
            eprintln!("skipping env-lever test: runtime archive unavailable");
            return;
        };
        let s = Shape {
            depth: 2,
            lo: 0,
            inverted: false,
            base: 0,
            dims: (32, 4, 1),
            conditional: false,
            inner_while: false,
            two_targets: false,
            temp_vec: false,
            work: 200,
            fill: 0,
        };
        let src = render_program(&s);
        let dir = format!("/tmp/karac_diff_lever_{}", std::process::id());
        let _ = std::fs::create_dir_all(&dir);
        let kara = format!("{dir}/lever.kara");
        std::fs::write(&kara, &src).expect("write program");

        // `karac build` emits `<stem>` next to the source; build each leg in
        // its own directory so the two never collide.
        let build = |auto_par: Option<&str>| -> Option<Vec<u8>> {
            let leg = format!("{dir}/{}", auto_par.unwrap_or("on"));
            let _ = std::fs::create_dir_all(&leg);
            std::fs::write(format!("{leg}/lever.kara"), &src).ok()?;
            let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_karac"));
            // `karac build` writes its executable into the CURRENT DIRECTORY,
            // not beside the source. Without this the read below missed, the
            // helper returned `None`, and the test soft-skipped while reporting
            // a pass — and left a stray `lever` binary in the repo root, which
            // is how it was noticed.
            cmd.current_dir(&leg)
                .args(["build", "lever.kara"])
                .env("KARAC_RUNTIME", &rt);
            if let Some(v) = auto_par {
                cmd.env("KARAC_AUTO_PAR", v);
            }
            let out = cmd.output().ok()?;
            if !out.status.success() {
                panic!(
                    "karac build failed (KARAC_AUTO_PAR={:?}):\n{}",
                    auto_par,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            let built = std::fs::read(format!("{leg}/lever")).ok();
            assert!(
                built.is_some(),
                "karac build reported success but produced no binary at {leg}/lever — \
                 the helper would otherwise soft-skip and this test would pass vacuously"
            );
            built
        };

        let Some(on) = build(None) else {
            eprintln!("skipping env-lever test: build produced no binary");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let Some(off) = build(Some("0")) else {
            eprintln!("skipping env-lever test: build produced no binary");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let _ = std::fs::remove_dir_all(&dir);

        // Debug builds keep their symbol table, so the worker's name is present
        // verbatim in the emitted file.
        let needle = b"__karac_disjoint_worker_";
        let has = |bytes: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(
            has(&on),
            "baseline: this shape must fan out for the lever test to mean anything"
        );
        assert!(
            !has(&off),
            "KARAC_AUTO_PAR=0 must suppress the indexed-write fan-out"
        );
    }
}
