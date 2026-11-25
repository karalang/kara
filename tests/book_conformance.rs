//! The Kāra Book's claims, checked: conformance/book/SENTENCE_MAP.md maps
//! each stated output, error code and behaviour claim of docs/book/src to a
//! cell under conformance/book/<chapter>/. A cell is `<name>.kara`, or a
//! package directory `<name>/` holding a `kara.toml`, and what it is
//! checked against:
//!
//! | File | Check |
//! |---|---|
//! | `.out` | `karac run` prints exactly this on stdout, on the MIR interpreter (`--interp`) and, on the `--features llvm` leg, compiled |
//! | `.args` | extra arguments for that run (`--output=jsonl`) |
//! | `.exit` | the run's exit code (default 0) |
//! | `.stderr` | lines that must each appear in the run's stderr |
//! | `.has` | lines that must each appear in the run's stdout or stderr; a cell with only `.has` is run too |
//! | `.refused` | `karac check` fails, with each listed code among its errors |
//! | `.warns` | `karac check` passes and reports each listed lint name or code as a warning |
//! | `.clean` | `karac check` reports no diagnostic at all |
//! | `.explain` | `karac explain <file>.<first line>` prints each following line |
//! | `.test` | in a package, `karac test <first line>` exits 0 and prints each following line |
//! | `.open` | a ledger row: the cell fails today because of it, and the run fails once the cell passes, so the fix removes the file |

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn is_package(p: &Path) -> bool {
    p.is_dir() && p.join("kara.toml").is_file()
}

fn cells() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/book");
    let mut out = Vec::new();
    for ch in std::fs::read_dir(&root).unwrap().flatten() {
        if !ch.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(ch.path()).unwrap().flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == "kara") || is_package(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// A sidecar: `<cell>.<ext>` beside a file cell or a package directory.
fn side(cell: &Path, ext: &str) -> Option<String> {
    std::fs::read_to_string(cell.with_extension(ext)).ok()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let dst = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// Runs `karac <args> <cell>` in a scratch copy, so a file cell's own name
/// is `main.kara` wherever it lives; a package cell runs in its copied
/// directory with no file argument.
fn karac(cell: &Path, args: &[&str], after: Option<&str>) -> Output {
    let name = cell.file_stem().unwrap().to_string_lossy();
    let ch = cell
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    // The static and compiled tests run the same cells in parallel in one
    // process, so the directory carries a per-call count: with the pid and
    // cell alone, one test's `remove_dir_all` deleted the other's
    // `main.kara` mid-run ("cannot read 'main.kara'").
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("karac-book-{}-{n}-{ch}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    if is_package(cell) {
        copy_dir(cell, &dir);
        cmd.current_dir(&dir).args(args);
    } else {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(cell, dir.join("main.kara")).unwrap();
        cmd.current_dir(&dir).args(args);
        match after {
            Some(item) => cmd.arg(format!("main.kara.{item}")),
            None => cmd.arg("main.kara"),
        };
    }
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
}

fn label(cell: &Path) -> String {
    let p = cell
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(cell);
    p.display().to_string()
}

fn nonblank(s: &str) -> impl Iterator<Item = &str> {
    s.lines().filter(|l| !l.trim().is_empty())
}

fn check_run(cell: &Path, run: &[&str], fails: &mut Vec<String>) {
    let want = side(cell, "out");
    let also = side(cell, "has");
    if want.is_none() && also.is_none() {
        return;
    }
    let extra = side(cell, "args").unwrap_or_default();
    let mut args: Vec<&str> = run.to_vec();
    args.extend(extra.split_whitespace());
    let out = karac(cell, &args, None);
    let code = out.status.code().unwrap_or(-1);
    let want_code: i32 = side(cell, "exit").map_or(0, |s| s.trim().parse().unwrap());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let how = args.join(" ");
    if let Some(want) = want {
        if stdout != want {
            fails.push(format!(
                "{} ({how}): stdout\n--- want\n{want}--- got\n{stdout}--- stderr\n{stderr}",
                label(cell)
            ));
        }
    }
    if code != want_code {
        fails.push(format!(
            "{} ({how}): exit {code}, want {want_code}\n{stderr}",
            label(cell)
        ));
    }
    for line in side(cell, "stderr").unwrap_or_default().lines() {
        if !stderr.contains(line) {
            fails.push(format!(
                "{} ({how}): stderr lacks `{line}`:\n{stderr}",
                label(cell)
            ));
        }
    }
    let both = text(&out);
    for line in nonblank(&also.unwrap_or_default()) {
        if !both.contains(line) {
            fails.push(format!(
                "{} ({how}): output lacks `{line}`:\n{both}",
                label(cell)
            ));
        }
    }
}

fn diagnostics(cell: &Path) -> (bool, Vec<serde_json::Value>) {
    let out = karac(cell, &["check", "--output=json"], None);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{}: check output is not JSON ({e}):\n{}",
            label(cell),
            text(&out)
        )
    });
    let diags = doc["diagnostics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.get("code").is_some())
        .collect();
    (out.status.success(), diags)
}

fn has(diags: &[serde_json::Value], severity: &str, want: &str) -> bool {
    diags.iter().any(|d| {
        d["severity"] == severity
            && (d["code"] == want
                || d["parent_code"] == want
                || d["lint_name"] == want
                || d["message"].as_str().is_some_and(|m| m.contains(want)))
    })
}

fn check_static(cell: &Path, fails: &mut Vec<String>) {
    let refused = side(cell, "refused");
    let warns = side(cell, "warns");
    let clean = side(cell, "clean").is_some();
    if refused.is_none() && warns.is_none() && !clean {
        return;
    }
    let (ok, diags) = diagnostics(cell);
    if let Some(codes) = refused {
        if ok {
            fails.push(format!("{}: check passed, want a refusal", label(cell)));
        }
        for c in codes.lines().filter(|l| !l.trim().is_empty()) {
            if !has(&diags, "error", c.trim()) {
                fails.push(format!("{}: no error `{c}` in {diags:?}", label(cell)));
            }
        }
    }
    if let Some(ws) = warns {
        if !ok {
            fails.push(format!(
                "{}: check failed, want warnings only: {diags:?}",
                label(cell)
            ));
        }
        for w in ws.lines().filter(|l| !l.trim().is_empty()) {
            if !has(&diags, "warning", w.trim()) {
                fails.push(format!("{}: no warning `{w}` in {diags:?}", label(cell)));
            }
        }
    }
    if clean && (!ok || !diags.is_empty()) {
        fails.push(format!(
            "{}: want no diagnostic, got {diags:?}",
            label(cell)
        ));
    }
}

fn check_explain(cell: &Path, fails: &mut Vec<String>) {
    let Some(spec) = side(cell, "explain") else {
        return;
    };
    let mut lines = spec.lines();
    let item = lines.next().unwrap().trim();
    let out = karac(cell, &["explain"], Some(item));
    let got = text(&out);
    for want in lines.filter(|l| !l.trim().is_empty()) {
        if !got.contains(want) {
            fails.push(format!(
                "{}: explain {item} lacks `{want}`:\n{got}",
                label(cell)
            ));
        }
    }
}

fn check_test(cell: &Path, base: &[&str], fails: &mut Vec<String>) {
    let Some(spec) = side(cell, "test") else {
        return;
    };
    let mut ls = spec.lines();
    let filter = ls.next().unwrap_or_default().trim().to_string();
    let mut args: Vec<&str> = base.to_vec();
    if !filter.is_empty() {
        args.push(&filter);
    }
    let out = karac(cell, &args, None);
    let got = text(&out);
    let how = args.join(" ");
    if !out.status.success() {
        fails.push(format!(
            "{} ({how}): exit {:?}\n{got}",
            label(cell),
            out.status.code()
        ));
    }
    for want in ls.filter(|l| !l.trim().is_empty()) {
        if !got.contains(want) {
            fails.push(format!("{} ({how}): lacks `{want}`:\n{got}", label(cell)));
        }
    }
}

/// A cell with an `.open` row is expected to fail; one that passes has had
/// its row fixed, and says so.
fn settle(cell: &Path, mine: Vec<String>, fails: &mut Vec<String>) {
    match side(cell, "open") {
        Some(row) if mine.is_empty() => fails.push(format!(
            "{}: passes now; {} looks fixed, so delete the cell's .open",
            label(cell),
            row.trim()
        )),
        Some(_) => {}
        None => fails.extend(mine),
    }
}

#[test]
fn book_cells_static_and_interpreted() {
    let cells = cells();
    assert!(cells.len() > 20, "only {} cells", cells.len());
    let mut fails = Vec::new();
    for cell in &cells {
        let mut mine = Vec::new();
        check_static(cell, &mut mine);
        check_explain(cell, &mut mine);
        check_run(cell, &["run", "--interp"], &mut mine);
        check_test(cell, &["test", "--interp"], &mut mine);
        settle(cell, mine, &mut fails);
    }
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
}

#[cfg(feature = "llvm")]
#[test]
fn book_cells_compiled() {
    let mut fails = Vec::new();
    for cell in &cells() {
        let mut mine = Vec::new();
        check_run(cell, &["run"], &mut mine);
        check_test(cell, &["test"], &mut mine);
        // An open row's cell is judged on the interpreted leg.
        if side(cell, "open").is_none() {
            fails.extend(mine);
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
}
