//! B-2026-11-05-12: a release bundle carries wasi-libc next to its wasm
//! runtime archives (`lib/wasm32-wasip1/{crt1-command.o,libc.a}`,
//! `scripts/package-release.sh`), and `karac build --target=wasm_wasi`
//! links with those before asking the `rustc` on PATH, which outside the
//! repository is the user's default toolchain and need not have the wasm
//! target. Here a karac copied into a bundle-shaped directory links a
//! module with the bundle's libc, through a linker wrapper that records
//! its arguments, and the module runs under node.
//!
//! Skips (with a notice) when the wasm archive, the toolchain's wasi-libc
//! or a node with `node:wasi` is missing.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env: {}, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

fn rustc(args: &[&str]) -> Option<String> {
    let o = Command::new("rustc").args(args).output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[test]
fn wasm_build_links_the_bundled_wasi_libc() {
    let archive =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime_wasm.a");
    let libc_dir = rustc(&["--print", "target-libdir", "--target", "wasm32-wasip1"])
        .map(|d| Path::new(&d).join("self-contained"));
    let lld = rustc(&["--print", "sysroot"])
        .zip(rustc(&["-vV"]))
        .and_then(|(s, v)| {
            let host = v
                .lines()
                .find_map(|l| l.strip_prefix("host: "))?
                .to_string();
            Some(
                Path::new(&s)
                    .join("lib/rustlib")
                    .join(host)
                    .join("bin/rust-lld"),
            )
        });
    let node = Command::new("node").arg("--version").output();
    let (Some(libc_dir), Some(lld)) = (libc_dir, lld) else {
        eprintln!("SKIP: no rustc to take wasi-libc and rust-lld from");
        return;
    };
    if !archive.exists()
        || !libc_dir.join("libc.a").exists()
        || !lld.exists()
        || !matches!(node, Ok(ref o) if o.status.success())
    {
        eprintln!(
            "SKIP: needs {}, {}, {} and node",
            archive.display(),
            libc_dir.display(),
            lld.display()
        );
        return;
    }

    let root = std::env::temp_dir().join(format!("karac-wasm-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let scratch = Scratch(root.clone());
    let (bin, lib, work) = (root.join("b/bin"), root.join("b/lib"), root.join("work"));
    for d in [&bin, &lib.join("wasm32-wasip1"), &work] {
        std::fs::create_dir_all(d).unwrap();
    }
    let karac = bin.join("karac");
    std::fs::copy(env!("CARGO_BIN_EXE_karac"), &karac).unwrap();
    std::fs::copy(&archive, lib.join("libkarac_runtime_wasm.a")).unwrap();
    for f in ["crt1-command.o", "libc.a"] {
        std::fs::copy(libc_dir.join(f), lib.join("wasm32-wasip1").join(f)).unwrap();
    }
    // The wrapper records what it is given, then links with rust-lld.
    let args_file = root.join("args.txt");
    let wrapper = root.join("ld.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"$@\" > '{}'\nexec '{}' -flavor wasm \"$@\"\n",
            args_file.display(),
            lld.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        work.join("hello.kara"),
        "fn main() {\n    println(\"hello from the bundle\");\n}\n",
    )
    .unwrap();
    std::fs::write(work.join("run.mjs"), RUNNER).unwrap();

    let out = Command::new(&karac)
        .args(["build", "--target=wasm_wasi", "hello.kara"])
        .current_dir(&work)
        .env("KARAC_WASM_LD", &wrapper)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let args = std::fs::read_to_string(&args_file).unwrap();
    let bundled = bin.join("../lib/wasm32-wasip1/libc.a");
    assert!(
        args.contains(&bundled.display().to_string()),
        "the link did not take the bundle's libc ({}):\n{args}",
        bundled.display()
    );
    assert!(
        args.contains(
            &bin.join("../lib/libkarac_runtime_wasm.a")
                .display()
                .to_string()
        ),
        "the link did not take the bundle's runtime:\n{args}"
    );
    let run = Command::new("node")
        .args(["--no-warnings", "run.mjs", "hello.wasm"])
        .current_dir(&work)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "hello from the bundle\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    drop(scratch);
}
