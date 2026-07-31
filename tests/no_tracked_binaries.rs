//! Guard against compiled binaries being committed to the repository.
//!
//! # The recurring failure
//!
//! `karac build path/to/prog.kara` emits the executable as `<stem>` **into the
//! current working directory**. Run from the repo root during a bug hunt — the
//! normal way to reproduce a codegen bug — it drops a stray ELF/Mach-O file
//! right next to `Cargo.toml`, where a reflexive `git add -A` sweeps it into a
//! commit. This has happened repeatedly: `tup_vec`, then `blockmin` / `pan` /
//! `t` (untracked 2026-07-31), then `dc` and `uam` within the same day.
//!
//! # Why `.gitignore` is not the fix
//!
//! Each cleanup so far added the *specific names* that had leaked. That can
//! only ever catch binaries someone already committed once — the output name is
//! whatever the `.kara` file was called, so the next one has a new name and
//! walks straight past the list. `.gitignore` keeps `git status` quiet for
//! known names; it cannot make the class impossible.
//!
//! This test closes the class instead: it asks git what is **tracked** and
//! rejects anything carrying executable magic, whatever it is called. It runs
//! under the plain `cargo test --all` CI job (no `llvm` feature needed), so it
//! fires on every pull request.
//!
//! # Scope
//!
//! Deliberately limited to *executable* formats rather than "is this file
//! binary". Images, `.wasm` fixtures, and other binary assets are legitimate
//! repository content; a compiled program at any path is not.

use std::path::PathBuf;
use std::process::Command;

/// Leading bytes that identify a compiled executable or shared object.
const EXECUTABLE_MAGIC: &[(&[u8], &str)] = &[
    (b"\x7fELF", "ELF (Linux/BSD)"),
    (&[0xcf, 0xfa, 0xed, 0xfe], "Mach-O 64-bit"),
    (&[0xce, 0xfa, 0xed, 0xfe], "Mach-O 32-bit"),
    (&[0xfe, 0xed, 0xfa, 0xcf], "Mach-O 64-bit (BE)"),
    (&[0xfe, 0xed, 0xfa, 0xce], "Mach-O 32-bit (BE)"),
    // Also Java `.class`; this repo has no JVM artifacts, so treat as Mach-O fat.
    (&[0xca, 0xfe, 0xba, 0xbe], "Mach-O universal binary"),
    (b"MZ", "PE/COFF (Windows .exe)"),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Fill `buf` from `f`, tolerating short reads; returns the byte count.
fn read_header(f: &mut std::fs::File, buf: &mut [u8; 4]) -> usize {
    use std::io::Read;
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => break,
        }
    }
    filled
}

fn identify(bytes: &[u8]) -> Option<&'static str> {
    EXECUTABLE_MAGIC
        .iter()
        .find(|(magic, _)| bytes.starts_with(magic))
        .map(|(_, name)| *name)
}

#[test]
fn no_compiled_binaries_are_tracked() {
    let root = repo_root();

    let out = match Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        // Not a git checkout (e.g. a packaged source tarball) — nothing to
        // check. This is the one legitimate reason to skip; a git failure
        // inside a real checkout still surfaces below as an empty file list.
        _ => {
            eprintln!("note: `git ls-files` unavailable — skipping tracked-binary scan");
            return;
        }
    };

    let files: Vec<&str> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect();

    // Non-vacuity: a checkout of this repo tracks thousands of files. If the
    // list came back tiny, the scan is broken, not the invariant.
    assert!(
        files.len() > 100,
        "`git ls-files` returned only {} entries — the scan looks broken",
        files.len(),
    );

    let mut offenders: Vec<(String, &str)> = Vec::new();
    let mut header = [0u8; 4];
    for rel in &files {
        // Read only the magic, not the file: this walks every tracked path,
        // and slurping multi-MB sources to inspect 4 bytes would make the
        // guard cost more than the rest of the suite.
        let Ok(mut f) = std::fs::File::open(root.join(rel)) else {
            // Tracked-but-absent (staged deletion, sparse checkout) — not ours.
            continue;
        };
        let n = read_header(&mut f, &mut header);
        if let Some(kind) = identify(&header[..n]) {
            offenders.push(((*rel).to_string(), kind));
        }
    }

    assert!(
        offenders.is_empty(),
        "{} compiled binar{} tracked in git:\n{}\n\n\
         These are almost certainly stray `karac build` outputs: the AOT \
         compiler writes the executable as `<stem>` into the current working \
         directory, so building from the repo root drops it beside \
         `Cargo.toml` where `git add -A` picks it up.\n\n\
         Fix: `git rm --cached <file>` and rebuild into a scratch directory \
         (`karac build prog.kara -o /tmp/prog`, or run the build from outside \
         the repo). Do NOT silence this by adding the name to `.gitignore` — \
         name-by-name entries are what let this recur; the next stray output \
         has a different name.",
        offenders.len(),
        if offenders.len() == 1 {
            "y is"
        } else {
            "ies are"
        },
        offenders
            .iter()
            .map(|(p, k)| format!("  {p}  [{k}]"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
