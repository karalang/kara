//! Overriding the library's program-rooted resources with `with_provider`
//! (core-semantics §14.10-§14.14): the standard streams first. Each program
//! runs on the MIR interpreter (`karac __mir-run`) and, built ahead of time,
//! as a native binary; both must print the same, byte for byte.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-provider-overrides-{name}-{}",
            std::process::id()
        ));
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

/// What a run printed: its stdout, then its stderr.
type Printed = (String, String);

fn run(cmd: &mut Command, stdin: &str, what: &str) -> Printed {
    use std::io::Write;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// The program's output on the interpreter and, when the runtime archive
/// links, as a built binary, both fed `stdin`.
fn interp_and_build(dir: &Path, src: &str, stdin: &str) -> (Printed, Option<Printed>) {
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let interp = run(
        Command::new(karac)
            .current_dir(dir)
            .args(["__mir-run", file.to_str().unwrap()]),
        stdin,
        "interpreter",
    );
    let exe = dir.join("main");
    let built = Command::new(karac)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
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
    let binary = run(Command::new(&exe).current_dir(dir), stdin, "binary");
    (interp, Some(binary))
}

/// The program's MIR at `stage` (`built`, or `elaborated`: after its
/// providers are passed).
fn mir(dir: &Path, stage: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", dir.join("main.kara").to_str().unwrap()])
        .current_dir(dir)
        .env("KARAC_MIR_DUMP", stage)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// `print` and `println` under `with_provider[Stdout]` call the provider's
/// `write` with the formatted text, the newline included, in the callee
/// that prints too; `Stdout.println` and `stdout.flush()` reach it as
/// well. Outside the call they write to the process's stdout.
const STDOUT_OVERRIDE: &str = r#"
struct Tag { n: i64 }

impl StdoutProvider for Tag {
    fn write(mut ref self, s: Str) {
        self.n = self.n + 1;
        eprintln(f"[out {self.n}] {s}|");
    }

    fn flush(mut ref self) {
        eprintln("[flush]");
    }
}

fn helper(x: i64) {
    println(f"helper {x}");
}

fn main() {
    println("before");
    let got = with_provider[Stdout](Tag { n: 0 }, || {
        helper(1);
        Stdout.println("static");
        print("a");
        stdout.flush();
        7
    });
    println(f"after {got}");
}
"#;

#[test]
fn provider_stdout_override_takes_print_and_println() {
    let dir = Scratch::new("stdout");
    let (interp, built) = interp_and_build(dir.path(), STDOUT_OVERRIDE, "");
    assert_eq!(interp.0, "before\nafter 7\n");
    assert_eq!(
        interp.1,
        "[out 1] helper 1\n|\n[out 2] static\n|\n[out 3] a|\n[flush]\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
    // Outside the override, `println` keeps the fused native, which builds
    // no `String` (§14.10); inside, the text is formatted for `write`.
    let after = mir(dir.path(), "elaborated");
    let main = after
        .split("\nfn ")
        .find(|f| f.starts_with("main("))
        .expect("main's MIR");
    assert!(
        main.contains("println(const \"before\")") && !main.contains("format("),
        "main:\n{main}"
    );
    let helper = after
        .split("\nfn ")
        .find(|f| f.starts_with("helper{Stdout=Tag}("))
        .expect("helper's copy for the provider");
    assert!(
        helper.contains("format(") && helper.contains("Tag.write"),
        "helper:\n{helper}"
    );
}

/// A program that never overrides a stream prints through the fused
/// natives only, with no placeholder to rewrite, even when it binds
/// another resource.
const NO_STREAM_OVERRIDE: &str = r#"
struct FakeEnv { }

impl EnvProvider for FakeEnv {
    fn var(ref self, name: Str) -> Result[String, VarError] { Ok("faked") }
}

fn main() {
    let v = with_provider[Env](FakeEnv { }, || Env.var("A"));
    match v {
        Ok(s) => println(f"var {s}"),
        Err(_) => println("none"),
    }
    eprintln("done");
}
"#;

#[test]
fn provider_no_stream_override_keeps_the_fused_printers() {
    let dir = Scratch::new("no_stream");
    let (interp, built) = interp_and_build(dir.path(), NO_STREAM_OVERRIDE, "");
    assert_eq!(interp, ("var faked\n".to_string(), "done\n".to_string()));
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
    let before = mir(dir.path(), "built");
    assert!(
        !before.contains("Stdout.println(") && !before.contains("Stderr.println("),
        "{before}"
    );
}

/// `with_provider[Stderr]` takes `eprintln`; `with_provider[Stdin]` serves
/// `Stdin.read_line` and `stdin.read_to_string` from the provider, and the
/// real stdin is read again once the call returns.
const STDERR_AND_STDIN: &str = r#"
struct Collect { lines: i64 }

impl StderrProvider for Collect {
    fn write(mut ref self, s: Str) {
        self.lines = self.lines + 1;
        println(f"stderr {self.lines}: {s.trim()}");
    }
}

struct Canned { left: i64 }

impl StdinProvider for Canned {
    fn read_line(mut ref self) -> Result[String, IoError] {
        if self.left == 0 {
            return Ok("");
        }
        self.left = self.left - 1;
        Ok(f"canned {self.left}\n")
    }

    fn read_to_string(mut ref self) -> Result[String, IoError] {
        Ok("all of it")
    }
}

fn main() {
    with_provider[Stderr](Collect { lines: 0 }, || {
        eprintln("one");
        eprintln("two");
    });
    with_provider[Stdin](Canned { left: 2 }, || {
        loop {
            match Stdin.read_line() {
                Ok(s) if s.is_empty() => break,
                Ok(s) => print(f"got {s}"),
                Err(_) => break,
            }
        }
        match stdin.read_to_string() {
            Ok(s) => println(s),
            Err(_) => println("err"),
        }
    });
    match Stdin.read_line() {
        Ok(s) => print(f"real {s}"),
        Err(_) => println("err"),
    }
}
"#;

#[test]
fn provider_stderr_and_stdin_overrides() {
    let dir = Scratch::new("stderr_stdin");
    let (interp, built) = interp_and_build(dir.path(), STDERR_AND_STDIN, "piped\n");
    assert_eq!(
        interp,
        (
            "stderr 1: one\nstderr 2: two\ngot canned 1\ngot canned 0\nall of it\nreal piped\n"
                .to_string(),
            String::new()
        )
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}
