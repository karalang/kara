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

/// `with_provider[FileSystem]` serves `fs`'s path operations from the
/// provider, in a callee too (§14.11); once the call returns, `fs` is the
/// real file system again.
const FILE_SYSTEM_PATHS: &str = r#"
struct MemFs { files: Map[String, String] }

impl FileSystemProvider for MemFs {
    fn read_to_string(ref self, path: Str) -> Result[String, IoError] {
        match self.files.get(path) {
            Some(s) => Ok(s.clone()),
            None => Err(IoError.NotFound),
        }
    }

    fn exists(ref self, path: Str) -> bool {
        self.files.contains_key(path)
    }

    fn write(mut ref self, path: Str, contents: Str) -> Result[(), IoError] {
        self.files.insert(path.to_string(), contents.to_string());
        Ok(())
    }
}

fn save(name: Str) -> Result[(), IoError] with writes(FileSystem) {
    fs.write(name, f"hello {name}")
}

fn main() {
    let r = with_provider[FileSystem](MemFs { files: Map.new() }, || {
        let a = save("kprov-a.txt");
        println(f"saved {a.is_ok()} {fs.exists("kprov-a.txt")} {fs.exists("kprov-b.txt")}");
        match FileSystem.read_to_string("kprov-a.txt") {
            Ok(s) => println(f"read {s}"),
            Err(e) => println(f"err {e}"),
        }
        match fs.read_to_string("kprov-b.txt") {
            Ok(s) => println(f"read {s}"),
            Err(e) => println(f"err {e}"),
        }
        3
    });
    println(f"after {r} {fs.exists("kprov-a.txt")}");
    let w = fs.write("kprov-c.txt", "on disk");
    match fs.read_to_string("kprov-c.txt") {
        Ok(s) => println(f"{w.is_ok()} {s}"),
        Err(e) => println(f"err {e}"),
    }
}
"#;

#[test]
fn provider_file_system_override_takes_path_operations() {
    let dir = Scratch::new("fs_paths");
    let (interp, built) = interp_and_build(dir.path(), FILE_SYSTEM_PATHS, "");
    assert_eq!(
        interp.0,
        "saved true true false\nread hello kprov-a.txt\nerr NotFound\nafter 3 false\ntrue on disk\n"
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A program's own `Stdout` or `FileSystem` type shadows the library's
/// resource of that name, in a program that overrides another resource.
const OWN_RESOURCE_NAMES: &str = r#"
struct Stdout { n: i64 }

struct FileSystem { m: i64 }

struct FakeEnv { }

impl EnvProvider for FakeEnv {
    fn var(ref self, name: Str) -> Result[String, VarError] { Ok("faked") }
}

fn main() {
    let s = Stdout { n: 4 };
    let f = FileSystem { m: 5 };
    let v = with_provider[Env](FakeEnv { }, || Env.var("A"));
    println(f"{s.n} {f.m} {v.is_ok()}");
}
"#;

#[test]
fn provider_own_type_named_like_a_resource_shadows_it() {
    let dir = Scratch::new("own_names");
    let (interp, built) = interp_and_build(dir.path(), OWN_RESOURCE_NAMES, "");
    assert_eq!(interp, ("4 5 true\n".to_string(), String::new()));
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A `File` opened under `with_provider[FileSystem]` reads, writes and
/// closes through the provider's handle methods; `flush` is `Ok`, `seek`
/// `Unsupported`, and once the call has returned the file fails with
/// `ProviderUnavailable` and drops quietly (core-semantics §14.11).
const FILE_HANDLES: &str = r#"
struct MemFs { data: Vec[u8], opened: i64, closed: i64 }

impl FileSystemProvider for MemFs {
    fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] {
        if path == "missing" {
            return Err(IoError.NotFound);
        }
        self.opened = self.opened + 1;
        Ok(self.opened)
    }

    fn read_handle(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, IoError] {
        let mut n = 0;
        while n < buf.len() and n < self.data.len() {
            buf[n] = self.data[n];
            n = n + 1;
        }
        Ok(n)
    }

    fn write_handle(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, IoError] {
        for b in buf {
            self.data.push(*b);
        }
        Ok(buf.len())
    }

    fn close(mut ref self, h: i64) {
        self.closed = self.closed + 1;
        println(f"close {h}");
    }
}

fn write_it() -> Result[i64, IoError] with writes(FileSystem) {
    let f = File.create("notes.txt")?;
    f.write("hello".bytes())
}

fn main() {
    let mut kept: Vec[File] = Vec.new();
    let total = with_provider[FileSystem](MemFs { data: Vec.new(), opened: 0, closed: 0 }, || {
        println(f"wrote {write_it()}");
        match File.open("missing") {
            Ok(_) => println("opened?"),
            Err(e) => println(f"err {e}"),
        }
        let f = File.options().read(true).open("notes.txt")?;
        let mut buf = [0u8; 8];
        let n = f.read(buf[..])?;
        println(f"read {n} {buf[0]} {f.flush().is_ok()} {f.seek(SeekFrom.Start, 0).is_ok()}");
        kept.push(File.open("later")?);
        Ok(n)
    });
    println(f"total {total}");
    let g = kept.pop();
    match g {
        Some(f) => {
            let mut buf = [0u8; 4];
            match f.read(buf[..]) {
                Ok(n) => println(f"late read {n}"),
                Err(e) => println(f"late err {e}"),
            }
        }
        None => println("none"),
    }
    println("end");
}
"#;

#[test]
fn provider_file_system_override_takes_file_handles() {
    let dir = Scratch::new("fs_handles");
    let (interp, built) = interp_and_build(dir.path(), FILE_HANDLES, "");
    assert_eq!(
        interp,
        (
            "close 1\nwrote Ok(5)\nerr NotFound\nread 5 104 true false\nclose 2\ntotal Ok(5)\nlate err ProviderUnavailable\nend\n".to_string(),
            String::new()
        )
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A file reaches the provider it was opened under while that provider is
/// live, inside a nested `with_provider[FileSystem]` too; an `Other`
/// error keeps its text, and a `Vec[File]` closes each file.
const NESTED_FILE_HANDLES: &str = r#"
struct Tagged { name: String, next: i64 }

impl FileSystemProvider for Tagged {
    fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] {
        if path == "bad" {
            return Err(IoError.Other(f"{self.name} refuses {path}"));
        }
        self.next = self.next + 1;
        Ok(self.next)
    }

    fn read_handle(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, IoError] {
        println(f"{self.name} reads {h}");
        if h == 99 {
            return Err(IoError.Other("never"));
        }
        Ok(0)
    }

    fn write_handle(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, IoError] {
        Err(IoError.Other(f"{self.name} is read-only"))
    }

    fn close(mut ref self, h: i64) {
        println(f"{self.name} closes {h}");
    }
}

fn peek(f: ref File) -> i64 with writes(FileSystem) {
    let mut b = [0u8; 2];
    match f.read(b[..]) {
        Ok(n) => n,
        Err(_) => -1,
    }
}

fn main() {
    with_provider[FileSystem](Tagged { name: "outer", next: 0 }, || {
        let f = File.open("a").unwrap();
        match File.open("bad") {
            Ok(_) => println("?"),
            Err(e) => println(f"err {e}"),
        }
        match f.write("x".bytes()) {
            Ok(_) => println("?"),
            Err(e) => println(f"err {e}"),
        }
        with_provider[FileSystem](Tagged { name: "inner", next: 10 }, || {
            println(f"peek outer file {peek(f)}");
            let g = File.open("b").unwrap();
            println(f"peek inner file {peek(g)}");
            let mut v: Vec[File] = Vec.new();
            v.push(File.open("c").unwrap());
            v.push(File.open("d").unwrap());
            println("vec built");
        });
        println("inner done");
    });
    println("end");
}
"#;

#[test]
fn provider_file_reaches_its_own_provider_inside_a_nested_override() {
    let dir = Scratch::new("fs_nested");
    let (interp, built) = interp_and_build(dir.path(), NESTED_FILE_HANDLES, "");
    assert_eq!(
        interp,
        (
            "err Other(outer refuses bad)\nerr Other(outer is read-only)\nouter reads 1\npeek outer file 0\ninner reads 11\npeek inner file 0\nvec built\ninner closes 12\ninner closes 13\ninner closes 11\ninner done\nouter closes 1\nend\n".to_string(),
            String::new()
        )
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// `BufReader` and `BufWriter` over a provider's file read lines and write
/// through its handle methods, short reads and writes included, and fail
/// with `ProviderUnavailable` once its call has returned.
const BUFFERED_FILE_HANDLES: &str = r#"
struct Mem { files: Map[i64, Vec[u8]], pos: Map[i64, i64], next: i64, chunk: i64 }

impl FileSystemProvider for Mem {
    fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] {
        self.next = self.next + 1;
        let mut data: Vec[u8] = Vec.new();
        if path == "in.txt" {
            for b in "alpha\nbeta\r\ngamma".bytes() {
                data.push(*b);
            }
        }
        self.files.insert(self.next, data);
        self.pos.insert(self.next, 0);
        Ok(self.next)
    }

    fn read_handle(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, IoError] {
        let data = self.files[h].clone();
        let mut at = self.pos[h];
        let mut n = 0;
        while n < buf.len() and n < self.chunk and at < data.len() {
            buf[n] = data[at];
            n = n + 1;
            at = at + 1;
        }
        self.pos[h] = at;
        Ok(n)
    }

    fn write_handle(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, IoError] {
        let mut n = 0;
        for b in buf {
            if n < self.chunk {
                self.files[h].push(*b);
                n = n + 1;
            }
        }
        Ok(n)
    }

    fn close(mut ref self, h: i64) {
        let mut text = String.new();
        for b in self.files[h] {
            text.push(char.try_from(*b as u32).unwrap());
        }
        println(f"close {h}: {text.len()} bytes [{text.replace("\n", "|")}]");
    }
}

fn main() {
    let mut late: Vec[BufReader[File]] = Vec.new();
    let mut late_w: Vec[BufWriter[File]] = Vec.new();
    with_provider[FileSystem](Mem { files: Map.new(), pos: Map.new(), next: 0, chunk: 3 }, || {
        let r = BufReader.new(File.open("in.txt").unwrap());
        for line in r.lines() {
            match line {
                Ok(l) => println(f"line [{l}]"),
                Err(e) => println(f"err {e}"),
            }
        }
        let r2 = BufReader.new(File.open("in.txt").unwrap());
        let mut first = String.new();
        let n = r2.read_line(first).unwrap();
        let mut rest = String.new();
        let m = r2.read_to_string(rest).unwrap();
        println(f"{n} [{first.trim()}] {m} [{rest.replace("\n", "|").replace("\r", "")}]");
        let w = BufWriter.new(File.create("out.txt").unwrap());
        w.write_all("hello buffered world\n".bytes()).unwrap();
        println(f"flush {w.flush().is_ok()}");
        let w2 = BufWriter.with_capacity(File.create("out2.txt").unwrap(), 4);
        let _ = w2.write("abcdefgh".bytes());
        late.push(BufReader.new(File.open("in.txt").unwrap()));
        late_w.push(BufWriter.new(File.create("late.txt").unwrap()));
        println("dropping");
    });
    let r = late.pop().unwrap();
    let mut s = String.new();
    match r.read_line(s) {
        Ok(n) => println(f"late {n}"),
        Err(e) => println(f"late err {e}"),
    }
    let w = late_w.pop().unwrap();
    println(f"late write {w.write_all("zz".bytes()).is_ok()}");
    match w.flush() {
        Ok(_) => println("late flush ok"),
        Err(e) => println(f"late flush err {e}"),
    }
    println("end");
}
"#;

#[test]
fn provider_buffered_files_reach_the_provider() {
    let dir = Scratch::new("fs_buffered");
    let (interp, built) = interp_and_build(dir.path(), BUFFERED_FILE_HANDLES, "");
    assert_eq!(
        interp,
        (
            "line [alpha]\nline [beta]\nline [gamma]\n6 [alpha] 11 [beta|gamma]\nflush true\ndropping\nclose 4: 3 bytes [abc]\nclose 3: 21 bytes [hello buffered world|]\nclose 2: 17 bytes [alpha|beta\r|gamma]\nclose 1: 17 bytes [alpha|beta\r|gamma]\nlate err ProviderUnavailable\nlate write true\nlate flush err ProviderUnavailable\nend\n".to_string(),
            String::new()
        )
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A `providers { FileSystem => p } in { .. }` block's files reach `p`
/// until the block ends, by falling off its end, `break`, `return` or
/// `?`; an OS file opened outside it works inside it.
const PROVIDERS_BLOCK_FILES: &str = r#"
struct MemFs { data: Vec[u8], opened: i64, closed: i64 }

impl FileSystemProvider for MemFs {
    fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] {
        if path == "missing" {
            return Err(IoError.NotFound);
        }
        self.opened = self.opened + 1;
        Ok(self.opened)
    }

    fn read_handle(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, IoError] {
        let mut n = 0;
        while n < buf.len() and n < self.data.len() {
            buf[n] = self.data[n];
            n = n + 1;
        }
        Ok(n)
    }

    fn write_handle(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, IoError] {
        for b in buf {
            self.data.push(*b);
        }
        Ok(buf.len())
    }

    fn close(mut ref self, h: i64) {
        self.closed = self.closed + 1;
        println(f"close {h}");
    }
}

fn write_it() -> Result[i64, IoError] with writes(FileSystem) {
    let f = File.create("notes.txt")?;
    f.write("hello".bytes())
}

fn early(stop: bool) -> Result[File, IoError] {
    providers { FileSystem => MemFs { data: Vec.new(), opened: 10, closed: 0 } } in {
        let f = File.open("a")?;
        if stop {
            return Ok(f);
        }
        let g = File.open("missing")?;
        Ok(g)
    }
}

fn main() {
    let mut kept: Vec[File] = Vec.new();
    let os = File.create("os.txt").unwrap();
    let _ = os.write("disk".bytes());
    let back = File.open("os.txt").unwrap();
    providers { FileSystem => MemFs { data: Vec.new(), opened: 0, closed: 0 } } in {
        match write_it() {
            Ok(n) => println(f"wrote {n}"),
            Err(e) => println(f"err {e}"),
        }
        let f = File.open("notes.txt").unwrap();
        let mut buf = [0u8; 8];
        println(f"read {f.read(buf[..]).unwrap()} {buf[0]}");
        kept.push(File.open("later").unwrap());
        let mut b = [0u8; 8];
        println(f"os read {back.read(b[..]).unwrap()} {b[0]}");
        println(f"os write {os.write("!".bytes()).unwrap()}");
    }
    for i in 0..3 {
        providers { FileSystem => MemFs { data: Vec.new(), opened: 20 + i, closed: 0 } } in {
            let f = File.open("x").unwrap();
            if i == 1 {
                kept.push(f);
                break;
            }
        }
    }
    match early(true) {
        Ok(f) => kept.push(f),
        Err(e) => println(f"early err {e}"),
    }
    match early(false) {
        Ok(f) => kept.push(f),
        Err(e) => println(f"early err {e}"),
    }
    println(f"kept {kept.len()}");
    while let Some(f) = kept.pop() {
        let mut buf = [0u8; 4];
        match f.read(buf[..]) {
            Ok(n) => println(f"late read {n}"),
            Err(e) => println(f"late err {e}"),
        }
    }
    println("end");
}
"#;

#[test]
fn provider_block_files_end_on_every_exit() {
    let dir = Scratch::new("fs_block");
    let (interp, built) = interp_and_build(dir.path(), PROVIDERS_BLOCK_FILES, "");
    assert_eq!(
        interp,
        (
            "close 1\nwrote 5\nread 5 104\nos read 4 100\nos write 1\nclose 2\nclose 21\nclose 11\nearly err NotFound\nkept 3\nlate err ProviderUnavailable\nlate err ProviderUnavailable\nlate err ProviderUnavailable\nend\n".to_string(),
            String::new()
        )
    );
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A `FileSystem` provider's handle methods run with the resources around
/// its `with_provider` call or `providers` block: at their defaults they
/// print, and a `Stdout` overridden there refuses the program, naming the
/// method (core-semantics §14.11; check side B-2026-10-29-15).
const HANDLER_RESOURCES: &str = r#"
struct MemFs { n: i64 }

impl FileSystemProvider for MemFs {
    fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] { Ok(1) }
    fn close(mut ref self, h: i64) { println(f"close {h}"); }
}

fn main() {
    with_provider[FileSystem](MemFs { n: 0 }, || {
        let _ = File.open("a");
    });
    providers { FileSystem => MemFs { n: 0 } } in {
        let _ = File.open("b");
    }
}
"#;

#[test]
fn provider_file_handle_methods_see_the_resources_around_the_override() {
    let dir = Scratch::new("fs_handler_resources");
    let (interp, built) = interp_and_build(dir.path(), HANDLER_RESOURCES, "");
    assert_eq!(interp, ("close 1\nclose 1\n".to_string(), String::new()));
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
    let refused = HANDLER_RESOURCES
        .replace(
            "fn main() {\n    with_provider[FileSystem]",
            "struct Cap { }\n\nimpl StdoutProvider for Cap {\n    fn write(mut ref self, s: Str) { }\n}\n\n\
             fn main() {\n    with_provider[Stdout](Cap { }, || main2());\n}\n\n\
             fn main2() with writes(Stdout) {\n    with_provider[FileSystem]",
        );
    assert!(refused.contains("main2"));
    let file = dir.path().join("refused.kara");
    std::fs::write(&file, refused).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "ran: {err}");
    assert!(
        err.contains("`MemFs.close` uses `Stdout`, which is overridden"),
        "{err}"
    );
}
