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
