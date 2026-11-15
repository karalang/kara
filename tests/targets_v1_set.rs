//! design.md § 16's closed target set at the driver: gaps the target
//! probe found (B-2026-11-15-51 .. -55) and a false note after
//! target twins (B-2026-11-15-56).
//!
//! - A `*-linux-musl` build is fully static or refused; it linked against
//!   the host's glibc into a binary that could not run.
//! - A bare-metal triple, and one LLVM does not know, are refused before
//!   any pass runs, with a diagnostic of their own.
//! - `#[cfg(target: ...)]` points at the interim `#[target(...)]`, with a
//!   fix, and its twin item is not a duplicate definition.
//! - `[build].targets`, `check --targets` and `check --target` refuse
//!   `wasm_browser` and `gpu`.
//! - `karac build --help` says `--no-proxy` fetches git and registry
//!   dependencies directly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("karac-v1-set-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("karac")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const HELLO: &str = "fn main() { println(\"hi\"); }\n";

#[test]
fn a_bare_metal_triple_is_refused_up_front() {
    let d = Scratch::new("none");
    std::fs::write(d.0.join("h.kara"), HELLO).unwrap();
    let o = karac(&d.0, &["build", "--target=thumbv7em-none-eabihf", "h.kara"]);
    let e = stderr(&o);
    assert_eq!(o.status.code(), Some(1), "{e}");
    assert!(
        e.contains("`thumbv7em-none-eabihf` is a bare-metal (no_std) target, which is not in v1"),
        "{e}"
    );
    assert!(!e.contains("cargo rustc"), "no archive recipe for it: {e}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_triple_llvm_does_not_know_is_refused_up_front() {
    let d = Scratch::new("bogus");
    std::fs::write(d.0.join("h.kara"), HELLO).unwrap();
    let o = karac(&d.0, &["build", "--target=bogus-triple", "h.kara"]);
    let e = stderr(&o);
    assert_eq!(o.status.code(), Some(1), "{e}");
    assert!(
        e.contains("unknown target `bogus-triple`: not a target name (native, wasm_wasi)"),
        "{e}"
    );
    assert!(!e.contains("error[mir]"), "{e}");
}

/// Whether the ELF file names a program interpreter (`PT_INTERP`).
#[cfg(all(feature = "llvm", target_os = "linux", target_arch = "x86_64"))]
fn has_interpreter(path: &Path) -> bool {
    let b = std::fs::read(path).unwrap();
    assert_eq!(&b[..4], b"\x7fELF");
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let phoff = u64::from_le_bytes(b[32..40].try_into().unwrap()) as usize;
    (0..u16_at(56)).any(|i| {
        let o = phoff + i * u16_at(54);
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) == 3
    })
}

/// A musl build is a fully static binary that runs. Skips without the
/// runtime archive built for musl, which CLAUDE.md's recipe does not make.
#[cfg(all(feature = "llvm", target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_musl_build_is_fully_static_and_runs() {
    let triple = "x86_64-unknown-linux-musl";
    let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(triple)
        .join("release/libkarac_runtime.a");
    if !archive.exists() {
        eprintln!("skipping: {} is missing", archive.display());
        return;
    }
    let d = Scratch::new("musl");
    std::fs::write(d.0.join("h.kara"), HELLO).unwrap();
    let o = karac(
        &d.0,
        &["build", &format!("--target={triple}"), "h.kara", "-o", "h"],
    );
    let e = stderr(&o);
    if e.contains("needs musl's C library") {
        eprintln!("skipping: no musl on this host: {e}");
        return;
    }
    assert!(o.status.success(), "{e}");
    let exe = d.0.join("h");
    assert!(!has_interpreter(&exe), "a musl build must be fully static");
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hi\n");
}

const CFG_TWINS: &str = "#[cfg(target: \"native\")]\n\
fn name() -> String { \"native\".to_string() }\n\
#[cfg(target: \"wasm_wasi\")]\n\
fn name() -> String { \"wasm\".to_string() }\n\
fn main() { println(name()); }\n";

#[test]
fn cfg_points_at_target_and_its_twin_is_no_duplicate() {
    let d = Scratch::new("cfg");
    std::fs::write(d.0.join("c.kara"), CFG_TWINS).unwrap();
    let o = karac(&d.0, &["check", "c.kara"]);
    let e = stderr(&o);
    assert!(!o.status.success(), "{e}");
    assert_eq!(
        e.matches("`#[cfg(...)]` is not implemented yet").count(),
        2,
        "{e}"
    );
    assert!(e.contains("`#[target(native)]`"), "{e}");
    assert!(!e.contains("already defined"), "no E0101 on the twin: {e}");
    assert!(!e.contains("diagnostic::cfg"), "{e}");
    // The fix writes the interim spelling, and the program then checks.
    let f = karac(&d.0, &["fix", "c.kara"]);
    assert!(f.status.success(), "{}", stderr(&f));
    let fixed = std::fs::read_to_string(d.0.join("c.kara")).unwrap();
    assert!(
        fixed.starts_with("#[target(native)]\n")
            && fixed.contains("#[target(wasm_wasi)]\n")
            && !fixed.contains("cfg"),
        "{fixed}"
    );
    let o = karac(&d.0, &["check", "c.kara"]);
    let e = stderr(&o);
    assert!(o.status.success(), "{e}");
    // Each twin is checked by its own target's pass, so neither is
    // reported as checked nowhere (B-2026-11-15-56).
    assert!(!e.contains("NOT checked"), "{e}");
}

#[test]
fn build_targets_refuses_the_later_tracks() {
    for (name, track) in [("gpu", "the GPU track"), ("wasm_browser", "the web track")] {
        let d = Scratch::new(name);
        std::fs::write(
            d.0.join("kara.toml"),
            format!("[package]\nname = \"t\"\n\n[build]\ntargets = [\"native\", \"{name}\"]\n"),
        )
        .unwrap();
        std::fs::create_dir_all(d.0.join("src")).unwrap();
        std::fs::write(d.0.join("src/main.kara"), HELLO).unwrap();
        let o = karac(&d.0, &["check", "src/main.kara"]);
        let e = stderr(&o);
        assert!(!o.status.success(), "{e}");
        assert!(
            e.contains(&format!(
                "target '{name}' is not in v1 ({track}; design.md § 16)"
            )) && e.contains("Valid targets: native, wasm_wasi"),
            "{e}"
        );
    }
}

#[test]
fn check_targets_refuses_the_later_tracks() {
    let d = Scratch::new("cli-targets");
    std::fs::write(d.0.join("h.kara"), HELLO).unwrap();
    for flag in [
        "--targets=native,gpu",
        "--targets=wasm_browser",
        "--target=gpu",
    ] {
        let o = karac(&d.0, &["check", "h.kara", flag]);
        let e = stderr(&o);
        assert_eq!(o.status.code(), Some(1), "{flag}: {e}");
        assert!(
            e.contains("is not in v1 (the") && e.contains("Valid targets: native, wasm_wasi, all."),
            "{flag}: {e}"
        );
    }
}

#[test]
fn build_help_says_no_proxy_fetches_dependencies_directly() {
    let d = Scratch::new("help");
    let o = karac(&d.0, &["build", "--help"]);
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), stderr(&o));
    assert!(
        text.contains("git and\n") && text.contains("fetched straight"),
        "{text}"
    );
    assert!(!text.contains("only prints a"), "{text}");
}
