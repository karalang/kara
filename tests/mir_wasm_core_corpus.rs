//! `corpus/core` and `corpus/design` on `--target=wasm_wasi`, run under
//! node's WASI and compared with each entry's expected output, as
//! `scripts/corpus/run.py` compares a native backend's.
//!
//! The four wasm32 miscompiles B-2026-11-15-3..6 were each correct natively
//! and passed every wasm test CI had; a probe found them by running these
//! entries on wasm. This lane runs them on every push so the next one shows
//! up in CI's `wasm-e2e` job.
//!
//! An entry is taken when it expects stdout and exit 0 with no env, stdin,
//! args, backend restriction or `bench` tag. `REFUSED_ON_WASM` names the
//! entries the wasm target refuses by design (a resource it does not
//! provide); each must still be refused, so a lifted refusal is noticed.
//!
//! Skips (with a notice) when the wasm archive or node is missing.
#![cfg(feature = "llvm")]
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Entries `wasm_wasi` refuses to build, with the reason the refusal names.
const REFUSED_ON_WASM: &[(&str, &str)] = &[
    (
        "core/par_clause_capabilities_apart",
        "does not provide resource 'Network'",
    ),
    (
        "core/par_real_clients",
        "does not provide resource 'Network'",
    ),
    // `std.crypto`'s digests are native code only the native runtime
    // carries, so the import is refused on wasm (B-2026-11-16-197).
    (
        "design/ok_crypto_known_answer_vectors",
        "is not available on target `wasm_wasi`",
    ),
];

/// The driver's refusal when an entry needs a wasm opt-in archive
/// (`std.regex`, `String.normalize`) this host has not built: the entry is
/// skipped with a notice naming it, as the native opt-in tests skip.
const OPT_IN_MISSING: &str = "which on wasm needs the runtime archive";

/// Entries skipped for a missing opt-in archive.
static SKIPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Runs a preview1 module under node: argv[2] is the module. The corpus
/// runner pins the hash seed, so this does too.
const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env: { KARAC_HASH_SEED: '1' }, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn wasm_archive() -> PathBuf {
    root().join("target/release/libkarac_runtime_wasm.a")
}

/// Why this host cannot run the lane, if it cannot.
fn skip_reason() -> Option<String> {
    if !wasm_archive().exists() {
        return Some(format!(
            "{} is missing (see CLAUDE.md's archive recipe)",
            wasm_archive().display()
        ));
    }
    match Command::new("node").arg("--version").output() {
        Ok(o) if o.status.success() => None,
        _ => Some("node is not installed".into()),
    }
}

struct Entry {
    /// `core/<name>` or `design/<name>`.
    name: String,
    dir: PathBuf,
    /// Compare lines as a multiset (`model_verdict = "ORDER-UNSPEC"`).
    as_set: bool,
}

fn is_empty(v: Option<&toml::Value>) -> bool {
    match v {
        None => true,
        Some(toml::Value::Table(t)) => t.is_empty(),
        Some(toml::Value::Array(a)) => a.is_empty(),
        Some(toml::Value::String(s)) => s.is_empty(),
        Some(_) => false,
    }
}

fn entries() -> Vec<Entry> {
    let mut out = Vec::new();
    for group in ["core", "design"] {
        let base = root().join("corpus").join(group);
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
            .unwrap_or_else(|e| panic!("{}: {e}", base.display()))
            .map(|d| d.unwrap().path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let meta = dir.join("meta.toml");
            if !dir.join("source.kara").exists()
                || !dir.join("expected.out").exists()
                || !meta.exists()
                || dir.join("stdin.txt").exists()
                || dir.join("args.txt").exists()
            {
                continue;
            }
            let Ok(meta) = std::fs::read_to_string(&meta)
                .unwrap()
                .parse::<toml::Table>()
            else {
                continue;
            };
            let bench = meta
                .get("tags")
                .and_then(|t| t.as_array())
                .is_some_and(|t| t.iter().any(|t| t.as_str() == Some("bench")));
            if meta.get("expect").and_then(|v| v.as_str()) != Some("stdout")
                || meta.get("exit").and_then(|v| v.as_integer()).unwrap_or(0) != 0
                || !is_empty(meta.get("env"))
                || !is_empty(meta.get("stdin"))
                || !is_empty(meta.get("backends"))
                || bench
            {
                continue;
            }
            let as_set = meta.get("model_verdict").and_then(|v| v.as_str()) == Some("ORDER-UNSPEC");
            let name = format!("{group}/{}", dir.file_name().unwrap().to_string_lossy());
            out.push(Entry { name, dir, as_set });
        }
    }
    out
}

/// One entry's failure, or `None` when it passed.
fn run_one(e: &Entry, scratch: &Path) -> Option<String> {
    let dir = scratch.join(e.name.replace('/', "__"));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("source.kara");
    std::fs::copy(e.dir.join("source.kara"), &src).unwrap();
    let wasm = dir.join("source.wasm");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_wasi"])
        .arg(&src)
        .arg("-o")
        .arg(&wasm)
        // No `KARAC_RUNTIME`: the driver picks the archive from the object's
        // symbols, so a `std.regex` entry takes `libkarac_runtime_wasm_regex.a`
        // as a user's build does. Forced to the plain archive, that entry's
        // link failed on `karac_regex_mir`.
        .current_dir(&dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&built.stderr);
    let refused = REFUSED_ON_WASM.iter().find(|(n, _)| *n == e.name);
    if let Some((_, why)) = refused {
        return if built.status.success() {
            Some(format!(
                "{}: built on wasm; drop it from REFUSED_ON_WASM",
                e.name
            ))
        } else if !stderr.contains(why) {
            Some(format!("{}: refused for another reason: {stderr}", e.name))
        } else {
            None
        };
    }
    if !built.status.success() {
        if stderr.contains(OPT_IN_MISSING) {
            eprintln!(
                "mir_wasm_core_corpus: skipped {}: its wasm opt-in runtime archive is not built (CLAUDE.md's archive recipe)",
                e.name
            );
            SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        return Some(format!("{}: build failed: {stderr}", e.name));
    }
    let run = Command::new("node")
        .arg("--no-warnings")
        .arg(scratch.join("run.mjs"))
        .arg(&wasm)
        .current_dir(&dir)
        .output()
        .unwrap();
    let got = String::from_utf8_lossy(&run.stdout);
    let want = std::fs::read_to_string(e.dir.join("expected.out")).unwrap();
    let same = if e.as_set {
        let mut a: Vec<&str> = got.lines().collect();
        let mut b: Vec<&str> = want.lines().collect();
        a.sort_unstable();
        b.sort_unstable();
        a == b
    } else {
        got == want
    };
    if run.status.code() != Some(0) {
        return Some(format!(
            "{}: exit {:?}: {}",
            e.name,
            run.status.code(),
            String::from_utf8_lossy(&run.stderr)
        ));
    }
    if !same {
        return Some(format!(
            "{}: stdout differs\n--- want\n{want}--- got\n{got}",
            e.name
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
    None
}

#[test]
fn wasm_runs_corpus_core_and_design_as_expected() {
    if let Some(why) = skip_reason() {
        eprintln!("skipping mir_wasm_core_corpus: {why}");
        return;
    }
    let all = entries();
    // Guards against a selection that silently takes nothing.
    assert!(all.len() > 150, "only {} entries selected", all.len());
    for (name, _) in REFUSED_ON_WASM {
        assert!(
            all.iter().any(|e| e.name == *name),
            "{name} is in REFUSED_ON_WASM but not selected"
        );
    }
    let scratch = std::env::temp_dir().join(format!("karac-wasm-core-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("run.mjs"), RUNNER).unwrap();
    let next = Mutex::new(0usize);
    let failures = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(8));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = {
                    let mut n = next.lock().unwrap();
                    let i = *n;
                    *n += 1;
                    i
                };
                let Some(e) = all.get(i) else { break };
                if let Some(f) = run_one(e, &scratch) {
                    failures.lock().unwrap().push(f);
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&scratch);
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    assert!(
        failures.is_empty(),
        "{} of {} entries differ on wasm_wasi:\n\n{}",
        failures.len(),
        all.len(),
        failures.join("\n\n")
    );
    let skipped = SKIPPED.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "mir_wasm_core_corpus: {} entries agree on wasm, {skipped} skipped",
        all.len() - skipped
    );
}
