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
//! # How it runs without a GPU
//!
//! `mesa-vulkan-drivers` provides **lavapipe**, a software Vulkan
//! implementation, so `KARAC_GPU_BACKEND=cpu` runs the real naga + Vulkan
//! pipeline on a GPU-less machine — CI runners included. This is also the
//! first automated coverage of CG-6's `KARAC_GPU_BACKEND=cpu` leg, which was
//! deferred as "Linux-container territory" when the rest of CG-6 landed on
//! Metal.
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

/// `Ok(stdout)`, or `Err(diagnostic)` when the program could not be built or
/// the device refused to run it. The caller decides whether an error is a
/// skip or a failure — only [`gpu_available`] treats it as "no adapter".
fn build_and_run_on_gpu(dir: &Path, src_path: &Path, stem: &str) -> Result<String, String> {
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
    let out = Command::new(&bin)
        .env("KARAC_GPU_BACKEND", "cpu")
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

/// Why the probe kernel could not run — the distinction that decides whether
/// a failure may be skipped.
enum Probe {
    Ready,
    /// The host genuinely has no device. Skippable.
    NoAdapter(String),
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

/// Probe once per test binary: build and run the most trivial possible kernel.
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
        match build_and_run_on_gpu(&dir, &src, "probe") {
            Ok(_) => Probe::Ready,
            Err(why) if is_no_adapter(&why) => Probe::NoAdapter(why),
            Err(why) => Probe::Broken(why),
        }
    })
}

/// `None` means "skip this test".
///
/// The three-way split is the whole point, and it was NOT the first design:
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
fn gpu_or_skip() -> Option<()> {
    match gpu_probe() {
        Probe::Ready => Some(()),
        Probe::Broken(why) => panic!(
            "the GPU probe kernel — `fn k(x: f32) -> f32 {{ x }}`, the simplest kernel \
             expressible — failed on a host that HAS a working adapter. That is a \
             compiler or runtime bug, not a missing device, so it is never skippable \
             and no environment variable suppresses it.\n\nprobe failure:\n{why}"
        ),
        Probe::NoAdapter(why) => {
            if std::env::var("KARAC_REQUIRE_GPU_ADAPTER").as_deref() == Ok("1") {
                panic!(
                    "no usable GPU adapter and KARAC_REQUIRE_GPU_ADAPTER=1 forbids the \
                     soft-skip, so this suite would otherwise report green while executing \
                     no shader at all. On Linux install a software Vulkan implementation \
                     (`sudo apt-get install -y mesa-vulkan-drivers`, which provides \
                     lavapipe) and build the opt-in GPU archive (CLAUDE.md § Commands):\n\
                     \x20 cargo rustc -p karac-runtime --release --features gpu \
                     --crate-type staticlib\n\
                     \x20 cp target/release/libkarac_runtime.a \
                     target/release/libkarac_runtime_gpu.a\n\n\
                     probe failure:\n{why}"
                );
            }
            eprintln!(
                "gpu_e2e: skipping — no usable GPU adapter (install mesa-vulkan-drivers \
                 and build libkarac_runtime_gpu.a; set KARAC_REQUIRE_GPU_ADAPTER=1 to make \
                 this a failure).\nprobe failure:\n{why}"
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
    let Some(()) = gpu_or_skip() else { return };

    let dir = scratch(tag);
    let src = dir.join(format!("{tag}.kara"));
    std::fs::write(&src, program(kernel, elem_ty, literals)).expect("write fixture source");

    let interp = run_interp(&src);
    let gpu = build_and_run_on_gpu(&dir, &src, tag).unwrap_or_else(|e| panic!("{tag}: {e}"));

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
         \x20   while i < 3 { acc = acc + x; i = i + 1; }\n\
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
