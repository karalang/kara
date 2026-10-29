//! Overriding `Network` with `with_provider` (core-semantics §14.12): a
//! fake's sockets, client and server side, on the MIR interpreter
//! (`karac __mir-run`) and, built ahead of time, as a native binary; both
//! must print the same, byte for byte. And `karac test` on a package whose
//! test file binds `Network`, which leaves the package's build untouched.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-network-provider-{name}-{}",
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

fn stdout_of(out: Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The program's stdout on the interpreter and, when the runtime archive
/// links, as a built binary.
fn interp_and_build(dir: &Path, src: &str) -> (String, Option<String>) {
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let run = |cmd: &mut Command| cmd.stdin(Stdio::null()).output().unwrap();
    let interp = stdout_of(
        run(Command::new(karac).args(["__mir-run", file.to_str().unwrap()])),
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
    let binary = stdout_of(run(&mut Command::new(&exe)), "binary");
    (interp, Some(binary))
}

fn assert_both(name: &str, src: &str, want: &str) {
    let dir = Scratch::new(name);
    let (interp, built) = interp_and_build(dir.path(), src);
    assert_eq!(interp, want);
    if let Some(built) = built {
        assert_eq!(built, interp);
    }
}

/// A client: `connect` reaches the fake, the stream's `write_all`, `read`
/// and `shutdown_write` its `write_stream`, `read_stream` and `shutdown`,
/// a refusal comes back as the fake's error, and its drop calls `close`.
/// A stream kept past the override fails with `ProviderUnavailable` and
/// drops quietly.
const CLIENT: &str = r#"
struct Fake { n: i64, data: Vec[u8] }

impl NetworkProvider for Fake {
    fn connect(mut ref self, addr: Str) -> Result[i64, NetError] {
        if addr == "refuse" {
            return Err(NetError.ConnectionRefused);
        }
        self.n = self.n + 1;
        Ok(self.n)
    }

    fn read_stream(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, NetError] {
        let mut k = 0;
        while k < buf.len() and k < self.data.len() {
            buf[k] = self.data[k];
            k = k + 1;
        }
        Ok(k)
    }

    fn write_stream(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, NetError] {
        for b in buf {
            self.data.push(*b);
        }
        Ok(buf.len())
    }

    fn shutdown(mut ref self, h: i64) -> Result[(), NetError] {
        println(f"shutdown {h}");
        Ok(())
    }

    fn close(mut ref self, h: i64) {
        println(f"close {h}");
    }
}

fn why(e: NetError) -> String {
    match e {
        NetError.ConnectionRefused => "refused".to_string(),
        NetError.ProviderUnavailable => "provider unavailable".to_string(),
        NetError.Other(c) => f"other {c}",
        _ => "else".to_string(),
    }
}

fn talk() -> Result[i64, NetError] {
    let s = TcpStream.connect("10.0.0.1:80")?;
    let _ = s.write_all("ping".bytes())?;
    let mut buf = [0u8; 8];
    let n = s.read(mut buf)?;
    let _ = s.shutdown_write()?;
    Ok(n)
}

fn main() {
    let mut kept: Vec[TcpStream] = Vec.new();
    with_provider[Network](Fake { n: 0, data: Vec.new() }, || {
        match talk() {
            Ok(n) => println(f"read {n}"),
            Err(e) => println(f"err {why(e)}"),
        }
        match TcpStream.connect("refuse") {
            Ok(_) => println("?"),
            Err(e) => println(f"err {why(e)}"),
        }
        kept.push(TcpStream.connect("later").unwrap());
    });
    let s = kept.pop().unwrap();
    let mut buf = [0u8; 4];
    match s.read(mut buf) {
        Ok(n) => println(f"late {n}"),
        Err(e) => println(f"late err {why(e)}"),
    }
    println("end");
}
"#;

#[test]
fn provider_network_override_takes_client_sockets() {
    assert_both(
        "client",
        CLIENT,
        "shutdown 1\nclose 1\nread 4\nerr refused\nlate err provider unavailable\nend\n",
    );
}

/// A server in a `providers` block: `bind` and `accept` reach the fake,
/// an accepted stream belongs to it too, its errors keep their payloads,
/// and `try_clone` on a fake's socket is `Other(-1)`.
const SERVER: &str = r#"
struct Wire { next: i64, pending: Vec[i64], log: Vec[String] }

impl NetworkProvider for Wire {
    fn bind(mut ref self, addr: Str) -> Result[i64, NetError] {
        if addr == "busy" {
            return Err(NetError.AddrInUse);
        }
        self.pending.push(7);
        self.pending.push(8);
        Ok(100)
    }

    fn accept(mut ref self, listener: i64) -> Result[i64, NetError] {
        match self.pending.pop() {
            Some(h) => Ok(h),
            None => Err(NetError.Other(42)),
        }
    }

    fn read_stream(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, NetError] {
        if h == 7 {
            return Err(NetError.TimedOut);
        }
        buf[0] = 104u8;
        buf[1] = 105u8;
        Ok(2)
    }

    fn write_stream(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, NetError] {
        println(f"write {h} {buf.len()}");
        Ok(buf.len())
    }

    fn close(mut ref self, h: i64) {
        println(f"close {h}");
    }
}

fn why(e: NetError) -> String {
    match e {
        NetError.AddrInUse => "in use".to_string(),
        NetError.TimedOut => "timed out".to_string(),
        NetError.ProviderUnavailable => "provider unavailable".to_string(),
        NetError.Other(c) => f"other {c}",
        _ => "else".to_string(),
    }
}

fn serve() -> Result[i64, NetError] {
    let l = TcpListener.bind("0.0.0.0:80")?;
    let mut total = 0;
    loop {
        let s = match l.accept() {
            Ok(s) => s,
            Err(e) => {
                println(f"accept {why(e)}");
                return Ok(total);
            }
        };
        let mut buf = [0u8; 4];
        match s.read(mut buf) {
            Ok(n) => {
                total = total + n;
                let _ = s.write_all(buf[0..n])?;
            }
            Err(e) => println(f"read {why(e)}"),
        }
        match s.try_clone() {
            Ok(_) => println("clone?"),
            Err(e) => println(f"clone {why(e)}"),
        }
    }
}

fn main() {
    providers {
        Network => Wire { next: 0, pending: Vec.new(), log: Vec.new() },
    } in {
        match serve() {
            Ok(n) => println(f"served {n}"),
            Err(e) => println(f"err {why(e)}"),
        }
        match TcpListener.bind("busy") {
            Ok(_) => println("?"),
            Err(e) => println(f"bind {why(e)}"),
        }
    }
}
"#;

#[test]
fn provider_network_block_takes_listeners_and_accepted_streams() {
    assert_both(
        "server",
        SERVER,
        "write 8 2\nclone other -1\nclose 8\nread timed out\nclone other -1\nclose 7\n\
         accept other 42\nclose 100\nserved 2\nbind in use\n",
    );
}

/// A socket reaches the fake it was opened under inside a nested override,
/// and from a task, whose read of a fake's stream completes at once.
const NESTED: &str = r#"
struct Named { name: String, next: i64 }

impl NetworkProvider for Named {
    fn connect(mut ref self, addr: Str) -> Result[i64, NetError] {
        self.next = self.next + 1;
        Ok(self.next)
    }

    fn read_stream(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, NetError] {
        println(f"{self.name} reads {h}");
        Ok(0)
    }

    fn close(mut ref self, h: i64) {
        println(f"{self.name} closes {h}");
    }
}

fn drain(s: ref TcpStream) -> i64 {
    let mut b = [0u8; 4];
    match s.read(b[..]) {
        Ok(n) => n,
        Err(_) => -1,
    }
}

fn main() {
    with_provider[Network](Named { name: "outer".to_string(), next: 0 }, || {
        let s = TcpStream.connect("a:1").unwrap();
        with_provider[Network](Named { name: "inner".to_string(), next: 10 }, || {
            let t = TcpStream.connect("b:1").unwrap();
            println(f"s {drain(s)} t {drain(t)}");
        });
        let group = TaskGroup.new();
        let h = group.spawn(|| drain(s));
        println(f"task {h.join()}");
    });
    println("end");
}
"#;

#[test]
fn provider_network_socket_reaches_its_own_provider_nested_and_in_a_task() {
    assert_both(
        "nested",
        NESTED,
        "outer reads 1\ninner reads 11\ns 0 t 0\ninner closes 11\nouter reads 1\ntask 0\n\
         outer closes 1\nend\n",
    );
}

/// `karac test` on a package whose test file declares a `Network` fake:
/// the tree check sees `NetworkProvider` and `NetError` (it read only the
/// production files' library before), the test runs on both executors,
/// and `karac build` of the package, which does not name the fake, keeps
/// the socket natives: its two `par` reads of real sockets do not conflict.
#[test]
fn provider_network_fake_in_a_test_file_leaves_the_build_alone() {
    let dir = Scratch::new("package");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.kara"),
        r#"pub fn peek(s: ref TcpStream) -> i64 with sends(Network) receives(Network) blocks suspends {
    let mut b = [0u8; 2];
    match s.read(b[..]) {
        Ok(n) => n,
        Err(_) => -1,
    }
}

fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let at = l.local_addr().unwrap();
    let a = TcpStream.connect(at).unwrap();
    let b = TcpStream.connect(at).unwrap();
    let (x, y) = {
        let c = l.accept().unwrap();
        let d = l.accept().unwrap();
        let _ = c.write_all("hi".bytes());
        let _ = d.write_all("yo".bytes());
        par { peek(a), peek(b) }
    };
    println(f"{x} {y}");
}
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/main_test.kara"),
        r#"struct Fake { }

impl NetworkProvider for Fake {
    fn connect(mut ref self, addr: Str) -> Result[i64, NetError] { Ok(1) }
    fn read_stream(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, NetError] { Ok(0) }
    fn close(mut ref self, h: i64) { }
}

#[with_provider(Network, Fake { })]
test "peek a fake" {
    let s = TcpStream.connect("x:1").unwrap();
    assert_eq(peek(s), 0);
}

test "refused names NetError" {
    match TcpStream.connect("127.0.0.1:1") {
        Err(NetError.ConnectionRefused) => {}
        _ => assert(false),
    }
}
"#,
    )
    .unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    for lane in [&[][..], &["--interp"][..]] {
        let out = Command::new(karac)
            .current_dir(root)
            .arg("test")
            .args(lane)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("\"total\":2,\"passed\":2"),
            "{lane:?}: {text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let built = Command::new(karac)
        .current_dir(root)
        .arg("build")
        .output()
        .unwrap();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    })
    .is_none()
    {
        return;
    }
    let printed = stdout_of(
        Command::new(root.join("pkg")).output().unwrap(),
        "package binary",
    );
    assert_eq!(printed, "2 2\n");
}
