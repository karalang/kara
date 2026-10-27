//! `karac build --enable-hot-swap` and a cross `--target` on v2
//! (B-2026-10-27-4): the hot-swap table's IR shape, the flag-off IR
//! without it, a Windows executable built from another host when the
//! MinGW toolchain and the runtime archive for the triple are here, and the
//! recipe a build without that archive names.
//!
//! Without the llvm feature there is no code generator, so each test that
//! needs one returns early.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LLVM: bool = cfg!(feature = "llvm");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-build-target-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(dir: &Path, args: &[&str], ll: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.current_dir(dir).args(args).env_remove("KARAC_RUNTIME");
    if let Some(ll) = ll {
        cmd.env("KARAC_MIR_LL", ll);
    }
    cmd.output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const HOT_SWAP: &str = "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n\n\
pub fn label(n: i64) -> String {\n    f\"n={n}\"\n}\n\n\
pub fn same[T](x: own T) -> T {\n    x\n}\n\n\
fn twice(x: i64) -> i64 {\n    x * 2\n}\n\n\
fn main() {\n    println(f\"{add(2, 3)}\");\n    println(label(twice(4)));\n    println(f\"{same(7)}\");\n}\n";

#[test]
fn build_v2_hot_swap_calls_pub_fns_through_the_table() {
    if !LLVM {
        return;
    }
    let dir = scratch("hot-swap");
    std::fs::write(dir.join("hs.kara"), HOT_SWAP).unwrap();
    let ll = dir.join("on.ll");
    let out = karac(&dir, &["build", "hs.kara", "--enable-hot-swap"], Some(&ll));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let ir = std::fs::read_to_string(&ll).unwrap();
    // One slot per non-generic `pub fn`: `add` and `label`, not the
    // generic `same`, the private `twice` or `main`.
    assert!(
        ir.contains("@karac_hotswap_table = global [2 x ptr] zeroinitializer"),
        "{ir}"
    );
    assert!(ir.contains("define internal void @__karac_init_hot_swap_table()"));
    assert!(ir.contains("@llvm.global_ctors = appending global"));
    assert!(ir.contains("store ptr @kara.add, ptr @karac_hotswap_table"));
    assert!(ir.contains("store ptr @kara.label, ptr getelementptr"));
    assert!(!ir.contains("store ptr @kara.twice"));
    // The calls load their slot and call through it, with the callee's
    // parameter attributes: `label` returns through a `noalias` slot.
    assert!(
        ir.lines()
            .any(|l| l.contains("load ptr, ptr @karac_hotswap_table")),
        "{ir}"
    );
    assert!(
        ir.lines()
            .any(|l| l.contains("call void %") && l.contains("ptr noalias")),
        "{ir}"
    );
    let exe = dir.join(if cfg!(windows) { "hs.exe" } else { "hs" });
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(text(&run.stdout), "5\nn=8\n7\n", "{}", text(&run.stderr));

    // Off, none of it is emitted.
    let off = dir.join("off.ll");
    let out = karac(&dir, &["build", "hs.kara"], Some(&off));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let ir = std::fs::read_to_string(&off).unwrap();
    for name in [
        "karac_hotswap_table",
        "__karac_init_hot_swap_table",
        "llvm.global_ctors",
    ] {
        assert!(!ir.contains(name), "{name} without --enable-hot-swap");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_v2_hot_swap_is_refused_on_a_wasm_triple() {
    let dir = scratch("hot-swap-wasm");
    std::fs::write(dir.join("hs.kara"), HOT_SWAP).unwrap();
    let out = karac(
        &dir,
        &[
            "build",
            "hs.kara",
            "--enable-hot-swap",
            "--target=wasm32-wasip1",
        ],
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("--enable-hot-swap is incompatible with --target=wasm32-wasip1"),
        "{}",
        text(&out.stderr)
    );
}

/// Whether `name` is an executable on PATH.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

#[test]
fn build_v2_cross_target_builds_a_windows_executable() {
    const TRIPLE: &str = "x86_64-pc-windows-gnu";
    let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(TRIPLE)
        .join("release/libkarac_runtime.a");
    if !LLVM || cfg!(windows) || !on_path("x86_64-w64-mingw32-gcc") || !archive.exists() {
        eprintln!(
            "skipped: needs x86_64-w64-mingw32-gcc and {}",
            archive.display()
        );
        return;
    }
    let dir = scratch("cross-windows");
    std::fs::write(
        dir.join("hello.kara"),
        "fn main() {\n    println(\"hi\");\n}\n",
    )
    .unwrap();
    let out = karac(
        &dir,
        &["build", "hello.kara", &format!("--target={TRIPLE}")],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("Built: hello.exe"),
        "{}",
        text(&out.stdout)
    );
    let exe = std::fs::read(dir.join("hello.exe")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    // A PE image: the DOS header's `MZ`, and at its `e_lfanew` the
    // `PE\0\0` signature with the x86-64 machine type.
    assert_eq!(&exe[..2], b"MZ");
    let pe = u32::from_le_bytes(exe[0x3c..0x40].try_into().unwrap()) as usize;
    assert_eq!(&exe[pe..pe + 4], b"PE\0\0");
    assert_eq!(u16::from_le_bytes([exe[pe + 4], exe[pe + 5]]), 0x8664);
}

#[test]
fn build_v2_cross_target_without_its_archive_names_the_recipe() {
    const TRIPLE: &str = "aarch64-unknown-linux-gnu";
    let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(TRIPLE)
        .join("release/libkarac_runtime.a");
    if !LLVM || archive.exists() || std::env::consts::ARCH == "aarch64" {
        return;
    }
    let dir = scratch("cross-no-archive");
    std::fs::write(
        dir.join("hello.kara"),
        "fn main() {\n    println(\"hi\");\n}\n",
    )
    .unwrap();
    let out = karac(
        &dir,
        &["build", "hello.kara", &format!("--target={TRIPLE}")],
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    let err = text(&out.stderr);
    assert!(
        err.contains(&format!(
            "a build for `{TRIPLE}` needs the runtime archive built for it"
        )) && err.contains(&format!("--target {TRIPLE} --crate-type staticlib")),
        "{err}"
    );
}
