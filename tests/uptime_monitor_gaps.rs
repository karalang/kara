//! Found building the uptime_monitor app.
//!
//! B-2026-11-09-67: `karac fix` turned a `main` with a `?` into
//! `fn main() -> Result[(), AnyError]` and left its bare `return;`s, so the
//! fixed program failed with "expected return value". They now become
//! `return Ok(());`, except inside a closure.
//!
//! B-2026-11-09-68: Python's `{x:.1f}` and `{name:>8s}` were refused with
//! "unsupported type" and no fix. The letter is now named and deleted.
//!
//! B-2026-11-09-69: an HTTP request past its `timeout` failed in the
//! interpreter with ureq's "Network Error: Network Error: Error encountered
//! in the status line: timed out reading response", where a compiled
//! program says "<url>: request timed out after <ms> ms".
//!
//! B-2026-11-09-70: `File` had no `write_all`, which `TcpStream`,
//! `BufWriter` and `ChildStdin` have.
//!
//! B-2026-11-09-71: under a `FileSystem` override, a compiled `File.open`,
//! `File.create` or `OpenOptions.open` of a `String` path (not a literal)
//! passed the provider's `open` the `&String` where it takes a `Str`, so
//! the fake saw a garbage path (an ASan stack-buffer-overflow).

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn scratch(src: &str) -> std::path::PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path =
        std::env::temp_dir().join(format!("karac-uptime-gaps-{}-{n}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The source after `karac fix`, and whether `karac check` then passes.
fn fixed(src: &str) -> (String, bool, String) {
    let path = scratch(src);
    let p = path.to_str().unwrap();
    for _ in 0..3 {
        karac(&["fix", p]);
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let (ok, diags) = karac(&["check", p]);
    let _ = std::fs::remove_file(&path);
    (text, ok, diags)
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[cfg(feature = "llvm")]
fn compiled(src: &str) -> Option<String> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-uptime-gaps-b-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).current_dir(&dir).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    assert!(out.status.success(), "{:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn the_main_result_fix_returns_ok_from_a_bare_return() {
    let (text, ok, diags) = fixed(
        "fn main() {
    let n = i64.parse(\"12\")?;
    if n > 10 {
        println(\"big\");
        return;
    }
    let f = |x: i64| {
        if x > 0 {
            return;
        }
        println(\"neg\");
    };
    f(n);
}
",
    );
    assert!(ok, "{diags}\n{text}");
    assert!(
        text.contains("fn main() -> Result[(), AnyError] {"),
        "{text}"
    );
    assert!(
        text.contains("        println(\"big\");\n        return Ok(());"),
        "{text}"
    );
    // The closure's own `return` is the closure's.
    assert!(
        text.contains("        if x > 0 {\n            return;\n"),
        "{text}"
    );
}

#[test]
fn a_python_float_or_string_format_letter_is_named_and_deleted() {
    let src = "fn main() {
    let x = 2.0 / 3.0;
    let name = \"ada\";
    println(f\"{x:.2f} [{name:>6s}] [{name:s}] {x:>8.3F}\");
}
";
    let path = scratch(src);
    let (ok, diags) = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(!ok);
    assert!(
        diags.contains(":4:20: format spec `.2f`: Kāra has no `f` type, a float's precision alone sets its digits after the point; write `.2`"),
        "{diags}"
    );
    assert!(
        diags.contains("format spec `>6s`: Kāra has no `s` type"),
        "{diags}"
    );
    assert!(diags.contains("format spec `s`: Kāra has no `s` type, a value is formatted by its `Display` text; drop the `:s`"), "{diags}");
    let (text, ok, diags) = fixed(src);
    assert!(ok, "{diags}\n{text}");
    assert!(
        text.contains("println(f\"{x:.2} [{name:>6}] [{name}] {x:>8.3}\");"),
        "{text}"
    );
    assert_eq!(run(&text), "0.67 [   ada] [ada]    0.667\n");
}

#[test]
fn an_unknown_format_letter_keeps_the_general_message() {
    let path = scratch("fn main() {\n    let x = 2.5;\n    println(f\"{x:.1e}\");\n}\n");
    let (ok, diags) = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(!ok);
    assert!(diags.contains("unsupported type `e`"), "{diags}");
}

/// A server that takes connections and never answers, on a free port.
fn silent_server() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for c in l.incoming() {
            held.push(c);
        }
    });
    port
}

fn timeout_program(port: u16) -> String {
    format!(
        "fn main() {{
    let c = Client.new();
    match c.request(\"GET\".to_string(), \"http://127.0.0.1:{port}/slow\".to_string()).timeout(200).send() {{
        Ok(r) => println(f\"status {{r.status()}}\"),
        Err(e) => println(e.message()),
    }}
}}
"
    )
}

#[test]
fn an_http_timeout_says_so_in_the_interpreter() {
    let port = silent_server();
    assert_eq!(
        run(&timeout_program(port)),
        format!("http://127.0.0.1:{port}/slow: request timed out after 200 ms\n")
    );
}

#[cfg(feature = "llvm")]
#[test]
fn an_http_timeout_says_so_compiled() {
    let port = silent_server();
    if let Some(out) = compiled(&timeout_program(port)) {
        assert_eq!(
            out,
            format!("http://127.0.0.1:{port}/slow: request timed out after 200 ms\n")
        );
    }
}

const WRITE_ALL: &str = "fn save(path: Str, text: Str) -> Result[(), IoError] {
    let f = File.create(path)?;
    f.write_all(text.bytes())?;
    f.write_all(\"\".bytes())?;
    f.sync_all()?;
    Ok(())
}

fn main() {
    let path = \"karac_write_all_probe.txt\";
    match save(path, \"hello\\nworld\\n\") {
        Ok(()) => print(fs.read_to_string(path) ?? \"?\".to_string()),
        Err(e) => println(f\"error: {e}\"),
    }
    let _ = fs.remove_file(path);
}
";

#[test]
fn a_file_writes_all() {
    let path = scratch(WRITE_ALL);
    let (ok, diags) = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(ok, "{diags}");
    assert_eq!(run(WRITE_ALL), "hello\nworld\n");
}

#[cfg(feature = "llvm")]
#[test]
fn a_file_writes_all_compiled() {
    if let Some(out) = compiled(WRITE_ALL) {
        assert_eq!(out, "hello\nworld\n");
    }
}

/// A fake file system keyed by the paths its `open` was given, opened
/// with `String` paths built at run time.
const FAKE_FS_PATHS: &str = "struct MemFs {
    files: Map[String, String],
    open: Map[i64, String],
    next: i64,
}

impl FileSystemProvider for MemFs {
    fn read_to_string(ref self, path: Str) -> Result[String, IoError] {
        match self.files.get(path) {
            Some(s) => Ok(s.clone()),
            None => Err(IoError.NotFound),
        }
    }

    fn rename(mut ref self, from: Str, to: Str) -> Result[(), IoError] {
        match self.files.remove(from) {
            Some(s) => {
                self.files.insert(to.to_string(), s);
                Ok(())
            }
            None => Err(IoError.NotFound),
        }
    }

    fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] {
        self.next += 1;
        self.open.insert(self.next, path.to_string());
        self.files.insert(path.to_string(), String.new());
        Ok(self.next)
    }

    fn write_handle(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, IoError] {
        let key = self.open.get(h).ok_or(IoError.InvalidInput)?.clone();
        let old = self.files.get(key.as_str()).unwrap_or(\"\").to_string();
        let text = String.from_utf8(buf).unwrap_or(String.new());
        self.files.insert(key, f\"{old}{text}\");
        Ok(buf.len())
    }

    fn close(mut ref self, h: i64) {
        self.open.remove(h);
    }
}

fn save(path: Str, text: Str) -> Result[(), IoError] {
    let tmp = f\"{path}.tmp\";
    let f = File.options().write(true).create(true).truncate(true).open(tmp)?;
    f.write_all(text.bytes())?;
    f.sync_all()?;
    fs.rename(tmp, path)?;
    Ok(())
}

fn main() {
    let m = MemFs { files: Map.new(), open: Map.new(), next: 0 };
    with_provider[FileSystem](m, || {
        let dir = \"/var/state\";
        match save(f\"{dir}/a\".as_str(), \"saved\") {
            Ok(()) => println(fs.read_to_string(\"/var/state/a\") ?? \"none\".to_string()),
            Err(e) => println(f\"save failed: {e}\"),
        }
        let name = f\"{dir}/b\";
        match File.create(name) {
            Ok(f) => {
                let _ = f.write_all(\"created\".bytes());
            }
            Err(e) => println(f\"create failed: {e}\"),
        }
        println(fs.read_to_string(\"/var/state/b\") ?? \"none\".to_string());
    });
}
";

#[test]
fn a_fake_file_system_opens_string_paths() {
    assert_eq!(run(FAKE_FS_PATHS), "saved\ncreated\n");
}

#[cfg(feature = "llvm")]
#[test]
fn a_fake_file_system_opens_string_paths_compiled() {
    if let Some(out) = compiled(FAKE_FS_PATHS) {
        assert_eq!(out, "saved\ncreated\n");
    }
}
