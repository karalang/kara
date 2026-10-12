//! The I/O natives on the MIR backend (redesign M2): `File` and `Clock`.
//! The program runs on the MIR interpreter (`karac __mir-run`) and, built
//! ahead of time (`karac build --mir`), as a native binary; both must print
//! the same, byte for byte.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("karac-mir-io-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The program's stdout on the interpreter and, when the runtime archive
/// links, as a binary built with `--mir`. `data.txt` in `dir` is removed
/// before each run, so both start from nothing.
fn interp_and_build(dir: &Path, src: &str) -> (String, Option<String>) {
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let data = dir.join("data.txt");
    let _ = std::fs::remove_file(&data);
    let interp = Command::new(karac)
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        interp.status.success(),
        "interpreter: {}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let interp = String::from_utf8(interp.stdout).unwrap();
    let exe = dir.join("main");
    let built = Command::new(karac)
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_none() {
        return (interp, None);
    }
    let _ = std::fs::remove_file(&data);
    let run = Command::new(&exe).output().unwrap();
    assert!(
        run.status.success(),
        "binary: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    (interp, Some(String::from_utf8(run.stdout).unwrap()))
}

const FILES: &str = r#"
fn show(e: IoError) -> String {
    match e {
        IoError.NotFound => "not found".to_string(),
        IoError.Other(m) => f"other {m}",
        _ => "some other error".to_string(),
    }
}

fn p() -> String { "DIR/data.txt".to_string() }

fn main() {
    match File.create(p()) {
        Ok(f) => {
            let mut data: Vec[u8] = Vec.new();
            data.push(65u8); data.push(66u8); data.push(10u8);
            match f.write(data) { Ok(n) => println(f"wrote {n}"), Err(e) => println(show(e)) }
            match f.sync_all() { Ok(_) => println("synced"), Err(e) => println(show(e)) }
            match f.sync_data() { Ok(_) => println("data synced"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match File.append(p()) {
        Ok(f) => {
            let more = [67u8, 68u8, 10u8];
            match f.write(more[0..3]) { Ok(n) => println(f"appended {n}"), Err(e) => println(show(e)) }
            match f.flush() { Ok(_) => println("flushed"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match FileSystem.read_to_string(p()) {
        Ok(s) => println(f"contents {s.len()} {s.trim()}"),
        Err(e) => println(show(e)),
    }
    match File.open(p()) {
        Ok(g) => {
            match g.seek(SeekFrom.Start, 1i64) { Ok(p) => println(f"pos {p}"), Err(e) => println(show(e)) }
            let mut buf = [0u8, 0u8, 0u8];
            match g.read(mut buf) { Ok(n) => println(f"read {n} {buf[0]} {buf[1]} {buf[2]}"), Err(e) => println(show(e)) }
            match g.seek(SeekFrom.End, -2i64) { Ok(p) => println(f"end {p}"), Err(e) => println(show(e)) }
            match g.seek(SeekFrom.Current, 0i64) { Ok(p) => println(f"cur {p}"), Err(e) => println(show(e)) }
            match g.seek(SeekFrom.Start, -5i64) { Ok(_) => println("unexpected"), Err(e) => println(show(e)) }
        }
        Err(e) => println(show(e)),
    }
    match File.open("DIR/missing/none.txt".to_string()) {
        Ok(_) => println("unexpected"),
        Err(e) => println(show(e)),
    }
    let t = Clock.now();
    println(f"clock {t > 1700000000}");
}
"#;

/// Create, write, sync, append, reopen, seek and read a file, and a
/// missing one, on both backends.
#[test]
fn file_and_clock_natives_match_the_interpreter() {
    let dir = Scratch::new("files");
    let src = FILES.replace("DIR", &dir.path().to_string_lossy());
    let (interp, built) = interp_and_build(dir.path(), &src);
    assert_eq!(
        interp,
        "wrote 3\nsynced\ndata synced\nappended 3\nflushed\ncontents 6 AB\nCD\npos 1\n\
         read 3 66 10 67\nend 4\ncur 4\nother invalid seek to a negative position\n\
         not found\nclock true\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}
