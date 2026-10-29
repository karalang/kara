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
        NetError.Unsupported => "unsupported".to_string(),
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
/// and `try_clone` on a fake's socket is `Unsupported`.
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
        NetError.Unsupported => "unsupported".to_string(),
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
        "write 8 2\nclone unsupported\nclose 8\nread timed out\nclone unsupported\nclose 7\n\
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

/// The HTTP client under an override: `get`, `post` and a built request's
/// `send` reach the provider's `send` with a `Request` it can read, and a
/// handler takes a `Request` a program built itself.
#[test]
fn provider_network_send_takes_the_http_client() {
    assert_both(
        "http",
        r#"struct Recorder { seen: i64 }

impl NetworkProvider for Recorder {
    fn send(mut ref self, req: Request) -> Result[Response, HttpError] {
        self.seen = self.seen + 1;
        let mut text = f"{req.method()} {req.url()} path={req.path()}";
        for (k, v) in req.query() {
            text = text + f" q:{k}={v}";
        }
        for (k, v) in req.headers() {
            text = text + f" h:{k}={v}";
        }
        text = text + f" body={req.body()} n={self.seen}";
        if req.path() == "/fail" {
            return Err(HttpError { message: "down".to_string() });
        }
        Ok(Response { status: 200, body: text })
    }
}

fn show(r: Result[Response, HttpError]) {
    match r {
        Ok(resp) => println(f"{resp.status()} {resp.body()}"),
        Err(e) => println(f"err {e.message()}"),
    }
}

fn handler(req: ref Request) -> Response {
    Response { status: 201, body: f"handled {req.method()} {req.path()} {req.body()}" }
}

fn main() {
    with_provider[Network](Recorder { seen: 0 }, || {
        let c = Client.new();
        show(c.get("http://api.test/items?id=7&q=a+b"));
        show(c.post("http://api.test/items", "{\"x\":1}"));
        show(c.request("PUT", "https://api.test").header("X-Key", "k1").body("pp").send());
        show(c.get("http://api.test/fail"));
    });
    let req = Request.new("DELETE".to_string(), "/items/3?force=1".to_string()).with_header("A".to_string(), "b".to_string()).with_body("bye".to_string());
    let resp = handler(req);
    println(f"{resp.status} {resp.body} url={req.url()} q={req.query().len()} h={req.header("a".to_string())}");
}
"#,
        r#"200 GET http://api.test/items?id=7&q=a+b path=/items q:id=7 q:q=a b body= n=1
200 POST http://api.test/items path=/items body={"x":1} n=2
200 PUT https://api.test path=/ h:X-Key=k1 body=pp n=3
err down
201 handled DELETE /items/3 bye url=/items/3?force=1 q=1 h=Some(b)
"#,
    );
}

/// B-2026-10-29-26: a `Request` a program builds frees its runtime entry
/// when it drops, so a client that builds requests in a loop runs in
/// constant memory. 400000 of them reached 108 MB of resident memory
/// before; the bound leaves room for the runtime and the allocator.
#[cfg(target_os = "linux")]
#[test]
fn provider_network_built_requests_are_freed() {
    let src = "fn main() {\n\
                   let mut n = 0;\n\
                   for i in 0..ROUNDS {\n\
                       let r = Request.new(\"GET\".to_string(), f\"http://api.test/items/{i}?page=1\".to_string()).with_body(\"x\".to_string());\n\
                       n = n + r.url().len();\n\
                   }\n\
                   println(n);\n\
               }\n";
    let dir = Scratch::new("built_requests");
    let (interp, _) = interp_and_build(dir.path(), &src.replace("ROUNDS", "2000"));
    assert_eq!(interp, "64890\n");
    let file = dir.path().join("loop.kara");
    std::fs::write(&file, src.replace("ROUNDS", "400000")).unwrap();
    let exe = dir.path().join("loop");
    let karac = env!("CARGO_BIN_EXE_karac");
    let built = Command::new(karac)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
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
    // `wait4` below reaps it, with its own resource usage.
    #[allow(clippy::zombie_processes)]
    let child = Command::new(&exe).stdout(Stdio::null()).spawn().unwrap();
    let mut status = 0;
    // SAFETY: an all-zero `rusage` is a valid value, and `wait4` reaps the
    // child it is given, which nothing else waits for.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let pid = unsafe { libc::wait4(child.id() as i32, &mut status, 0, &mut usage) };
    assert_eq!(pid, child.id() as i32);
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    assert!(
        usage.ru_maxrss < 40_000,
        "400000 built requests reached {} KB",
        usage.ru_maxrss
    );
}
