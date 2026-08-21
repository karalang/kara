//! GPU **execution** E2E — the first tests in the suite that actually run a
//! `#[gpu]` kernel on a device.
//!
//! # Why this file exists
//!
//! Before it, the repo had ~101 GPU tests across seven files and **not one
//! executed a shader**: they assert on WGSL *text* (or on the WGSL string
//! baked into the emitted LLVM IR). That is a real gap rather than a
//! stylistic one, because the emitter's hard parts are *semantic*:
//!
//! - A kernel local named `i` is RENAMED (`i` → `i_k`) because `i` is the
//!   generated wrapper's thread index and the kernel parameter lowers to
//!   `input[i]`. Get the rename wrong and every thread reads the wrong
//!   element — while the shader still compiles and still *looks* right.
//! - A value `match` / `if` lowers to a branchless `select()` chain, which
//!   evaluates every arm. Text assertions cannot tell a correct chain from
//!   one whose arms are ordered wrongly.
//!
//! Both classes produce plausible WGSL that computes the wrong answer, so the
//! only honest oracle is running the thing. Each fixture below asserts a
//! three-way agreement: **interpreter == GPU == a hardcoded expected string**.
//! The interpreter leg catches run/build divergence; the hardcoded leg catches
//! the case where both legs are wrong the same way (i.e. the fixture author
//! misunderstood the semantics).
//!
//! # Which device it runs on
//!
//! The probe RESOLVES a backend rather than pinning one (B-2026-08-19-9): an
//! explicit `KARAC_GPU_BACKEND` wins, then the platform's default adapter, then
//! a forced software one. So macOS runs these fixtures on **Metal** (naga →
//! MSL) and a GPU-less Linux box runs them on **lavapipe** (naga → SPIR-V),
//! which is worth more than either alone — the two exercise different naga
//! backends, so a lowering that is only valid in one shader language fails in
//! exactly one lane.
//!
//! On Linux, `mesa-vulkan-drivers` provides lavapipe, so the real naga + Vulkan
//! pipeline runs on a GPU-less machine, CI runners included. That is also the
//! automated coverage of CG-6's `KARAC_GPU_BACKEND=cpu` leg, which was deferred
//! as "Linux-container territory" when the rest of CG-6 landed on Metal.
//!
//! # No vacuous passes
//!
//! Without an adapter these tests soft-skip, which is the same hole
//! B-2026-07-28-1 documented for the runtime archives: a suite that reports
//! green while asserting nothing. `KARAC_REQUIRE_GPU_ADAPTER=1` turns every
//! skip into a hard failure; the CI lane sets it, so a broken
//! driver-install step fails the job instead of quietly covering nothing.
//! Note this is the *second* line of defense — the runtime's own
//! `doubles_an_f32_buffer_on_the_gpu` and `select_adapter_honors_backend_cpu`
//! unit tests already pass adapterlessly by design.

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Wraps a kernel in a `main` that dispatches it over `buf` and prints each
/// element, so a fixture only has to spell the kernel and the input.
fn program(kernel: &str, elem_ty: &str, literals: &str) -> String {
    format!(
        "{kernel}\n\
         fn main() {{\n\
         \x20   let buf: Vec[{elem_ty}] = [{literals}];\n\
         \x20   let out = gpu.dispatch(k, buf);\n\
         \x20   for v in out {{ println(f\"{{v}}\") }}\n\
         }}\n"
    )
}

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

/// A private directory per fixture. `karac build` writes the binary to CWD and
/// has no `-o` flag, so tests running in parallel would otherwise race on one
/// output path.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-gpu-e2e-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn run_interp(src_path: &Path) -> String {
    let out = karac()
        .args(["run", "--interp"])
        .arg(src_path)
        .output()
        .expect("spawn karac run --interp");
    assert!(
        out.status.success(),
        "`karac run --interp` failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The DEFAULT `karac run` — the JIT lane, not `--interp`.
///
/// A separate helper because it exercises a different failure mode, and one
/// this suite was blind to until it bit: the JIT runner's runtime rlib is
/// built without the opt-in `gpu` feature, so a program reaching
/// `karac_runtime_gpu_*` must be ROUTED to the interpreter by
/// `program_uses_gpu_runtime`. Every reduction did reach it and none was
/// routed — `gpu.sum` died with `Symbols not found:
/// [ karac_runtime_gpu_reduce_f32 ]` under `karac run` while `karac build`
/// answered correctly. Testing only `--interp` cannot see that, because
/// `--interp` is precisely the lane the routing is supposed to select.
fn run_default(src_path: &Path) -> String {
    let out = karac()
        .arg("run")
        .arg(src_path)
        .output()
        .expect("spawn karac run");
    assert!(
        out.status.success(),
        "`karac run` (JIT lane) failed — a GPU program must be ROUTED to the \
         interpreter, not handed to a JIT that cannot resolve its symbols:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `Ok(stdout)`, or `Err(diagnostic)` when the program could not be built or
/// the device refused to run it. The caller decides whether an error is a
/// skip or a failure — only [`gpu_available`] treats it as "no adapter".
fn build_and_run_on_gpu(
    dir: &Path,
    src_path: &Path,
    stem: &str,
    backend: &Backend,
) -> Result<String, String> {
    let built = karac()
        .arg("build")
        .arg(src_path)
        .current_dir(dir)
        .output()
        .expect("spawn karac build");
    if !built.status.success() {
        return Err(format!(
            "karac build failed:\n{}{}",
            String::from_utf8_lossy(&built.stdout),
            String::from_utf8_lossy(&built.stderr)
        ));
    }
    let bin = dir.join(stem);
    if !bin.exists() {
        // The `llvm`-less binary type-checks and reports success without
        // emitting anything; `#![cfg(feature = "llvm")]` should make that
        // unreachable, so say so plainly rather than failing on a missing file.
        return Err(format!(
            "karac build reported success but produced no binary at {}; \
             was karac built without --features llvm?",
            bin.display()
        ));
    }
    let mut run = Command::new(&bin);
    match backend {
        Some(b) => run.env("KARAC_GPU_BACKEND", b),
        // Unset rather than absent-by-accident: the parent `cargo test` process
        // may itself have been run with `KARAC_GPU_BACKEND` exported, and a
        // child inherits it. Leaving it to chance would silently ignore the
        // backend the probe resolved.
        None => run.env_remove("KARAC_GPU_BACKEND"),
    };
    let out = run
        .current_dir(dir)
        .output()
        .expect("spawn built GPU binary");
    if !out.status.success() {
        return Err(format!(
            "GPU binary exited {:?}:\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Which adapter every fixture runs on, resolved once by the probe.
///
/// `None` leaves `KARAC_GPU_BACKEND` unset, so the runtime takes the platform's
/// preferred device — Metal on macOS, a real Vulkan/DX device elsewhere.
/// `Some("cpu")` forces a software adapter (lavapipe / WARP).
type Backend = Option<String>;

/// The backends to try, in order, given whatever `KARAC_GPU_BACKEND` holds.
///
/// Pure so it can be pinned by a test on a host with no GPU at all — which is
/// exactly the host where getting this order wrong costs the most, since there
/// nothing else in this file asserts anything.
///
/// An explicit value is honored verbatim and NOT retried: someone forcing the
/// software lane on a host that HAS a device means it (it is how a CI job pins
/// lavapipe), and silently falling back to the real GPU would report a pass for
/// a lane that never ran.
fn backend_candidates(forced: Option<String>) -> Vec<Backend> {
    match forced {
        Some(b) => vec![Some(b)],
        None => vec![None, Some("cpu".to_string())],
    }
}

/// Why no device was reached, phrased for what was actually TRIED.
///
/// Forcing `KARAC_GPU_BACKEND` suppresses the fallback, so on a macOS box with
/// a working Metal device `=cpu` legitimately finds nothing. Reporting that as
/// "this host has no adapter" would be false, and false in the direction that
/// makes someone go looking for a driver problem they do not have.
/// Pure in its input for the same reason [`backend_candidates`] is: a test that
/// read the ambient `KARAC_GPU_BACKEND` would pass or fail depending on how the
/// suite was invoked.
fn no_adapter_reason(forced: Option<String>) -> String {
    match forced {
        Some(b) => format!(
            "KARAC_GPU_BACKEND={b} is set, so only that backend was tried and no adapter \
             matched it. Unset it to use whatever device this host has"
        ),
        None => "this host offers neither a default adapter nor a software one (on \
                 Linux, `mesa-vulkan-drivers` provides lavapipe)"
            .to_string(),
    }
}

/// Human wording for a resolved [`Backend`], for the one-line banner.
fn backend_label(backend: &Backend) -> &str {
    match backend {
        None => "the platform's default adapter",
        Some(_) => "a forced software (CPU) adapter",
    }
}

/// Why the probe kernel could not run — the distinction that decides whether
/// a failure may be skipped.
enum Probe {
    /// A device is there, reached via this backend.
    Ready(Backend),
    /// The host genuinely has no device. Skippable.
    NoAdapter(String),
    /// The optional `libkarac_runtime_gpu.a` has not been built on this host, so
    /// the probe never got as far as asking for a device. Skippable, unless
    /// `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` — B-2026-08-18-51.
    NoArchive(String),
    /// A device was there and the program still failed: invalid WGSL, a build
    /// error, a runtime fault. Never skippable — see [`gpu_or_skip`].
    Broken(String),
}

/// The environment's own "no device here" signatures, from
/// `runtime/src/gpu.rs`. Matching is on these rather than on "any failure"
/// because everything else is a compiler bug wearing a skip's clothing.
fn is_no_adapter(err: &str) -> bool {
    err.contains("no software (CPU) adapter is available")
        || err.contains("found no available GPU adapter")
        || err.contains("(no adapters found)")
}

/// Has the OPTIONAL GPU runtime archive simply not been built here?
///
/// B-2026-08-18-51. CLAUDE.md makes `libkarac_runtime_gpu.a` opt-in — "Skip
/// unless doing GPU work" — so its absence is the default state of a fresh
/// checkout, not a defect. Before this arm existed the resulting LINK error
/// fell through `is_no_adapter` into `Broken`, which panics unconditionally,
/// and every session working an unrelated bug got six failures out of
/// `cargo test --features llvm`.
///
/// Matched on the linker driver's own wording (`src/codegen/driver.rs`, the
/// `SpecialArchive::Gpu` arm) rather than on "any build failure", for the same
/// reason `is_no_adapter` is written that way: everything else really is a
/// compiler bug wearing a skip's clothing.
fn is_missing_gpu_archive(err: &str) -> bool {
    err.contains("needs the GPU runtime archive") && err.contains("libkarac_runtime_gpu.a")
}

/// Probe once per test binary: build and run the most trivial possible kernel,
/// and settle which adapter the rest of the fixtures use.
///
/// B-2026-08-19-9 — THE BACKEND IS RESOLVED, NOT PINNED. Every fixture used to
/// run under a hardcoded `KARAC_GPU_BACKEND=cpu`, which asks for a
/// `DeviceType::Cpu` adapter. macOS has none (Apple ships no software Metal
/// device), so on the project's own primary dev machine all fourteen execution
/// fixtures took the `NoAdapter` skip while an Apple M5 Pro sat one
/// `request_adapter` call away — the suite reported `ok` in 1.4s having
/// executed nothing. That is the vacuous pass this file's header was written
/// to close, reintroduced by the mechanism meant to make it portable.
///
/// The order is: an explicit `KARAC_GPU_BACKEND` from the caller wins, then the
/// platform's default device, then a forced software adapter. So macOS runs on
/// Metal, a GPU-less Linux box still lands on lavapipe, and pinning the
/// software lane is one env var away.
///
/// THE FALLBACK IS REACHED ONLY FROM `NoAdapter`, which is what keeps `Broken`
/// meaningful. Retrying on any failure would let a host with a working device
/// mask a genuinely broken shader as a skip — the exact hole the four-way split
/// exists to prevent.
fn gpu_probe() -> &'static Probe {
    static PROBE: OnceLock<Probe> = OnceLock::new();
    PROBE.get_or_init(|| {
        let dir = scratch("probe");
        let src = dir.join("probe.kara");
        std::fs::write(
            &src,
            program("#[gpu]\nfn k(x: f32) -> f32 { x }", "f32", "1.0"),
        )
        .expect("write probe source");

        let candidates = backend_candidates(std::env::var("KARAC_GPU_BACKEND").ok());

        let mut last_no_adapter = None;
        for backend in candidates {
            match build_and_run_on_gpu(&dir, &src, "probe", &backend) {
                Ok(_) => {
                    eprintln!("gpu_e2e: running on {}", backend_label(&backend));
                    return Probe::Ready(backend);
                }
                // No point asking a second adapter for an archive that is absent.
                Err(why) if is_missing_gpu_archive(&why) => return Probe::NoArchive(why),
                Err(why) if is_no_adapter(&why) => last_no_adapter = Some(why),
                Err(why) => return Probe::Broken(why),
            }
        }
        Probe::NoAdapter(last_no_adapter.expect("loop only exits here via a no-adapter error"))
    })
}

/// `None` means "skip this test".
///
/// The FOUR-way split is the whole point, and it was not the first design:
/// the original probe returned a plain `Result` and treated every failure as
/// "no adapter". Mutation-testing the emitter to produce invalid WGSL
/// (`arrayLength(input)`, which naga rejects) showed what that costs — the
/// probe program failed too, so all six fixtures soft-skipped and the suite
/// reported GREEN on a compiler that could not produce a single valid shader.
/// That is exactly the vacuous-pass hole this file was written to close,
/// reinvented inside it. `Broken` therefore panics unconditionally, ignoring
/// `KARAC_REQUIRE_GPU_ADAPTER` entirely — the same shape as
/// `common::link_or_skip`, where an undefined-symbol link error panics because
/// it can only mean staleness, while other link errors stay skippable.
///
/// B-2026-08-18-51 ADDED THE FOURTH ARM, and the reason is the other half of
/// that same analogy. `Broken` was catching a case it should never have owned:
/// a host that has simply not built the OPTIONAL `libkarac_runtime_gpu.a`,
/// which CLAUDE.md documents as opt-in and is therefore the default state of a
/// fresh checkout. That is an absent build artifact, not "a compiler or runtime
/// bug on a host that HAS a working adapter", and the panic made
/// `cargo test --features llvm` red for every session working an unrelated bug
/// — six failures whose message was a build recipe. `link_or_skip` already had
/// the right rule for exactly this shape, so `NoArchive` mirrors it: skippable
/// by default, fatal under `KARAC_REQUIRE_RUNTIME_ARCHIVE=1`, which is what
/// CI's archive-building jobs set. `Broken` keeps its unconditional panic for
/// what it was written for — the emitter producing a shader that will not run.
fn gpu_or_skip() -> Option<&'static Backend> {
    match gpu_probe() {
        Probe::Ready(backend) => Some(backend),
        Probe::Broken(why) => panic!(
            "the GPU probe kernel — `fn k(x: f32) -> f32 {{ x }}`, the simplest kernel \
             expressible — failed on a host that HAS a working adapter. That is a \
             compiler or runtime bug, not a missing device, so it is never skippable \
             and no environment variable suppresses it.\n\nprobe failure:\n{why}"
        ),
        Probe::NoArchive(why) => {
            if std::env::var("KARAC_REQUIRE_RUNTIME_ARCHIVE").as_deref() == Ok("1") {
                panic!(
                    "the optional GPU runtime archive is missing and \
                     KARAC_REQUIRE_RUNTIME_ARCHIVE=1 forbids the soft-skip, so this suite \
                     would otherwise report green while executing no shader at all. This \
                     is the same contract `common::link_or_skip` enforces for the other \
                     archives; CI's GPU job sets the variable. Build it (CLAUDE.md \
                     § Commands):\n\
                     \x20 cargo rustc -p karac-runtime --release --features gpu \
                     --crate-type staticlib\n\
                     \x20 cp target/release/libkarac_runtime.a \
                     target/release/libkarac_runtime_gpu.a\n\n\
                     probe failure:\n{why}"
                );
            }
            eprintln!(
                "gpu_e2e: skipping — the optional libkarac_runtime_gpu.a is not built here \
                 (it is opt-in; CLAUDE.md § Commands has the recipe). Set \
                 KARAC_REQUIRE_RUNTIME_ARCHIVE=1 to make this a failure.\nprobe failure:\n{why}"
            );
            None
        }
        Probe::NoAdapter(why) => {
            if std::env::var("KARAC_REQUIRE_GPU_ADAPTER").as_deref() == Ok("1") {
                panic!(
                    "no usable GPU adapter ({}) and KARAC_REQUIRE_GPU_ADAPTER=1 forbids \
                     the soft-skip, so this suite would otherwise report green while \
                     executing no shader at all. On Linux install a software Vulkan \
                     implementation (`sudo apt-get install -y mesa-vulkan-drivers`, which \
                     provides lavapipe) and build the opt-in GPU archive \
                     (CLAUDE.md § Commands):\n\
                     \x20 cargo rustc -p karac-runtime --release --features gpu \
                     --crate-type staticlib\n\
                     \x20 cp target/release/libkarac_runtime.a \
                     target/release/libkarac_runtime_gpu.a\n\n\
                     probe failure:\n{why}",
                    no_adapter_reason(std::env::var("KARAC_GPU_BACKEND").ok())
                );
            }
            eprintln!(
                "gpu_e2e: skipping — {}. Set KARAC_REQUIRE_GPU_ADAPTER=1 to make this a \
                 failure.\nprobe failure:\n{why}",
                no_adapter_reason(std::env::var("KARAC_GPU_BACKEND").ok())
            );
            None
        }
    }
}

/// The whole contract in one place: interpreter == GPU == `expected`.
fn assert_gpu_matches_interp(
    tag: &str,
    kernel: &str,
    elem_ty: &str,
    literals: &str,
    expected: &str,
) {
    let Some(backend) = gpu_or_skip() else { return };

    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(&src, program(kernel, elem_ty, literals)).expect("write fixture source");

    let interp = run_interp(&src);
    let gpu =
        build_and_run_on_gpu(&dir, &src, tag, backend).unwrap_or_else(|e| panic!("{tag}: {e}"));

    assert_eq!(
        interp, gpu,
        "{tag}: GPU execution DIVERGED from the interpreter — \
         a `karac run` vs GPU-dispatch divergence is a compiler bug"
    );
    assert_eq!(
        expected, gpu,
        "{tag}: both legs agree but disagree with the expected output, so the \
         kernel's meaning is not what this fixture claims"
    );
}

// ── The four landed kernel-body increments (B-2026-08-18-40) ────────────────

#[test]
fn gpu_executes_let_locals() {
    // Increment 1: `let` bindings before the tail, each seeing the previous.
    assert_gpu_matches_interp(
        "lets",
        "#[gpu]\nfn k(x: f32) -> f32 { let y: f32 = x * 2.0; let z: f32 = y + 1.0; z }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "3\n5\n7\n9",
    );
}

#[test]
fn gpu_executes_while_loop_with_thread_index_shadowing_local() {
    // Increment 2, and the single most valuable fixture here. The kernel's
    // loop counter is named `i`, which collides with the generated wrapper's
    // THREAD INDEX; the emitter renames it (`i` → `i_k`) so the parameter's
    // `input[i]` still means "this thread's element". If that rename ever
    // regresses, the shader still compiles and every thread accumulates the
    // WRONG element — invisible to any assertion on WGSL text, and caught here
    // because 3×x only holds when each thread reads its own value.
    assert_gpu_matches_interp(
        "while_rename",
        "#[gpu]\n\
         fn k(x: f32) -> f32 {\n\
         \x20   let mut acc: f32 = 0.0;\n\
         \x20   let mut i: i32 = 0;\n\
         \x20   while i < 3 { acc = acc + x; i = i.wrapping_add(1); }\n\
         \x20   acc\n\
         }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "3\n6\n9\n12",
    );
}

#[test]
fn gpu_executes_compound_assignment() {
    // Increment 2's compound-assignment spellings (`+=`, `-=`, …).
    assert_gpu_matches_interp(
        "compound",
        "#[gpu]\n\
         fn k(x: f32) -> f32 {\n\
         \x20   let mut acc: f32 = 0.0;\n\
         \x20   let mut n: i32 = 0;\n\
         \x20   while n < 4 { acc += x; n += 1; }\n\
         \x20   acc\n\
         }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "4\n8\n12\n16",
    );
}

#[test]
fn gpu_executes_for_range_exclusive_and_inclusive() {
    // Increment 3. Two sequential loops both binding `n` also pin the scope
    // stack's truncation at loop exit: without it the second loop would emit a
    // rename-suffixed variable, and an inclusive `..=` bound that silently
    // lowered as exclusive would show up as 4 rather than 5 in the first row.
    assert_gpu_matches_interp(
        "for_range",
        "#[gpu]\n\
         fn k(x: f32) -> f32 {\n\
         \x20   let mut acc: f32 = 0.0;\n\
         \x20   for n in 0..3 { acc = acc + x; }\n\
         \x20   for n in 0..=1 { acc = acc + 1.0; }\n\
         \x20   acc\n\
         }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "5\n8\n11\n14",
    );
}

#[test]
fn gpu_executes_match_as_select_chain() {
    // Increment 4. The chain is built from the last arm outward, so an
    // ordering slip yields a shader that compiles and returns the wrong arm's
    // value. Inputs hit each arm shape exactly once: literal, `|` alternation
    // (twice), and the `_` fallback.
    assert_gpu_matches_interp(
        "match_sel",
        "#[gpu]\nfn k(x: i32) -> i32 { match x { 0 => 40, 1 | 2 => 20, _ => 10 } }",
        "i32",
        "0, 1, 2, 3",
        "40\n20\n20\n10",
    );
}

#[test]
fn gpu_executes_value_if_as_select() {
    // The pre-existing value-`if` lowering, unchanged by the four increments
    // but never executed until now. Both branches are evaluated (`select` does
    // not short-circuit), which is sound because the `#[gpu]` effect gate
    // proves the kernel free of allocation, host I/O and explicit panics.
    assert_gpu_matches_interp(
        "value_if",
        "#[gpu]\nfn k(x: f32) -> f32 { if x > 2.0 { x * 10.0 } else { x } }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "1\n2\n30\n40",
    );
}

// ── Statement-form `if` (B-2026-08-18-49) ───────────────────────────────────

#[test]
fn gpu_executes_statement_if_inside_a_loop() {
    // The conditional accumulator — the shape B-2026-08-18-49 was filed for.
    // Note neither branch declares a local, which is why "locals inside an
    // `if` branch" understated the gap: this was rejected outright.
    //
    // The expected values are what make this a real oracle rather than a
    // smoke test: 1.0 and 2.0 take the `else` arm three times (0-1-1-1 = -3),
    // while 3.0 and 4.0 take the `then` arm three times (3x). A lowering that
    // inverted the condition would still run, still print four numbers, and be
    // caught here.
    assert_gpu_matches_interp(
        "cond_acc",
        "#[gpu]\n\
         fn k(x: f32) -> f32 {\n\
         \x20   let mut acc: f32 = 0.0;\n\
         \x20   let mut i: i32 = 0;\n\
         \x20   while i < 3 {\n\
         \x20       if x > 2.0 { acc = acc + x; } else { acc = acc - 1.0; }\n\
         \x20       i = i.wrapping_add(1);\n\
         \x20   }\n\
         \x20   acc\n\
         }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "-3\n-3\n9\n12",
    );
}

#[test]
fn gpu_executes_else_if_chain() {
    // One input per arm, with distinct outputs, so a chain that emitted its
    // arms in the wrong order or collapsed one into another cannot pass.
    assert_gpu_matches_interp(
        "elseif",
        "#[gpu]\n\
         fn k(x: i32) -> i32 {\n\
         \x20   let mut r: i32 = 0;\n\
         \x20   if x == 0 { r = 100; } else if x == 1 { r = 200; } \
         else if x == 2 { r = 300; } else { r = 400; }\n\
         \x20   r\n\
         }",
        "i32",
        "0, 1, 2, 3",
        "100\n200\n300\n400",
    );
}

#[test]
fn gpu_executes_bare_statement_if_without_else() {
    // A value-`if` must have an `else`; a statement one need not. The inputs
    // straddle the threshold so both the taken and untaken paths are observed.
    assert_gpu_matches_interp(
        "bare_if",
        "#[gpu]\n\
         fn k(x: f32) -> f32 {\n\
         \x20   let mut acc: f32 = x;\n\
         \x20   if x > 2.0 { acc = acc * 10.0; }\n\
         \x20   acc\n\
         }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "1\n2\n30\n40",
    );
}

// ── Value-`if` carrying locals, desugared onto the statement form (step 2) ──

#[test]
fn gpu_executes_let_bound_if_with_a_local_in_a_branch() {
    // The remainder B-2026-08-18-49 named: a value-`if` whose branch declares a
    // local. `select` cannot express it (an operand is one expression), so it
    // desugars to a hoisted `var` plus the statement `if` from step 1.
    //
    // The local is USED in the branch value (`t + 1.0`, not `t`), so a desugar
    // that dropped the binding or assigned the wrong expression diverges here
    // rather than coincidentally agreeing.
    assert_gpu_matches_interp(
        "let_if",
        "#[gpu]\n\
         fn k(x: f32) -> f32 {\n\
         \x20   let y: f32 = if x > 2.0 { let t: f32 = x * 2.0; t + 1.0 } else { x };\n\
         \x20   y\n\
         }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "1\n2\n7\n9",
    );
}

#[test]
fn gpu_executes_assigned_if_with_locals_in_both_branches() {
    // The assignment form needs no annotation — the destination already exists.
    // Both branches declare a local, and they REUSE the same source name in the
    // `let` form below; here they differ so the two paths are distinguishable.
    assert_gpu_matches_interp(
        "assign_if",
        "#[gpu]\n\
         fn k(x: f32) -> f32 {\n\
         \x20   let mut acc: f32 = 0.0;\n\
         \x20   acc = if x > 2.0 { let t: f32 = x * 3.0; t } else { let u: f32 = x + 1.0; u };\n\
         \x20   acc\n\
         }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "2\n3\n9\n12",
    );
}

#[test]
fn gpu_executes_else_if_chain_carrying_locals() {
    // Three arms, each with its own local, and inputs landing in each — so an
    // arm whose hoisted assignment went to the wrong slot shows up as a wrong
    // number rather than as a shader that merely still compiles.
    assert_gpu_matches_interp(
        "chain_locals",
        "#[gpu]\n\
         fn k(x: f32) -> f32 {\n\
         \x20   let y: f32 = if x > 3.0 { let a: f32 = x * 2.0; a } \
         else if x > 1.0 { let b: f32 = x + 5.0; b } else { x };\n\
         \x20   y\n\
         }",
        "f32",
        "1.0, 2.0, 3.0, 4.0",
        "1\n7\n8\n8",
    );
}

/// B-2026-08-18-51 — the probe's classifier, pinned directly.
///
/// The four-way split is only worth anything if `NoArchive` catches EXACTLY the
/// absent optional archive and nothing else. Widening it by one careless
/// `contains` would hand `Broken`'s cases a skip, and `Broken` is what stops
/// this suite reporting green on a compiler that cannot emit a valid shader —
/// the hole the file's own doc comment records being dug and refilled once
/// already.
///
/// The positive case is the linker driver's real wording (`src/codegen/
/// driver.rs`, `SpecialArchive::Gpu`), not a paraphrase. The negatives are the
/// three shapes that must keep their existing routing: a no-adapter host
/// (`NoAdapter`, skippable on its own terms), a shader the backend rejected,
/// and a runtime fault — the last two being `Broken`, which never skips.
///
/// Kept as a pure-string test so it runs everywhere, GPU or not. It is the only
/// part of this file that asserts anything on a host with no adapter, which is
/// precisely the host where the classifier decides everything.
#[test]
fn probe_classifier_matches_only_the_absent_gpu_archive() {
    let real_driver_message = "this program calls `gpu.dispatch`, which needs the GPU runtime \
         archive `libkarac_runtime_gpu.a` — not found. Build it with `cargo rustc -p \
         karac-runtime --release --features gpu --crate-type staticlib`";
    assert!(
        is_missing_gpu_archive(real_driver_message),
        "the linker driver's own archive-missing wording must classify as NoArchive; \
         if this fails the message was reworded and the matcher needs updating in step"
    );

    for (label, err) in [
        (
            "no adapter",
            "gpu.dispatch failed: found no available GPU adapter (no adapters found)",
        ),
        (
            "backend rejected the shader",
            "gpu.dispatch failed: shader validation error: arrayLength(input) \
             expects a pointer to a runtime-sized array",
        ),
        (
            "runtime fault",
            "GPU binary exited Some(134): thread panicked at runtime/src/gpu.rs",
        ),
        (
            "a DIFFERENT optional archive",
            "this program uses `Regex.compile` / `is_match`, which needs the regex runtime \
             archive `libkarac_runtime_regex.a` — not found.",
        ),
    ] {
        assert!(
            !is_missing_gpu_archive(err),
            "{label}: must NOT classify as a missing GPU archive — routing it to the \
             skippable arm is how this suite would go green on a broken emitter"
        );
    }

    // And the two predicates are disjoint: nothing may satisfy both, or the
    // arm order in `gpu_probe` would silently decide the outcome.
    assert!(!is_no_adapter(real_driver_message));
}

/// B-2026-08-19-9 — the backend order, pinned.
///
/// This suite spent its whole existence hardcoding `KARAC_GPU_BACKEND=cpu`,
/// which asks for a `DeviceType::Cpu` adapter. macOS has none, so on the
/// project's primary dev machine every execution fixture took the `NoAdapter`
/// skip while an Apple M5 Pro sat one `request_adapter` away, and the suite
/// reported `ok` having run no shader at all. Re-pinning it is a one-line
/// change that looks harmless and silently un-runs fourteen tests on a whole
/// platform, so the order is asserted rather than left to review.
///
/// Kept pure, like the classifier test above, so it runs on GPU-less hosts —
/// where it is the only thing standing between this file and a vacuous green.
#[test]
fn backend_order_prefers_the_hosts_own_device() {
    assert_eq!(
        backend_candidates(None),
        vec![None, Some("cpu".to_string())],
        "with nothing forced, the host's OWN device must be tried FIRST: preferring the \
         software adapter is what made this suite skip everything on macOS, and preferring \
         it again would do so silently"
    );

    // Forcing a backend must not fall back — a fallback would report a pass for
    // a lane that never ran, which is the same vacuity in the other direction.
    assert_eq!(
        backend_candidates(Some("cpu".to_string())),
        vec![Some("cpu".to_string())],
        "an explicit KARAC_GPU_BACKEND must be honored verbatim with no fallback"
    );

    // The reason text follows what was actually TRIED. Both arguments are
    // passed explicitly rather than read from the environment: this test must
    // give the same answer however the suite was invoked, and reading the
    // ambient value is what made an earlier draft fail under
    // `KARAC_GPU_BACKEND=cpu`.
    let unforced = no_adapter_reason(None);
    assert!(
        unforced.contains("neither a default adapter nor a software one"),
        "unforced: {unforced}"
    );
    // A forced-lane skip on a machine that HAS a GPU must not read as "this
    // host has no adapter" — that sends someone after a driver problem they do
    // not have.
    let forced = no_adapter_reason(Some("cpu".to_string()));
    assert!(
        forced.contains("KARAC_GPU_BACKEND=cpu is set") && forced.contains("Unset it"),
        "forced: {forced}"
    );
}

// ── Wrapping integer arithmetic on the device (B-2026-08-19-1) ──────────────

#[test]
fn gpu_executes_wrapping_add_at_i32_boundary() {
    // The whole point of the bug, executed: this exact program used to be
    // written `x + 1`, which trapped under --interp and silently produced
    // -2147483648 on the device. Bare `+` is now rejected; the named wrapping
    // form is accepted AND agrees with the interpreter, so run == build holds
    // at the boundary rather than only away from it.
    //
    // The first input is i32::MAX, so the fixture fails if either side stops
    // wrapping — this is not a test that passes on well-behaved values.
    assert_gpu_matches_interp(
        "wrap_add",
        "#[gpu]\nfn k(x: i32) -> i32 { x.wrapping_add(1) }",
        "i32",
        "2147483647, 1",
        "-2147483648\n2",
    );
}

#[test]
fn gpu_executes_wrapping_mul_overflowing_i32() {
    assert_gpu_matches_interp(
        "wrap_mul",
        "#[gpu]\nfn k(x: i32) -> i32 { x.wrapping_mul(x) }",
        "i32",
        "100000, 3",
        "1410065408\n9",
    );
}

// ── Whole-buffer reductions (B-2026-08-19-10, slice 1) ─────────────────────

/// Reduction fixtures need their own harness: `gpu.sum(buf)` takes no kernel,
/// and its result is a SCALAR rather than a buffer to iterate.
fn assert_gpu_reduce_matches_interp(tag: &str, body: &str, expected: &str) {
    let Some(backend) = gpu_or_skip() else { return };

    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(&src, body).expect("write fixture source");

    let interp = run_interp(&src);
    let gpu =
        build_and_run_on_gpu(&dir, &src, tag, backend).unwrap_or_else(|e| panic!("{tag}: {e}"));

    assert_eq!(
        interp, gpu,
        "{tag}: GPU reduction DIVERGED from the interpreter — the tree order is \
         specified precisely so these agree bit-for-bit"
    );
    assert_eq!(
        expected, gpu,
        "{tag}: both legs agree but not with the fixture"
    );

    // THE THIRD SURFACE. kara-katas/CLAUDE.md's A/B rule is `run` == `build`,
    // and `run` means the DEFAULT lane, which is the JIT — not `--interp`.
    // Checking only `--interp` against `build` leaves the lane most users
    // actually take untested, and that is exactly where the reductions were
    // broken: unrouted, they reached a JIT with no `karac_runtime_gpu_*`
    // symbols and failed outright while both other surfaces agreed.
    assert_eq!(
        run_default(&src),
        gpu,
        "{tag}: `karac run` (JIT lane) diverged from `karac build`"
    );
}

#[test]
fn gpu_sum_agrees_with_the_interpreter_bit_for_bit() {
    // THE fixture for the tree-order decision. 64 copies of 0.1 sum to
    // 6.400000 under the GPU's tree and 6.399996 under a left fold, so this
    // passes only if the interpreter reproduces the SHADER's order rather than
    // the obvious one. An epsilon-tolerant oracle would have accepted both and
    // proved nothing.
    assert_gpu_reduce_matches_interp(
        "reduce_sum_order",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..64 { v.push(0.1) }\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "6.400000095367432",
    );
}

#[test]
fn gpu_sum_and_prod_agree_on_small_buffers() {
    assert_gpu_reduce_matches_interp(
        "reduce_sum_small",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [1.0, 2.0, 3.0, 4.0];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "10",
    );
    assert_gpu_reduce_matches_interp(
        "reduce_prod_small",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [2.0, 3.0, 4.0];\n\
        \x20   println(f\"{gpu.prod(v)}\")\n\
        }\n",
        "24",
    );
}

#[test]
fn gpu_sum_agrees_bit_for_bit_past_one_workgroup() {
    // A buffer longer than one workgroup is a TREE OF TREES: the first
    // dispatch leaves one partial per workgroup and the host re-dispatches the
    // same shader over those. 4096 copies of 0.1 is two full levels — 64
    // chunks of 64, then one chunk of 64 — and the grouping is observable in
    // f32, so the interpreter has to reproduce the CHUNKING, not merely "a
    // tree". 6.4 (one chunk) times 64 is exact, which is why the answer lands
    // on 409.6000061035156 rather than drifting.
    assert_gpu_reduce_matches_interp(
        "reduce_sum_multi",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..4096 { v.push(0.1) }\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "409.6000061035156",
    );

    // 65 is the first length that needs a second chunk at all, and the one a
    // truncating implementation would answer 64.0 for.
    assert_gpu_reduce_matches_interp(
        "reduce_sum_spill",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..65 { v.push(1.0) }\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "65",
    );
}

#[test]
fn gpu_integer_reductions_agree_with_the_interpreter() {
    assert_gpu_reduce_matches_interp(
        "reduce_int_sum",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [3, 1, 2];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "6",
    );

    // Multi-workgroup, with a partial trailing chunk so the integer padding is
    // exercised on a real device.
    assert_gpu_reduce_matches_interp(
        "reduce_int_sum_multi",
        "fn main() {\n\
        \x20   let mut v: Vec[i32] = [];\n\
        \x20   for i in 0..4096 { v.push(((i % 11) - 5) as i32) }\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "-14",
    );

    // `min`/`max` are `Option[i32]`: fallibility is a property of the OP, not
    // of the element type.
    assert_gpu_reduce_matches_interp(
        "reduce_int_max",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [-5, 40, 2];\n\
        \x20   let m = gpu.max(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "40",
    );
    // ALL-NEGATIVE max: the case a wrong identity would fail. Padding with 0
    // instead of i32::MIN would answer 0 here — a plausible number that is not
    // in the buffer at all.
    assert_gpu_reduce_matches_interp(
        "reduce_int_max_all_negative",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [-5, -40, -2];\n\
        \x20   let m = gpu.max(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "-2",
    );
    assert_gpu_reduce_matches_interp(
        "reduce_int_min_empty",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [];\n\
        \x20   let m = gpu.min(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "empty",
    );

    // The order-dependent case from design.md: this buffer overflows under a
    // left fold and SURVIVES under the specified tree, which pairs each MAX
    // with a -MAX before they ever meet each other. Checked on the device, so
    // the shader's grouping is what is being verified, not just the twin's.
    assert_gpu_reduce_matches_interp(
        "reduce_int_order_survives",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [2147483647, 2147483647, -2147483647, -2147483647];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "0",
    );
}

#[test]
fn gpu_arg_reductions_agree_with_the_interpreter() {
    // The Arg family is the one reduction whose tree carries (value, index)
    // PAIRS, and whose fold level re-reads values from the ORIGINAL buffer
    // through the surviving candidate indices. Both properties are only
    // really exercised on a device.
    assert_gpu_reduce_matches_interp(
        "reduce_argmin_tie",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [3.0, 1.0, 1.0, 5.0];\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "1",
    );

    assert_gpu_reduce_matches_interp(
        "reduce_argmax_small",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [3.0, 5.0, 5.0];\n\
        \x20   let m = gpu.argmax(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "1",
    );

    // 65 elements: the winner sits alone in a chunk that is 63/64 PADDING. If
    // padding could win, this comes back as the index sentinel rather than 64.
    assert_gpu_reduce_matches_interp(
        "reduce_argmin_padding",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..65 { v.push(5.0) }\n\
        \x20   v[64] = -3.0;\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "64",
    );

    // Two full fold levels — the level-1 shader runs for real.
    assert_gpu_reduce_matches_interp(
        "reduce_argmin_multi",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..4096 { v.push(50.0) }\n\
        \x20   v[3000] = -1.0;\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "3000",
    );

    assert_gpu_reduce_matches_interp(
        "reduce_argmin_empty",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "empty",
    );

    // NaN always loses, from either side — the rule that makes the combine
    // associative and so lets the answer be grouping-independent.
    for (tag, buf, want) in [
        ("reduce_argmin_nan_first", "[nan, 3.0, 1.0]", "2"),
        ("reduce_argmin_nan_last", "[3.0, 1.0, nan]", "1"),
    ] {
        assert_gpu_reduce_matches_interp(
            tag,
            &format!(
                "fn main() {{\n\
                \x20   let zero: f32 = 0.0;\n\
                \x20   let nan: f32 = zero / zero;\n\
                \x20   let v: Vec[f32] = {buf};\n\
                \x20   let m = gpu.argmin(v);\n\
                \x20   match m {{\n\
                \x20       Some(x) => println(f\"{{x}}\"),\n\
                \x20       None => println(\"empty\"),\n\
                \x20   }}\n\
                }}\n"
            ),
            want,
        );
    }
}

#[test]
fn gpu_matmul_agrees_with_tensor_matmul_bit_for_bit() {
    // THE fixture for the tiling decision, and the one result in this family
    // that is an EQUALITY rather than a specified difference. Every other op
    // here had to pick a grouping the CPU does not use — `gpu.sum` is a tree
    // where `v.sum()` is a line. A tiled matmul walks tiles in ascending `k`
    // and the inner loop in ascending order within a tile, so the products
    // accumulate in the naive order and `gpu.matmul(a, b)` IS `a.matmul(b)`.
    //
    // Asserted as `==` between the two, not against printed values: printing
    // would compare to four decimals and pass on a near-miss, which is exactly
    // what an accumulation-order bug looks like.
    assert_gpu_reduce_matches_interp(
        "matmul_equals_cpu",
        "fn main() {\n\
        \x20   let a: Tensor[f32, [?, ?]] = Tensor.from([[1.5, -2.25, 3.125], [0.5, 4.0, -1.75]]);\n\
        \x20   let b: Tensor[f32, [?, ?]] = Tensor.from([[2.0, -0.5], [1.25, 3.0], [-4.5, 0.75]]);\n\
        \x20   let g = gpu.matmul(a, b);\n\
        \x20   let c = a.matmul(b);\n\
        \x20   let mut same = true;\n\
        \x20   for i in 0..2 {\n\
        \x20       for j in 0..2 {\n\
        \x20           if g[i, j] != c[i, j] { same = false; }\n\
        \x20       }\n\
        \x20   }\n\
        \x20   println(f\"{same} {g[0, 0]} {g[1, 1]}\");\n\
        }\n",
        "true -13.875 10.4375",
    );
}

#[test]
fn gpu_matmul_crosses_every_tile_edge() {
    // The tile is 16x16, so a matmul whose every dimension is a multiple of 16
    // exercises NO padding at all — and padding is where a tiled kernel goes
    // wrong. Each shape here is ragged in a different place:
    //
    //   17x1x1    m past the tile edge, k and n minimal
    //   1x17x1    the CONTRACTION past the edge — the padded-tile case, and
    //             the only one where a one-sided pad would show up
    //   1x1x17    n past the edge
    //   17x17x17  all three ragged at once
    //
    // Values are deliberately non-uniform: an all-ones matrix makes every
    // grouping and every padding mistake agree, which is the fixture that
    // proves nothing.
    for (m, k, n) in [(17, 1, 1), (1, 17, 1), (1, 1, 17), (17, 17, 17)] {
        let a: String = (0..m)
            .map(|i| {
                let row: Vec<String> = (0..k).map(|p| format!("{}.25", (i * k + p) % 7)).collect();
                format!("[{}]", row.join(", "))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let b: String = (0..k)
            .map(|p| {
                let row: Vec<String> = (0..n).map(|j| format!("-{}.5", (p * n + j) % 5)).collect();
                format!("[{}]", row.join(", "))
            })
            .collect::<Vec<_>>()
            .join(", ");
        assert_gpu_reduce_matches_interp(
            &format!("matmul_tile_{m}x{k}x{n}"),
            &format!(
                "fn main() {{\n\
                \x20   let a: Tensor[f32, [?, ?]] = Tensor.from([{a}]);\n\
                \x20   let b: Tensor[f32, [?, ?]] = Tensor.from([{b}]);\n\
                \x20   let g = gpu.matmul(a, b);\n\
                \x20   let c = a.matmul(b);\n\
                \x20   let mut same = true;\n\
                \x20   for i in 0..{m} {{\n\
                \x20       for j in 0..{n} {{\n\
                \x20           if g[i, j] != c[i, j] {{ same = false; }}\n\
                \x20       }}\n\
                \x20   }}\n\
                \x20   println(f\"{{same}} {{g.shape()[0]}} {{g.shape()[1]}}\");\n\
                }}\n"
            ),
            &format!("true {m} {n}"),
        );
    }
}

#[test]
fn gpu_matmul_needs_more_than_one_workgroup_per_side() {
    // 40x40x40 puts THREE tiles on every axis, so the kernel runs a 3x3 grid
    // of workgroups each looping over 3 contraction tiles — the first shape
    // where the second `workgroupBarrier()` matters. Without it a fast lane
    // stages tile t+1 over values a slow lane is still reading from tile t;
    // the corruption is a race, so it needs real contention to appear and a
    // single-tile fixture cannot produce it.
    let n = 40;
    let a: String = (0..n)
        .map(|i| {
            let row: Vec<String> = (0..n).map(|p| format!("{}.5", (i + p) % 9)).collect();
            format!("[{}]", row.join(", "))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let b: String = (0..n)
        .map(|p| {
            let row: Vec<String> = (0..n).map(|j| format!("-{}.25", (p * 3 + j) % 6)).collect();
            format!("[{}]", row.join(", "))
        })
        .collect::<Vec<_>>()
        .join(", ");
    assert_gpu_reduce_matches_interp(
        "matmul_multi_workgroup",
        &format!(
            "fn main() {{\n\
            \x20   let a: Tensor[f32, [?, ?]] = Tensor.from([{a}]);\n\
            \x20   let b: Tensor[f32, [?, ?]] = Tensor.from([{b}]);\n\
            \x20   let g = gpu.matmul(a, b);\n\
            \x20   let c = a.matmul(b);\n\
            \x20   let mut diffs = 0;\n\
            \x20   for i in 0..{n} {{\n\
            \x20       for j in 0..{n} {{\n\
            \x20           if g[i, j] != c[i, j] {{ diffs = diffs + 1; }}\n\
            \x20       }}\n\
            \x20   }}\n\
            \x20   println(f\"{{diffs}}\");\n\
            }}\n"
        ),
        "0",
    );
}

#[test]
fn gpu_matmul_is_not_gpu_dot_of_the_row_and_column() {
    // The consequence of the equality above, and the reason it is worth
    // stating: `gpu.matmul` accumulates in the NAIVE order, while `gpu.dot`
    // reduces with the halving TREE. Both compute the same mathematical dot
    // product of row 0 with column 0, by different groupings, and f32 addition
    // is not associative — so the two GPU ops legitimately disagree.
    //
    // The fixture prints which one it is rather than asserting inequality
    // directly, so if the two orders ever converge this fails saying so
    // instead of looking like a rounding regression.
    //
    // The values are the prefix-sum row's discriminating quartet: two 1.0s and
    // two values far below the f32 ulp at 1.0. The tree pairs each 1.0 with a
    // tiny value and loses both; the naive order adds the two tiny values to
    // the running total after it has already reached 2.0, losing them as well
    // — but the INTERMEDIATE roundings differ, which is what separates them.
    assert_gpu_reduce_matches_interp(
        "matmul_vs_dot_grouping",
        "fn main() {\n\
        \x20   let row: Vec[f32] = [1.0, 0.00000006, 1.0, 0.00000006];\n\
        \x20   let ones: Vec[f32] = [1.0, 1.0, 1.0, 1.0];\n\
        \x20   let a: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 0.00000006, 1.0, 0.00000006]]);\n\
        \x20   let b: Tensor[f32, [?, ?]] = Tensor.from([[1.0], [1.0], [1.0], [1.0]]);\n\
        \x20   let mm = gpu.matmul(a, b)[0, 0];\n\
        \x20   let dt = gpu.dot(row, ones);\n\
        \x20   if mm == dt {\n\
        \x20       println(\"agree\");\n\
        \x20   } else {\n\
        \x20       println(\"differ\");\n\
        \x20   }\n\
        }\n",
        "differ",
    );
}

#[test]
fn gpu_matmul_empty_contraction_is_a_block_of_zeros() {
    // `k == 0` is NOT the empty case: [m, 0] x [0, n] is an [m, n] block of
    // zeros, because the empty sum is the additive identity. Distinct from
    // every reduction here, where an empty input has no answer and returns
    // `None` — a matmul over an empty contraction has a perfectly good answer.
    assert_gpu_reduce_matches_interp(
        "matmul_empty_contraction",
        "fn main() {\n\
        \x20   let a: Tensor[f32, [?, ?]] = Tensor.zeros([2, 0]);\n\
        \x20   let b: Tensor[f32, [?, ?]] = Tensor.zeros([0, 3]);\n\
        \x20   let g = gpu.matmul(a, b);\n\
        \x20   println(f\"{g.shape()[0]} {g.shape()[1]} {g[0, 0]} {g[1, 2]}\");\n\
        }\n",
        "2 3 0 0",
    );
}

#[test]
fn gpu_prefix_sum_agrees_with_the_interpreter() {
    // The first GPU op whose result is a BUFFER, and the first that is not a
    // fold. Inclusive: out[i] is the sum THROUGH i.
    assert_gpu_reduce_matches_interp(
        "prefix_sum_small",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [1.0, 2.0, 3.0, 4.0];\n\
        \x20   println(f\"{gpu.prefix_sum(v)}\");\n\
        }\n",
        "[1, 3, 6, 10]",
    );

    // Empty in, empty out — and no `Option` anywhere. Every fold in this
    // family returns `Option` because an empty buffer has no sum/extremum to
    // report; a prefix sum has no such hole, so `Vec[f32]` is the honest type
    // and there is no `None` case to write.
    assert_gpu_reduce_matches_interp(
        "prefix_sum_empty",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   let p = gpu.prefix_sum(v);\n\
        \x20   println(f\"{p.len()}\");\n\
        }\n",
        "0",
    );

    // Negatives need no separate path, and the running total is allowed to
    // go back down — a scan is not a monotone sequence.
    assert_gpu_reduce_matches_interp(
        "prefix_sum_negatives",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [5.0, -5.0, 5.0, -5.0];\n\
        \x20   println(f\"{gpu.prefix_sum(v)}\");\n\
        }\n",
        "[5, 0, 5, 0]",
    );
}

#[test]
fn gpu_prefix_sum_carries_the_offset_across_chunk_boundaries() {
    // 4097 elements of 1.0 is past 64*64, so the per-chunk totals THEMSELVES
    // need more than one workgroup and the host recursion runs twice. A
    // single-level implementation is correct up to 4096 and wrong after —
    // passing every short fixture, which is the failure shape this family
    // keeps having to design against.
    //
    // All-ones makes every element its index plus one, so a missing or
    // misindexed chunk offset shows up as a RESET at a 64-boundary rather
    // than as a slightly wrong float. The probes straddle the first boundary
    // and both ends.
    assert_gpu_reduce_matches_interp(
        "prefix_sum_recursive",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..4097 { v.push(1.0) }\n\
        \x20   let p = gpu.prefix_sum(v);\n\
        \x20   println(f\"{p.len()} {p[0]} {p[63]} {p[64]} {p[4095]} {p[4096]}\");\n\
        }\n",
        "4097 1 64 65 4096 4097",
    );
}

#[test]
fn gpu_prefix_sums_last_element_need_not_equal_gpu_sum() {
    // SPECIFIED, not desirable — pinned end to end because it is the one
    // observable consequence of a prefix sum not being a fold. Both numbers
    // are "the total"; they group differently, and f32 addition is not
    // associative. The halving tree computes (a+c) + (b+d); Hillis-Steele's
    // last lane computes (a+b) + (c+d).
    //
    // The buffer is the smallest legible discriminator: 1.0, 1.0, 2^-24,
    // 2^-23. The tree pairs each 1.0 with a tiny value and loses both, giving
    // exactly 2; the scan adds the two tiny values to each other first, and
    // their sum survives.
    //
    // If this ever prints "same", the two summation orders have converged and
    // the docs on `tree_prefix_sum_f32` need re-deriving before the assertion
    // is relaxed.
    assert_gpu_reduce_matches_interp(
        "prefix_sum_vs_sum",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [1.0, 1.0, 0.00000005960464477539063, 0.0000001192092895507813];\n\
        \x20   let p = gpu.prefix_sum(v);\n\
        \x20   let total = gpu.sum(v);\n\
        \x20   if p[3] == total { println(\"same\") } else { println(\"differ\") }\n\
        }\n",
        "differ",
    );
}

#[test]
fn gpu_integer_prefix_sum_scans_across_all_three_surfaces() {
    assert_gpu_reduce_matches_interp(
        "int_prefix_sum",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [1 as i32, 2 as i32, 3 as i32, 4 as i32, 5 as i32];\n\
        \x20   let p = gpu.prefix_sum(v);\n\
        \x20   println(f\"{p[0]} {p[1]} {p[2]} {p[3]} {p[4]}\");\n\
        \x20   let n: Vec[i32] = [5 as i32, -3 as i32, 2 as i32];\n\
        \x20   let q = gpu.prefix_sum(n);\n\
        \x20   println(f\"{q[0]} {q[1]} {q[2]}\");\n\
        \x20   let u: Vec[u32] = [10u32, 20u32, 30u32];\n\
        \x20   let r = gpu.prefix_sum(u);\n\
        \x20   println(f\"{r[0]} {r[1]} {r[2]}\");\n\
        \x20   let e: Vec[i32] = [];\n\
        \x20   println(f\"{gpu.prefix_sum(e).len()}\");\n\
        }\n",
        "1 3 6 10 15\n5 2 4\n10 30 60\n0",
    );

    // 4097 is the load-bearing length: past 64*64 the chunk totals THEMSELVES
    // need more than one workgroup, so a host loop written for a single level
    // is correct up to 4096 and wrong after — passing every short fixture.
    assert_gpu_reduce_matches_interp(
        "int_prefix_sum_two_levels",
        "fn main() {\n\
        \x20   let mut v: Vec[i32] = [];\n\
        \x20   for i in 0..4097 { v.push(1 as i32) }\n\
        \x20   let p = gpu.prefix_sum(v);\n\
        \x20   println(f\"{p[0]} {p[63]} {p[64]} {p[4095]} {p[4096]}\");\n\
        }\n",
        "1 64 65 4096 4097",
    );
}

#[test]
fn gpu_integer_prefix_sum_traps_in_every_phase() {
    let Some(backend) = gpu_or_skip() else { return };

    // PHASE 1 — the running total leaves i32 inside a single chunk's scan.
    let dir = scratch("int_scan_phase1_overflow");
    let src = dir.join("int_scan_phase1_overflow.kara");
    std::fs::write(
        &src,
        "fn main() {\n\
        \x20   let v: Vec[i32] = [2147483647 as i32, 1 as i32, 1 as i32];\n\
        \x20   println(f\"{gpu.prefix_sum(v)[2]}\");\n\
        }\n",
    )
    .expect("write fixture source");
    let err = build_and_run_on_gpu(&dir, &src, "int_scan_phase1_overflow", backend)
        .expect_err("a scan past i32 must trap, not wrap");
    assert!(err.contains("integer overflow"), "phase 1: {err}");

    // PHASE 3 — the OFFSET ADD, which is the step most easily forgotten:
    // phases 1 and 2 look like the arithmetic and this one looks like
    // bookkeeping, but `scanned[i] + offset` adds two real values. Chunk 0
    // sums to i32::MAX and chunk 1's single element pushes it over, so
    // NEITHER per-chunk scan overflows on its own.
    let dir = scratch("int_scan_phase3_overflow");
    let src = dir.join("int_scan_phase3_overflow.kara");
    std::fs::write(
        &src,
        "fn main() {\n\
        \x20   let mut v: Vec[i32] = [];\n\
        \x20   v.push(2147483647 as i32);\n\
        \x20   for i in 0..63 { v.push(0 as i32) }\n\
        \x20   v.push(1 as i32);\n\
        \x20   println(f\"{gpu.prefix_sum(v)[64]}\");\n\
        }\n",
    )
    .expect("write fixture source");
    let err = build_and_run_on_gpu(&dir, &src, "int_scan_phase3_overflow", backend)
        .expect_err("an overflowing offset add must trap");
    assert!(err.contains("integer overflow"), "phase 3: {err}");
}

#[test]
fn gpu_integer_prefix_sum_traps_where_a_running_total_would_not() {
    // THE SPECIFIED ORDER DECIDES WHETHER IT TRAPS, not merely what it
    // returns — the scan's version of the fact design.md records for the
    // integer reductions, and the reason `gpu.prefix_sum` is not
    // interchangeable with a running total on integer data.
    //
    // Running totals here are -MAX, 0, MAX — all comfortably in range. The
    // first Hillis-Steele step computes prev[2] + prev[1] = MAX + MAX.
    //
    // All three surfaces trap, because the interpreter reproduces the device's
    // step order; that is what makes this specified behaviour rather than a
    // divergence.
    let Some(backend) = gpu_or_skip() else { return };
    let dir = scratch("int_scan_window_overflow");
    let src = dir.join("int_scan_window_overflow.kara");
    std::fs::write(
        &src,
        "fn main() {\n\
        \x20   let v: Vec[i32] = [-2147483647 as i32, 2147483647 as i32, 2147483647 as i32];\n\
        \x20   let p = gpu.prefix_sum(v);\n\
        \x20   println(f\"{p[0]} {p[1]} {p[2]}\");\n\
        }\n",
    )
    .expect("write fixture source");
    let err = build_and_run_on_gpu(&dir, &src, "int_scan_window_overflow", backend)
        .expect_err("the Hillis-Steele window sum must trap here");
    assert!(
        err.contains("integer overflow"),
        "the scan order's own trap: {err}"
    );
}

#[test]
fn gpu_integer_prod_traps_where_the_cpu_product_does() {
    // `v.product()` over a `Vec[i32]` already traps on overflow, so `gpu.prod`
    // inherits the contract rather than choosing one. The device does it with
    // a CHECKED multiply built on the emitted widening product — the primitive
    // whose absence was recorded as blocking `prod` for several batches.
    assert_gpu_reduce_matches_interp(
        "int_prod",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [2 as i32, 3 as i32, 4 as i32, 5 as i32];\n\
        \x20   println(f\"{gpu.prod(v)}\");\n\
        \x20   let n: Vec[i32] = [-2 as i32, 3 as i32, -4 as i32];\n\
        \x20   println(f\"{gpu.prod(n)}\");\n\
        \x20   let u: Vec[u32] = [2u32, 3u32, 7u32];\n\
        \x20   println(f\"{gpu.prod(u)}\");\n\
        }\n",
        "120\n24\n42",
    );

    // THE EMPTY PRODUCT IS 1, not 0. It is the one input no shader ever sees,
    // so the identity is supplied by the host — and a 0 here would be a wrong
    // answer that only an empty buffer reveals.
    assert_gpu_reduce_matches_interp(
        "int_prod_empty",
        "fn main() {\n\
        \x20   let e: Vec[i32] = [];\n\
        \x20   println(f\"{gpu.prod(e)}\");\n\
        }\n",
        "1",
    );

    // i32::MIN is reachable as a PRODUCT even though its magnitude is not
    // reachable as a positive one — the range is asymmetric, and the checked
    // multiply tests against the bound for the RESULT's sign.
    assert_gpu_reduce_matches_interp(
        "int_prod_asymmetric_edge",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [(-2147483647 - 1) as i32, 1 as i32];\n\
        \x20   println(f\"{gpu.prod(v)}\");\n\
        }\n",
        "-2147483648",
    );
}

#[test]
fn gpu_integer_dot_is_sum_of_products_including_the_traps() {
    // The defining identity, asserted IN the language rather than in a
    // comment: `gpu.dot(a, b) == gpu.sum(a * b)`. Over integers this now
    // extends to which programs trap, because both sides use the same checked
    // combine.
    assert_gpu_reduce_matches_interp(
        "int_dot_identity",
        "fn main() {\n\
        \x20   let a: Vec[i32] = [7 as i32, 11 as i32, 13 as i32];\n\
        \x20   let b: Vec[i32] = [3 as i32, 5 as i32, 2 as i32];\n\
        \x20   let mut p: Vec[i32] = [];\n\
        \x20   for i in 0..3 { p.push(a[i] * b[i]) }\n\
        \x20   println(f\"{gpu.dot(a, b) == gpu.sum(p)}\");\n\
        }\n",
        "true",
    );

    assert_gpu_reduce_matches_interp(
        "int_dot_values",
        "fn main() {\n\
        \x20   let a: Vec[i32] = [1 as i32, 2 as i32, 3 as i32, 4 as i32];\n\
        \x20   let b: Vec[i32] = [5 as i32, 6 as i32, 7 as i32, 8 as i32];\n\
        \x20   println(f\"{gpu.dot(a, b)}\");\n\
        \x20   let n: Vec[i32] = [-1 as i32, 2 as i32];\n\
        \x20   let m: Vec[i32] = [3 as i32, -4 as i32];\n\
        \x20   println(f\"{gpu.dot(n, m)}\");\n\
        \x20   let u: Vec[u32] = [2u32, 3u32];\n\
        \x20   let w: Vec[u32] = [10u32, 100u32];\n\
        \x20   println(f\"{gpu.dot(u, w)}\");\n\
        \x20   let e: Vec[i32] = [];\n\
        \x20   let f: Vec[i32] = [];\n\
        \x20   println(f\"{gpu.dot(e, f)}\");\n\
        }\n",
        "70\n-11\n320\n0",
    );
}

#[test]
fn gpu_integer_dot_traps_when_a_single_product_overflows() {
    // The PRODUCT can overflow with nothing accumulated yet, so checking only
    // the running sum would let a wrapped term through. `65536 * 65536` leaves
    // i32 in one term while the rest of the buffer is zeros.
    let Some(backend) = gpu_or_skip() else { return };
    let dir = scratch("int_dot_product_overflow");
    let src = dir.join("int_dot_product_overflow.kara");
    std::fs::write(
        &src,
        "fn main() {\n\
        \x20   let a: Vec[i32] = [65536 as i32, 0 as i32];\n\
        \x20   let b: Vec[i32] = [65536 as i32, 0 as i32];\n\
        \x20   println(f\"{gpu.dot(a, b)}\");\n\
        }\n",
    )
    .expect("write fixture source");
    let err = build_and_run_on_gpu(&dir, &src, "int_dot_product_overflow", backend)
        .expect_err("a product past i32 must trap, not wrap");
    assert!(
        err.contains("integer overflow"),
        "expected Kāra's own `integer overflow` panic, got: {err}"
    );
}

#[test]
fn gpu_integer_matmul_equals_the_cpu_matmul() {
    // Small case, checked against `a.matmul(b)` IN the program — the promise
    // is equality with the CPU form, so the test states it that way rather
    // than pinning numbers that could both drift together.
    assert_gpu_reduce_matches_interp(
        "int_matmul_small",
        "fn main() {\n\
        \x20   let a: Tensor[i32, [?, ?]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
        \x20   let b: Tensor[i32, [?, ?]] = Tensor.from([[7, 8], [9, 10], [11, 12]]);\n\
        \x20   let g = gpu.matmul(a, b);\n\
        \x20   let c = a.matmul(b);\n\
        \x20   println(f\"{g[0, 0]} {g[0, 1]} {g[1, 0]} {g[1, 1]}\");\n\
        \x20   println(f\"{g[0, 0] == c[0, 0]} {g[1, 1] == c[1, 1]}\");\n\
        \x20   let u: Tensor[u32, [?, ?]] = Tensor.from([[2u32, 3u32]]);\n\
        \x20   let v: Tensor[u32, [?, ?]] = Tensor.from([[10u32], [100u32]]);\n\
        \x20   println(f\"{gpu.matmul(u, v)[0, 0]}\");\n\
        }\n",
        "58 64 139 154\ntrue true\n320",
    );

    // 17 straddles the 16-wide tile in ALL THREE dimensions, so every edge
    // guard and the k-padding are exercised at once. A matmul whose dimensions
    // are all tile multiples exercises no padding at all.
    assert_gpu_reduce_matches_interp(
        "int_matmul_tile_edge",
        "fn main() {\n\
        \x20   let a: Tensor[i32, [?, ?]] = Tensor.from([[-5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1], [2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5], [-2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2], [5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2], [1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5], [-3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1], [4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3], [0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4], [-4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0], [3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4], [-1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3], [-5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1], [2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5], [-2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2], [5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2], [1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5], [-3, 0, 3, -5, -2, 1, 4, -4, -1, 2, 5, -3, 0, 3, -5, -2, 1]]);\n\
        \x20   let b: Tensor[i32, [?, ?]] = Tensor.from([[-6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0], [-1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5], [4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3], [-4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2], [1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6], [6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3, -1], [-2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2, 4], [3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4], [-5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1], [0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6], [5, -6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2], [-3, -1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3], [2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5], [-6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0], [-1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5], [4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2, 4, 6, -5, -3], [-4, -2, 0, 2, 4, 6, -5, -3, -1, 1, 3, 5, -6, -4, -2, 0, 2]]);\n\
        \x20   let g = gpu.matmul(a, b);\n\
        \x20   let c = a.matmul(b);\n\
        \x20   let mut same = true;\n\
        \x20   for i in 0..17 { for j in 0..17 { if g[i, j] != c[i, j] { same = false; } } }\n\
        \x20   println(f\"{same}\");\n\
        }\n",
        "true",
    );
}

#[test]
fn gpu_integer_matmul_traps_where_the_cpu_matmul_does() {
    // THE SHARPER HALF OF THE PROMISE. `gpu.matmul` equals `a.matmul(b)` trap
    // for trap, not merely value for value: the tiled order is the naive one,
    // so the same intermediates are formed, and overflow is a property of the
    // intermediates. B-2026-08-20-27 made the CPU side trap here; this pins
    // that the GPU side agrees.
    let Some(backend) = gpu_or_skip() else { return };
    let dir = scratch("int_matmul_overflow");
    let src = dir.join("int_matmul_overflow.kara");
    std::fs::write(
        &src,
        "fn main() {\n\
        \x20   let a: Tensor[i32, [?, ?]] = Tensor.from([[65536, 65536]]);\n\
        \x20   let b: Tensor[i32, [?, ?]] = Tensor.from([[65536], [65536]]);\n\
        \x20   println(f\"{gpu.matmul(a, b)[0, 0]}\");\n\
        }\n",
    )
    .expect("write fixture source");
    let err = build_and_run_on_gpu(&dir, &src, "int_matmul_overflow", backend)
        .expect_err("a contraction past i32 must trap, not wrap");
    assert!(
        err.contains("integer overflow"),
        "expected Kāra's own `integer overflow` panic, got: {err}"
    );
}

#[test]
fn gpu_integer_variance_is_exact_where_an_f32_deviation_would_not_be() {
    // THE fixture for the integer-variance decision. Sixty-four values
    // centred at 2³⁰ with a spread of ±100: forming `(x - mean)` in f32
    // quantises every element (f32 is exact on integers only to 2²⁴), and the
    // naive float path reports a variance several percent wrong. The integer
    // path shifts by an INTEGER `K` and squares into an exact `u64`, so it
    // reports the correctly-rounded value.
    //
    // The expected number is the exact rational variance — `Σ(n·x - Σx)² /
    // n³` evaluated in exact integer arithmetic — so this fixture is an
    // independent oracle rather than a snapshot of what the code does.
    assert_gpu_reduce_matches_interp(
        "int_variance_large",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [1073741724, 1073741731, 1073741738, 1073741745, 1073741752, 1073741759, 1073741766, 1073741773, 1073741780, 1073741787, 1073741794, 1073741801, 1073741808, 1073741815, 1073741822, 1073741829, 1073741836, 1073741843, 1073741850, 1073741857, 1073741864, 1073741871, 1073741878, 1073741885, 1073741892, 1073741899, 1073741906, 1073741913, 1073741920, 1073741726, 1073741733, 1073741740, 1073741747, 1073741754, 1073741761, 1073741768, 1073741775, 1073741782, 1073741789, 1073741796, 1073741803, 1073741810, 1073741817, 1073741824, 1073741831, 1073741838, 1073741845, 1073741852, 1073741859, 1073741866, 1073741873, 1073741880, 1073741887, 1073741894, 1073741901, 1073741908, 1073741915, 1073741922, 1073741728, 1073741735, 1073741742, 1073741749, 1073741756, 1073741763];\n\
        \x20   match gpu.variance(v) {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "3633.038818359375",
    );

    // Translation invariance, end to end: the SAME spread at the origin must
    // give the SAME variance. If the shift ever stops being exact this pair
    // separates, which no single-magnitude fixture would reveal.
    assert_gpu_reduce_matches_interp(
        "int_variance_at_origin",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [-100, -93, -86, -79, -72, -65, -58, -51, -44, -37, -30, -23, -16, -9, -2, 5, 12, 19, 26, 33, 40, 47, 54, 61, 68, 75, 82, 89, 96, -98, -91, -84, -77, -70, -63, -56, -49, -42, -35, -28, -21, -14, -7, 0, 7, 14, 21, 28, 35, 42, 49, 56, 63, 70, 77, 84, 91, 98, -96, -89, -82, -75, -68, -61];\n\
        \x20   match gpu.variance(v) {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "3633.038818359375",
    );
}

#[test]
fn gpu_integer_variance_handles_u32_past_the_signed_range() {
    // A `u32` buffer centred at 3e9 — past `i32::MAX`, so `K = round(mean)`
    // does not fit a signed 32-bit shift. The shift travels as two words for
    // exactly this case; a 32-bit `K` would wrap here and the deviations would
    // be measured from the wrong place.
    assert_gpu_reduce_matches_interp(
        "u32_variance_past_i32",
        "fn main() {\n\
        \x20   let v: Vec[u32] = [2999999900, 2999999907, 2999999914, 2999999921, 2999999928, 2999999935, 2999999942, 2999999949, 2999999956, 2999999963, 2999999970, 2999999977, 2999999984, 2999999991, 2999999998, 3000000005, 3000000012, 3000000019, 3000000026, 3000000033, 3000000040, 3000000047, 3000000054, 3000000061, 3000000068, 3000000075, 3000000082, 3000000089, 3000000096, 2999999902, 2999999909, 2999999916, 2999999923, 2999999930, 2999999937, 2999999944, 2999999951, 2999999958, 2999999965, 2999999972, 2999999979, 2999999986, 2999999993, 3000000000, 3000000007, 3000000014, 3000000021, 3000000028, 3000000035, 3000000042, 3000000049, 3000000056, 3000000063, 3000000070, 3000000077, 3000000084, 3000000091, 3000000098, 2999999904, 2999999911, 2999999918, 2999999925, 2999999932, 2999999939];\n\
        \x20   match gpu.variance(v) {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "3633.038818359375",
    );
}

#[test]
fn gpu_integer_stddev_is_the_root_of_the_integer_variance() {
    // `gpu.stddev(v)` and `gpu.variance(v).sqrt()` must be the same number —
    // one computation with one extra operation on the way out, not two
    // computations that happen to agree.
    assert_gpu_reduce_matches_interp(
        "int_stddev_root",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [1073741724, 1073741731, 1073741738, 1073741745, 1073741752, 1073741759, 1073741766, 1073741773, 1073741780, 1073741787, 1073741794, 1073741801, 1073741808, 1073741815, 1073741822, 1073741829, 1073741836, 1073741843, 1073741850, 1073741857, 1073741864, 1073741871, 1073741878, 1073741885, 1073741892, 1073741899, 1073741906, 1073741913, 1073741920, 1073741726, 1073741733, 1073741740, 1073741747, 1073741754, 1073741761, 1073741768, 1073741775, 1073741782, 1073741789, 1073741796, 1073741803, 1073741810, 1073741817, 1073741824, 1073741831, 1073741838, 1073741845, 1073741852, 1073741859, 1073741866, 1073741873, 1073741880, 1073741887, 1073741894, 1073741901, 1073741908, 1073741915, 1073741922, 1073741728, 1073741735, 1073741742, 1073741749, 1073741756, 1073741763];\n\
        \x20   let w: Vec[i32] = [1073741724, 1073741731, 1073741738, 1073741745, 1073741752, 1073741759, 1073741766, 1073741773, 1073741780, 1073741787, 1073741794, 1073741801, 1073741808, 1073741815, 1073741822, 1073741829, 1073741836, 1073741843, 1073741850, 1073741857, 1073741864, 1073741871, 1073741878, 1073741885, 1073741892, 1073741899, 1073741906, 1073741913, 1073741920, 1073741726, 1073741733, 1073741740, 1073741747, 1073741754, 1073741761, 1073741768, 1073741775, 1073741782, 1073741789, 1073741796, 1073741803, 1073741810, 1073741817, 1073741824, 1073741831, 1073741838, 1073741845, 1073741852, 1073741859, 1073741866, 1073741873, 1073741880, 1073741887, 1073741894, 1073741901, 1073741908, 1073741915, 1073741922, 1073741728, 1073741735, 1073741742, 1073741749, 1073741756, 1073741763];\n\
        \x20   match gpu.stddev(v) {\n\
        \x20       Some(s) => match gpu.variance(w) {\n\
        \x20           Some(x) => println(f\"{s == x.sqrt()}\"),\n\
        \x20           None => println(\"empty\"),\n\
        \x20       },\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "true",
    );
}

#[test]
fn gpu_integer_variance_traps_when_the_squared_deviations_overflow() {
    // `Σd²` is a `u64`, so a spread that genuinely does not fit TRAPS rather
    // than saturating — the same contract an overflowing integer `gpu.sum`
    // makes. Alternating i32::MIN / i32::MAX puts each d² near 2⁶², so the
    // accumulator passes 2⁶⁴ within one workgroup.
    //
    // Checked on the GPU leg only: `assert_gpu_reduce_matches_interp` compares
    // stdout, and a trapping program's output is on stderr with a non-zero
    // exit. The interpreter parity for the trap is
    // `reduce_kernel::tests::integer_variance_traps_when_the_squared_deviations_overflow`.
    let Some(backend) = gpu_or_skip() else { return };
    let dir = scratch("int_variance_overflow");
    let src = dir.join("int_variance_overflow.kara");
    let vals: Vec<String> = (0..64)
        .map(|i| {
            // `as i32` is required: a bare `2147483647` literal infers as
            // i64, and the narrowing coercion is an error by design.
            if i % 2 == 0 {
                "(-2147483647 - 1) as i32".to_string()
            } else {
                "2147483647 as i32".to_string()
            }
        })
        .collect();
    std::fs::write(
        &src,
        format!(
            "fn main() {{\n\
            \x20   let v: Vec[i32] = [{}];\n\
            \x20   match gpu.variance(v) {{\n\
            \x20       Some(x) => println(f\"{{x}}\"),\n\
            \x20       None => println(\"empty\"),\n\
            \x20   }}\n\
            }}\n",
            vals.join(", ")
        ),
    )
    .expect("write fixture source");

    let err = build_and_run_on_gpu(&dir, &src, "int_variance_overflow", backend)
        .expect_err("a Σd² past u64 must trap, not return a number");
    assert!(
        err.contains("integer overflow"),
        "expected Kāra's own `integer overflow` panic, got: {err}"
    );
}

#[test]
fn gpu_variance_and_stddev_agree_with_the_interpreter() {
    // The first TWO-PASS reduction, and the first shader with a UNIFORM. Both
    // only really run on a device: the mean is produced by a complete sum
    // reduction, read back to the host, and handed to a second dispatch that
    // squares each deviation on load.
    //
    // Textbook population example: [2,4,4,4,5,5,7,9] has mean 5, variance 4,
    // standard deviation 2. Matching `Stats.variance` / `Stats.stddev`, which
    // are population too.
    assert_gpu_reduce_matches_interp(
        "reduce_variance",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];\n\
        \x20   let a = gpu.variance(v);\n\
        \x20   match a {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "4",
    );
    assert_gpu_reduce_matches_interp(
        "reduce_stddev",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];\n\
        \x20   let a = gpu.stddev(v);\n\
        \x20   match a {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "2",
    );

    // A CONSTANT buffer is the fixture that proves the uniform actually
    // carries the mean: every deviation is zero. A uniform that arrived as 0.0
    // — an unbound or misbound binding — would give the sum of squares
    // instead, which is loudly nonzero here.
    assert_gpu_reduce_matches_interp(
        "reduce_variance_constant",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..200 { v.push(7.0) }\n\
        \x20   let a = gpu.variance(v);\n\
        \x20   match a {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "0",
    );

    // Two full fold levels in BOTH passes.
    assert_gpu_reduce_matches_interp(
        "reduce_stddev_multi",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..4096 { v.push((i % 13) as f32) }\n\
        \x20   let a = gpu.stddev(v);\n\
        \x20   match a {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "3.742374897003174",
    );

    // Small-n edges: one element has zero population variance, none has no
    // variance at all.
    assert_gpu_reduce_matches_interp(
        "reduce_variance_single",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [3.0];\n\
        \x20   let a = gpu.variance(v);\n\
        \x20   match a {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "0",
    );
    assert_gpu_reduce_matches_interp(
        "reduce_variance_empty",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   let a = gpu.variance(v);\n\
        \x20   match a {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "empty",
    );
}

#[test]
fn gpu_integer_arg_reductions_order_by_the_element_type() {
    // The discriminating pair: above 2^31 the signed and unsigned orders
    // disagree at BOTH ends, so a signed compare on unsigned data answers
    // argmax and argmin backwards. On the device the comparison comes from the
    // shader's declared `array<u32>` rather than from any host-side choice.
    assert_gpu_reduce_matches_interp(
        "reduce_arg_u32_max",
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   let m = gpu.argmax(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "0",
    );
    assert_gpu_reduce_matches_interp(
        "reduce_arg_u32_min",
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "1",
    );

    // Signed negatives order below zero; ties still take the first.
    assert_gpu_reduce_matches_interp(
        "reduce_arg_i32_tie",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [5, -7, -7];\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "1",
    );

    // Two full fold levels, so the integer fold shader runs for real.
    assert_gpu_reduce_matches_interp(
        "reduce_arg_i32_multi",
        "fn main() {\n\
        \x20   let mut v: Vec[i32] = [];\n\
        \x20   for i in 0..4096 { v.push(50) }\n\
        \x20   v[3000] = -1;\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "3000",
    );
}

#[test]
fn gpu_u32_reductions_are_unsigned_end_to_end() {
    // The whole u32 question in one place: above 2^31 the unsigned reading
    // differs from the signed one at EVERY step — the shader's compare, the
    // overflow rule, and the widening of the 32-bit result into Kāra's i64
    // carrier. The last one is the only place signedness reaches codegen (the
    // runtime entry point moves raw 4-byte words and never interprets them),
    // and a sign-extend there turns a large u32 into a negative i64.
    //
    // THIS fixture is the discriminating one: reverting the zero-extend makes
    // it print 18446744071562067968. A `max` fixture alone would NOT catch it
    // — the print path masks an Option payload back to 32 bits and hides the
    // sign extension.
    assert_gpu_reduce_matches_interp(
        "reduce_u32_past_i32",
        "fn main() {\n\
        \x20   let v: Vec[u32] = [2147483647, 1];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "2147483648",
    );

    // The same buffer would have TRAPPED as `Vec[i32]`. Signedness comes from
    // the type, which is why the interpreter reads it from the typechecker's
    // hint rather than sniffing the values — a `Vec[u32]` of small values is
    // indistinguishable from a `Vec[i32]` by inspection, and folding it with
    // the signed rule would trap somewhere past 2^31 that the compiled path
    // sails through. A run/build divergence reachable only on large data.
    assert_gpu_reduce_matches_interp(
        "reduce_u32_max_above_2_31",
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   let m = gpu.max(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => {\n\
        \x20           println(f\"{x}\")\n\
        \x20           let halved: u32 = x / 2;\n\
        \x20           println(f\"{halved}\")\n\
        \x20       },\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "4294967295\n2147483647",
    );

    // Unsigned `min` picks 1, where a signed compare would pick 4294967295
    // (it reads as -1).
    assert_gpu_reduce_matches_interp(
        "reduce_u32_min_above_2_31",
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   let m = gpu.min(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "1",
    );

    // Multi-workgroup, so the unsigned shader is exercised past one dispatch.
    assert_gpu_reduce_matches_interp(
        "reduce_u32_multi",
        "fn main() {\n\
        \x20   let mut v: Vec[u32] = [];\n\
        \x20   for i in 0..4096 { v.push((i % 11) as u32) }\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
        "20466",
    );
}

#[test]
fn gpu_u32_overflow_traps_at_its_own_boundary() {
    // u32 overflows on a CARRY, not a sign flip — the shader's check is
    // `s < a` rather than the signed shared-sign-then-flip test. So it must
    // trap here and NOT at 2^31, which the fixture above proves it sails
    // through.
    let Some(backend) = gpu_or_skip() else { return };
    let tag = "reduce_u32_overflow";
    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(
        &src,
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
    )
    .expect("write fixture source");

    let err = build_and_run_on_gpu(&dir, &src, tag, backend)
        .expect_err("an overflowing u32 reduction must not succeed");
    assert!(
        err.contains("integer overflow"),
        "compiled leg must trap with `integer overflow`, got: {err}"
    );

    for args in [vec!["run"], vec!["run", "--interp"]] {
        let out = karac()
            .args(&args)
            .arg(&src)
            .output()
            .expect("spawn karac run");
        assert!(
            !out.status.success(),
            "`karac {args:?}` must fail on an overflowing u32 reduction"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("integer overflow"),
            "`karac {args:?}` must say `integer overflow`"
        );
    }
}

#[test]
fn gpu_integer_mean_promotes_on_every_surface() {
    // The decision, end to end: an integer mean PROMOTES like `Stats.mean`
    // (1.5, not 1), and promotes to f64 because the integer sum it divides is
    // exact.
    assert_gpu_reduce_matches_interp(
        "reduce_int_mean",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [1, 2];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "1.5",
    );

    // THE discriminating fixture: both elements sit above 2^24, so promoting
    // the ELEMENTS to f32 first — which is what a GPU promote-then-sum would
    // have to do — quantises each one before the sum. Widening the finished
    // integer sum to f64 instead is lossless, so the whole operation rounds
    // exactly once and lands on the true mean.
    assert_gpu_reduce_matches_interp(
        "reduce_int_mean_above_2_24",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [16777217, 16777219];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "16777218",
    );

    // Unsigned, with a sum above 2^31 — so the widen has to be UNSIGNED or
    // the mean comes back negative. Chosen to fit u32: 4294967292 / 2.
    assert_gpu_reduce_matches_interp(
        "reduce_int_mean_unsigned",
        "fn main() {\n\
        \x20   let v: Vec[u32] = [2147483647, 2147483645];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "2147483646",
    );

    assert_gpu_reduce_matches_interp(
        "reduce_int_mean_empty",
        "fn main() {\n\
        \x20   let v: Vec[i32] = [];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "empty",
    );
}

#[test]
fn gpu_integer_overflow_traps_on_every_surface() {
    // The decision itself, end to end. WGSL has no trapping arithmetic — its
    // integer ops are DEFINED to wrap — so the shader computes an overflow
    // flag and folds it through the tree, the runtime reports it, and codegen
    // raises Kāra's own panic at the call site. All three surfaces must refuse
    // this program; a wrapping GPU sum would silently answer i32::MIN where
    // `v.sum()` already fails.
    let Some(backend) = gpu_or_skip() else { return };
    let tag = "reduce_int_overflow";
    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(
        &src,
        "fn main() {\n\
        \x20   let v: Vec[i32] = [2147483647, 1];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }\n",
    )
    .expect("write fixture source");

    // The COMPILED leg: the trap is Kāra's own panic, raised at the call site
    // with a span — not the runtime's bare abort. Same shape `v.sum()` gives
    // for the identical condition.
    let err = build_and_run_on_gpu(&dir, &src, tag, backend)
        .expect_err("an overflowing integer reduction must not succeed");
    assert!(
        err.contains("integer overflow"),
        "compiled leg must trap with `integer overflow`, got: {err}"
    );

    // Both interpreter lanes: `karac run` (routed) and explicit `--interp`.
    for args in [vec!["run"], vec!["run", "--interp"]] {
        let out = karac()
            .args(&args)
            .arg(&src)
            .output()
            .expect("spawn karac run");
        assert!(
            !out.status.success(),
            "`karac {args:?}` must fail on an overflowing integer reduction"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("integer overflow"),
            "`karac {args:?}` must say `integer overflow`, got: {stderr}"
        );
    }
}

#[test]
fn gpu_mean_equals_the_sum_over_the_count_on_the_device() {
    // Mean has no shader of its own — it runs the SUM kernel unchanged and the
    // host divides once, after the fold converges. (A shader cannot know it is
    // running the last level, so a division inside it would divide once per
    // level.) 4096 tenths is two full fold levels and a value where the tree
    // order is observable, so a mean that divided per level, or folded some
    // other way, would land somewhere else.
    assert_gpu_reduce_matches_interp(
        "reduce_mean_vs_sum",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   let mut p: Vec[f32] = [];\n\
        \x20   for i in 0..4096 { v.push(0.1) p.push(0.1) }\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        \x20   println(f\"{gpu.sum(p) / 4096.0}\")\n\
        }\n",
        "0.10000000149011612\n0.10000000149011612",
    );

    assert_gpu_reduce_matches_interp(
        "reduce_mean_small",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [1.0, 2.0, 3.0];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "2",
    );

    // Empty never reaches a device: decided entirely by codegen's branch, and
    // `0.0 / 0` is NaN if that branch is wrong.
    assert_gpu_reduce_matches_interp(
        "reduce_mean_empty",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "empty",
    );
}

#[test]
fn gpu_dot_equals_the_sum_of_the_products_on_the_device() {
    // The guarantee the two-shader design exists to hold. `dot`'s level-0
    // shader forms the product on load and then runs the SAME halving tree the
    // sum shader runs, and every level after it IS the sum shader — so the two
    // are the same number to the last bit rather than merely close. 4096
    // tenths is two full fold levels and a value where the order is
    // observable, so a `dot` that folded its partials some other way would
    // land somewhere else.
    assert_gpu_reduce_matches_interp(
        "reduce_dot_vs_sum",
        "fn main() {\n\
        \x20   let mut a: Vec[f32] = [];\n\
        \x20   let mut b: Vec[f32] = [];\n\
        \x20   let mut p: Vec[f32] = [];\n\
        \x20   for i in 0..4096 {\n\
        \x20       let x: f32 = 0.1;\n\
        \x20       let y: f32 = 1.0;\n\
        \x20       a.push(x)\n\
        \x20       b.push(y)\n\
        \x20       p.push(x * y)\n\
        \x20   }\n\
        \x20   println(f\"{gpu.dot(a, b)}\")\n\
        \x20   println(f\"{gpu.sum(p)}\")\n\
        }\n",
        "409.6000061035156\n409.6000061035156",
    );

    assert_gpu_reduce_matches_interp(
        "reduce_dot_small",
        "fn main() {\n\
        \x20   let a: Vec[f32] = [1.0, 2.0, 3.0];\n\
        \x20   let b: Vec[f32] = [4.0, 5.0, 6.0];\n\
        \x20   println(f\"{gpu.dot(a, b)}\")\n\
        }\n",
        "32",
    );

    // 65 is the first length that spills into a second chunk, so the level-0
    // shader's own padding is exercised before any fold runs.
    assert_gpu_reduce_matches_interp(
        "reduce_dot_spill",
        "fn main() {\n\
        \x20   let mut a: Vec[f32] = [];\n\
        \x20   let mut b: Vec[f32] = [];\n\
        \x20   for i in 0..65 { a.push(2.0) b.push(3.0) }\n\
        \x20   println(f\"{gpu.dot(a, b)}\")\n\
        }\n",
        "390",
    );

    // Empty needs no device: the additive identity, like an empty sum.
    assert_gpu_reduce_matches_interp(
        "reduce_dot_empty",
        "fn main() {\n\
        \x20   let a: Vec[f32] = [];\n\
        \x20   let b: Vec[f32] = [];\n\
        \x20   println(f\"{gpu.dot(a, b)}\")\n\
        }\n",
        "0",
    );
}

#[test]
fn gpu_min_max_agree_with_the_interpreter_including_nan() {
    // NaN is the reason min/max needed a hand-written combine rather than
    // WGSL's `min` builtin: the builtin returns `e1` unless `e2 < e1`, and
    // every comparison against NaN is false, so its answer depends on which
    // side the NaN is on. In a halving tree that side is decided by the
    // grouping. Both orders must give 1 — on the device, not just in the twin.
    for (tag, buf) in [
        ("reduce_min_nan_first", "[nan, 1.0, 2.0]"),
        ("reduce_min_nan_last", "[2.0, 1.0, nan]"),
    ] {
        assert_gpu_reduce_matches_interp(
            tag,
            &format!(
                "fn main() {{\n\
                \x20   let zero: f32 = 0.0;\n\
                \x20   let nan: f32 = zero / zero;\n\
                \x20   let v: Vec[f32] = {buf};\n\
                \x20   let m = gpu.min(v);\n\
                \x20   match m {{\n\
                \x20       Some(x) => println(f\"{{x}}\"),\n\
                \x20       None => println(\"empty\"),\n\
                \x20   }}\n\
                }}\n"
            ),
            "1",
        );
    }

    // Multi-workgroup, with a partial trailing chunk so the +inf padding is
    // exercised on a real device: 200 elements is 3 full chunks plus 8.
    assert_gpu_reduce_matches_interp(
        "reduce_min_multi",
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..200 { v.push(500.0 - (i as f32)) }\n\
        \x20   let m = gpu.min(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "301",
    );

    assert_gpu_reduce_matches_interp(
        "reduce_max_small",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [3.0, 1.5, 2.0];\n\
        \x20   let m = gpu.max(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "3",
    );
}

#[test]
fn gpu_min_of_an_empty_buffer_is_none_on_both_legs() {
    // The empty case never reaches a device, so it is decided entirely by
    // codegen's branch — and getting it wrong would hand back the shader's
    // +inf padding identity, a plausible wrong answer rather than an obvious
    // one. `Stats.min` and `Vec.min` both say `None` here.
    assert_gpu_reduce_matches_interp(
        "reduce_min_empty",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   let m = gpu.min(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }\n",
        "empty",
    );
}

#[test]
fn gpu_prod_of_an_empty_buffer_is_one_on_both_legs() {
    // The one input that never reaches a device: the runtime short-circuits
    // before touching an adapter, so it has to be TOLD the identity. A
    // hardcoded 0.0 there would print 0 under `karac build` and 1 under
    // `karac run` — a run/build divergence on the cheapest possible program,
    // and one no GPU-having host would catch any sooner than a GPU-less one.
    assert_gpu_reduce_matches_interp(
        "reduce_prod_empty",
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   println(f\"{gpu.prod(v)}\")\n\
        }\n",
        "1",
    );
}

/// A reduction over more elements than ONE dispatch grid row can address must
/// still see every element (B-2026-08-21-13).
///
/// `run_compute` caps a dispatch's X extent at 65535 workgroups and spreads
/// the remainder across grid ROWS. Every reduce/scan shader indexed `gid.x`
/// alone and wrote its partial to `output[wid.x]`, so above
/// `65535 * 64 = 4_194_240` elements row 0 was the only row read, and the
/// rows that did run overwrote each other's partials. Measured before the
/// fix, on this exact fixture: `5000006 / 7 / 4999999` came back as
/// `4194240 / 1 / 0`.
///
/// **Build-only, deliberately.** The sibling reduction tests compare all
/// three surfaces, but a dispatch grid exists only on the device path — the
/// interpreter has no geometry to get wrong — and a tree-walk over five
/// million elements costs minutes for no added coverage. What replaces the
/// interpreter here is an oracle that needs no twin: every value is a small
/// integer and every partial sum stays under 2^24, so the sum is exact in
/// f32 REGARDLESS of tree order. This test therefore checks element
/// COVERAGE, not float association, which is the property actually at risk.
///
/// Three assertions that fail differently, so the failure names its own
/// cause: the sum catches a dropped COUNT, while max and argmax catch
/// dropped DATA — a maximum planted in the very last element is invisible to
/// any shader that stops at row 0.
#[test]
fn a_reduction_past_one_dispatch_row_still_sees_every_element() {
    let Some(backend) = gpu_or_skip() else { return };

    // The boundary this fixture must clear, spelled out so it cannot rot
    // below the threshold and start passing for the wrong reason.
    const ROW_SPAN: usize = 65535 * 64;
    const N: usize = 5_000_000;
    // A COMPILE-TIME guard, not a runtime one: shrinking `N` below a single
    // row would leave this test passing while testing nothing at all, and
    // that should break the build rather than quietly go green.
    const _: () = assert!(N > ROW_SPAN);

    let tag = "reduce_two_dispatch_rows";
    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(
        &src,
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = Vec.new();\n\
        \x20   let mut i: i64 = 0;\n\
        \x20   while i < 5000000 {\n\
        \x20       v.push(1.0);\n\
        \x20       i = i + 1;\n\
        \x20   }\n\
        \x20   v[4999999] = 7.0;\n\
        \x20   println(f\"{gpu.sum(v)}\");\n\
        \x20   let m = gpu.max(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"none\"),\n\
        \x20   }\n\
        \x20   let a = gpu.argmax(v);\n\
        \x20   match a {\n\
        \x20       Some(k) => println(f\"{k}\"),\n\
        \x20       None => println(\"none\"),\n\
        \x20   }\n\
        }\n",
    )
    .expect("write fixture source");

    let out =
        build_and_run_on_gpu(&dir, &src, tag, backend).unwrap_or_else(|e| panic!("{tag}: {e}"));
    let got: Vec<&str> = out.lines().map(str::trim).collect();

    assert_eq!(
        got,
        vec!["5000006", "7", "4999999"],
        "{tag}: a reduction lost the elements past dispatch row 0 \
         (4_999_999 ones plus a 7 in the LAST slot). Pre-fix this read \
         `4194240 / 1 / 0` — one row's worth, with the tail unseen."
    );
}

/// A resident FIELD reduction agrees with the round-trip reduction of the same
/// numbers BIT-FOR-BIT, not within an epsilon (GPU-SLIP-4b-3).
///
/// This is the oracle the whole slice rests on. `gpu.sum(buf.m)` reads a buffer
/// already on the device; `gpu.sum(host)` uploads the identical values from a
/// host `Vec[f32]`. The two must be the same number — which holds only because
/// the strided level-0 kernel walks the same padded-halving tree at the same
/// width as the contiguous one, and every level above it IS the contiguous
/// kernel, shared verbatim.
///
/// **The comparison happens inside the program, not over printed text.**
/// `println` of an `f32` rounds for display, so two values that differ in the
/// last bit can print identically; `want == got` is a real float comparison and
/// is what the `MATCH` line reports. The printed values are there to make a
/// failure readable, not to be the assertion.
///
/// 5000 elements is chosen to force a THREE-level fold (5000 → 79 → 2 → 1), so
/// the partial-folding path is exercised rather than a single workgroup, and
/// `i * 0.1` gives values whose f32 sum genuinely depends on association — a
/// left fold over them lands elsewhere.
#[test]
fn a_resident_field_reduction_matches_the_round_trip_oracle() {
    let Some(backend) = gpu_or_skip() else { return };

    let tag = "resident_field_oracle";
    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(
        &src,
        "struct Cell { m: f32, t: f32 }\n\
        fn main() {\n\
        \x20   let mut cells: Vec[Cell] = Vec.new();\n\
        \x20   let mut host: Vec[f32] = Vec.new();\n\
        \x20   let mut i: i64 = 0;\n\
        \x20   while i < 5000 {\n\
        \x20       let v: f32 = (i as f32) * 0.1;\n\
        \x20       cells.push(Cell { m: v, t: 0.0 });\n\
        \x20       host.push(v);\n\
        \x20       i = i + 1;\n\
        \x20   }\n\
        \x20   let want = gpu.sum(host);\n\
        \x20   let buf = gpu.upload(cells);\n\
        \x20   let got = gpu.sum(buf.m);\n\
        \x20   if want == got { println(\"MATCH\"); } else { println(\"DIVERGE\"); }\n\
        \x20   println(f\"{want}\");\n\
        \x20   println(f\"{got}\");\n\
        }\n",
    )
    .expect("write fixture source");

    let out =
        build_and_run_on_gpu(&dir, &src, tag, backend).unwrap_or_else(|e| panic!("{tag}: {e}"));
    let got: Vec<&str> = out.lines().map(str::trim).collect();
    assert_eq!(
        got.first().copied(),
        Some("MATCH"),
        "{tag}: a resident field reduction diverged from the round-trip reduction of the \
         same values — the two tree orders have drifted apart. Full output:\n{out}"
    );
}

/// A resident field reduction resolves the field's LAYOUT GROUP, stride and
/// offset — not just its position in the struct (GPU-SLIP-4b-3).
///
/// A `layout` block splits `Body` across two device buffers, so the four fields
/// land at four distinct `(group, offset)` pairs under a stride of 2: `x` is
/// (0, 0), `y` is (0, 1), `vx` is (1, 0), `vy` is (1, 1). Every one of the four
/// sums is a different number, and each is wrong in a DIFFERENT way if the
/// resolution slips — reading the wrong group returns another field's total,
/// reading the wrong offset within the right group returns its neighbour's.
/// That is why all four are asserted rather than one: a single-field test
/// passes under a stride bug that happens to alias.
///
/// `min` / `max` / `mean` ride along because they take the `Option` path, which
/// is a different tail in codegen — and `mean`'s host-side divide is the only
/// arithmetic outside a shader in the whole family.
#[test]
fn a_resident_field_reduction_resolves_layout_groups_and_offsets() {
    let Some(backend) = gpu_or_skip() else { return };

    let tag = "resident_field_layout";
    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(
        &src,
        "struct Body { x: f32, y: f32, vx: f32, vy: f32 }\n\
        layout bodies: Vec[Body] {\n\
        \x20   group pos { x, y }\n\
        \x20   group vel { vx, vy }\n\
        }\n\
        fn main() {\n\
        \x20   let mut bodies: Vec[Body] = Vec.new();\n\
        \x20   bodies.push(Body { x: 1.0, y: 2.0, vx: 3.0, vy: 4.0 });\n\
        \x20   bodies.push(Body { x: 10.0, y: 20.0, vx: 30.0, vy: 40.0 });\n\
        \x20   bodies.push(Body { x: 100.0, y: 200.0, vx: 300.0, vy: 400.0 });\n\
        \x20   let buf = gpu.upload(bodies);\n\
        \x20   println(f\"{gpu.sum(buf.x)}\");\n\
        \x20   println(f\"{gpu.sum(buf.y)}\");\n\
        \x20   println(f\"{gpu.sum(buf.vx)}\");\n\
        \x20   println(f\"{gpu.sum(buf.vy)}\");\n\
        \x20   match gpu.max(buf.vy) {\n\
        \x20       Some(m) => println(f\"{m}\"),\n\
        \x20       None => println(\"none\"),\n\
        \x20   }\n\
        \x20   match gpu.min(buf.x) {\n\
        \x20       Some(m) => println(f\"{m}\"),\n\
        \x20       None => println(\"none\"),\n\
        \x20   }\n\
        \x20   match gpu.mean(buf.y) {\n\
        \x20       Some(m) => println(f\"{m}\"),\n\
        \x20       None => println(\"none\"),\n\
        \x20   }\n\
        }\n",
    )
    .expect("write fixture source");

    let out =
        build_and_run_on_gpu(&dir, &src, tag, backend).unwrap_or_else(|e| panic!("{tag}: {e}"));
    let got: Vec<&str> = out.lines().map(str::trim).collect();
    assert_eq!(
        got,
        vec!["111", "222", "333", "444", "400", "1", "74"],
        "{tag}: a field resolved to the wrong layout group or offset. The four sums are \
         deliberately distinct — 111/222/333/444 name (group 0, off 0), (0, 1), (1, 0) and \
         (1, 1) in that order, so a swapped pair identifies which half slipped."
    );
}

/// An EMPTY resident buffer answers with the operation's identity, and `min`
/// answers `None` — no device is touched at all (GPU-SLIP-4b-3).
///
/// The same contract the host-side reduction has, and it matters for the same
/// reason: the empty input is the one case no shader ever sees, so the answer
/// comes from codegen and the runtime agreeing rather than from a device. `min`
/// over nothing must be `None` rather than the `+inf` that pads a short chunk —
/// which is exactly the plausible wrong answer available here.
#[test]
fn a_resident_field_reduction_of_an_empty_buffer_is_the_identity() {
    let Some(backend) = gpu_or_skip() else { return };

    let tag = "resident_field_empty";
    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(
        &src,
        "struct Cell { m: f32, t: f32 }\n\
        fn main() {\n\
        \x20   let empty: Vec[Cell] = Vec.new();\n\
        \x20   let buf = gpu.upload(empty);\n\
        \x20   println(f\"{gpu.sum(buf.m)}\");\n\
        \x20   match gpu.min(buf.m) {\n\
        \x20       Some(m) => println(f\"{m}\"),\n\
        \x20       None => println(\"none\"),\n\
        \x20   }\n\
        }\n",
    )
    .expect("write fixture source");

    let out =
        build_and_run_on_gpu(&dir, &src, tag, backend).unwrap_or_else(|e| panic!("{tag}: {e}"));
    let got: Vec<&str> = out.lines().map(str::trim).collect();
    assert_eq!(
        got,
        vec!["0", "none"],
        "{tag}: an empty device buffer must reduce to the identity, and `min` to `None` — \
         a `+inf` here would be the padding leaking out as an answer."
    );
}

/// A resident field reduction past the 2-D dispatch row boundary still sees
/// every element (GPU-SLIP-4b-3, guarding the B-2026-08-21-13 class).
///
/// The strided level-0 kernel is NEW code that recovers a thread's index
/// itself, so it can reintroduce exactly the defect the twelve contiguous
/// emitters were just fixed for: reading `gid.x` alone silently stops at
/// `65535 * 64 = 4_194_240` elements. A structural gate over the emitter text
/// would not catch a regression here, because this kernel's bound is
/// `arrayLength(&input) / stride` rather than the array length — one more
/// place the record grid and the f32 grid can be confused.
///
/// Build-only for the same reason as its contiguous sibling: a dispatch grid
/// exists only on the device path, and with every value 1.0 the sum is exact in
/// f32 at any tree order, so this measures element COVERAGE rather than
/// association.
#[test]
fn a_resident_field_reduction_past_one_dispatch_row_sees_every_element() {
    let Some(backend) = gpu_or_skip() else { return };

    const ROW_SPAN: usize = 65535 * 64;
    const N: usize = 5_000_000;
    const _: () = assert!(N > ROW_SPAN);

    let tag = "resident_field_two_rows";
    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(
        &src,
        "struct Cell { m: f32, t: f32 }\n\
        fn main() {\n\
        \x20   let mut big: Vec[Cell] = Vec.new();\n\
        \x20   let mut i: i64 = 0;\n\
        \x20   while i < 5000000 {\n\
        \x20       big.push(Cell { m: 1.0, t: 0.0 });\n\
        \x20       i = i + 1;\n\
        \x20   }\n\
        \x20   let buf = gpu.upload(big);\n\
        \x20   println(f\"{gpu.sum(buf.m)}\");\n\
        }\n",
    )
    .expect("write fixture source");

    let out =
        build_and_run_on_gpu(&dir, &src, tag, backend).unwrap_or_else(|e| panic!("{tag}: {e}"));
    assert_eq!(
        out.trim(),
        "5000000",
        "{tag}: the strided field kernel lost the elements past dispatch row 0 — \
         a `4194240` here is the `gid.x`-only index returning."
    );
}
