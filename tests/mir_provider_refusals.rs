//! What an override cannot do, reported by `karac check` and refused by
//! every backend (core-semantics §14.11, §14.12): under an override of
//! `Network`, a call of UDP, TLS, WebSocket or a server, which no provider
//! serves (`E_PROVIDER_UNSUPPORTED`, E0421, B-2026-10-29-20), and a
//! `FileSystem` or `Network` provider's handle method that uses a resource
//! overridden where the provider is bound (E0422, B-2026-10-29-15). And
//! a coercion to `dyn` of a type whose methods reach a provider-rooted
//! resource, or one overridden where it is made (`E_DYN_PROVIDER`, E0423,
//! design.md §9): a `dyn` value has no room to carry a provider.

use std::process::{Command, Output, Stdio};

/// `karac <args..> <file>` on `src`, written to a scratch file.
fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-provider-refusals-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

/// `karac check --output=json` on `src`: each diagnostic's code, line,
/// column and message.
fn check(tag: &str, src: &str) -> Vec<(String, u64, u64, String)> {
    let out = karac(tag, &["check", "--output=json"], src);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tag}: {e}"));
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap_or("").to_string(),
                d["line"].as_u64().unwrap_or(0),
                d["column"].as_u64().unwrap_or(0),
                d["message"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// The interpreter refuses `src` before it runs, naming `want`.
fn run_refuses(tag: &str, src: &str, want: &str) {
    let out = karac(tag, &["__mir-run"], src);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: ran");
    assert!(out.stdout.is_empty(), "{tag}: printed");
    assert!(err.contains(want), "{tag}: {err}");
}

const SERVE: &str = r#"struct Fake { }
impl NetworkProvider for Fake {}

fn route(req: ref Request) -> Response { Response { status: 200, body: "ok".to_string() } }

fn main() {
    with_provider[Network](Fake { }, || {
        let _ = Server.serve("127.0.0.1:0", route);
    });
}
"#;

/// The row's repro: `Server.serve` inside `with_provider[Network]` would
/// bind a real listener. The check names the call and `Network`.
#[test]
fn provider_refuses_a_server_under_a_network_override() {
    let found = check("serve", SERVE);
    assert_eq!(found.len(), 1, "{found:?}");
    let (code, line, col, msg) = &found[0];
    assert_eq!((code.as_str(), *line, *col), ("E0421", 8, 17), "{msg}");
    assert!(
        msg.contains("`Server.serve` is not served by a `Network` provider"),
        "{msg}"
    );
    assert!(msg.contains("E_PROVIDER_UNSUPPORTED"), "{msg}");
    run_refuses("serve", SERVE, "`Server.serve` is not served");
}

const REACHED: &str = r#"struct Fake { }
impl NetworkProvider for Fake {}

fn ping() -> i64 with sends(Network) receives(Network) {
    match UdpSocket.bind("127.0.0.1:0") {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

fn main() {
    let outside = UdpSocket.bind("127.0.0.1:0");
    with_provider[Network](Fake { }, || {
        println(ping());
    });
    providers { Network => Fake { } } in {
        match outside {
            Ok(s) => {
                let _ = s.send_to([1u8].as_slice(), "127.0.0.1:9");
            }
            Err(_) => {}
        }
        let _ = TlsListener.bind_tls("127.0.0.1:0", "c", "k");
    }
}
"#;

/// A UDP or TLS call reached through a callee, or in a `providers` block,
/// on a socket opened outside every override too, is refused where it is
/// made; the same calls outside every override are not.
#[test]
fn provider_refuses_udp_and_tls_reached_under_a_network_override() {
    let found: Vec<(String, u64, u64)> = check("reached", REACHED)
        .into_iter()
        .map(|(c, l, k, m)| {
            assert!(m.contains("is not served by a `Network` provider"), "{m}");
            (c, l, k)
        })
        .collect();
    let want = [("E0421", 5, 11), ("E0421", 19, 25), ("E0421", 23, 17)];
    let want: Vec<(String, u64, u64)> = want
        .iter()
        .map(|(c, l, k)| (c.to_string(), *l, *k))
        .collect();
    assert_eq!(found, want);
    run_refuses("reached", REACHED, "E_PROVIDER_UNSUPPORTED");
}

const SERVED: &str = r#"struct Fake { }
impl NetworkProvider for Fake {
    fn connect(mut ref self, addr: Str) -> Result[i64, NetError] { Err(NetError.ConnectionRefused) }
}

fn tick() -> i64 with sends(Network) receives(Network) {
    match TcpStream.connect("127.0.0.1:1") {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

fn main() {
    match UdpSocket.bind("127.0.0.1:0") {
        Ok(_) => println("udp"),
        Err(_) => println("udp"),
    }
    with_provider[Network](Fake { }, || {
        println(tick());
        println(WebSocket.max_message() > 0);
    });
}
"#;

/// What a provider serves (TCP), a `WebSocket` function that does not
/// reach the network, and UDP outside every override all still check and
/// run.
#[test]
fn provider_network_override_keeps_what_it_serves() {
    let found = check("served", SERVED);
    assert!(found.is_empty(), "{found:?}");
    let out = karac("served", &["__mir-run"], SERVED);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "udp\n0\ntrue\n");
}

const FILE_HANDLER: &str = r#"struct MemFs { n: i64 }
impl FileSystemProvider for MemFs {
    fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] { Ok(1) }
    fn close(mut ref self, h: i64) { println(f"close {h}"); }
}
struct Cap { }
impl StdoutProvider for Cap { fn write(mut ref self, s: Str) { } }
fn main() {
    with_provider[Stdout](Cap { }, || {
        with_provider[FileSystem](MemFs { n: 0 }, || {
            let _ = File.open("a");
        });
    });
    providers { Stdout => Cap { } } in {
        providers { FileSystem => MemFs { n: 0 } } in {
            let _ = File.open("b");
        }
    }
}
"#;

/// B-2026-10-29-15's repro, with a `providers` block too: a file's `close`
/// runs with every other resource at its default, so one that prints under
/// a `Stdout` override is reported at each binding, naming the method and
/// the resource.
#[test]
fn provider_file_handle_method_using_an_override_is_checked() {
    let found = check("file-handler", FILE_HANDLER);
    let at: Vec<(&str, u64, u64)> = found
        .iter()
        .map(|(c, l, k, _)| (c.as_str(), *l, *k))
        .collect();
    assert_eq!(at, [("E0422", 10, 9), ("E0422", 15, 9)], "{found:?}");
    assert!(
        found[0].3.contains(
            "`MemFs.close` uses `Stdout`, which is overridden where the \
             `with_provider[FileSystem]` call is"
        ),
        "{}",
        found[0].3
    );
    assert!(
        found[1].3.contains("where the `providers` block is"),
        "{}",
        found[1].3
    );
    run_refuses(
        "file-handler",
        FILE_HANDLER,
        "`MemFs.close` uses `Stdout`, which is overridden",
    );
}

const SOCKET_HANDLER: &str = r#"struct Fake { }
impl NetworkProvider for Fake {
    fn connect(mut ref self, addr: Str) -> Result[i64, NetError] { Ok(7) }
    fn close(mut ref self, h: i64) { println(f"close {h}"); }
}
struct Cap { }
impl StdoutProvider for Cap { fn write(mut ref self, s: Str) { } }
fn main() {
    with_provider[Stdout](Cap { }, || {
        with_provider[Network](Fake { }, || {
            let _ = TcpStream.connect("127.0.0.1:1");
        });
    });
}
"#;

/// The same for a socket's `close` (B-2026-10-29-20's second half).
#[test]
fn provider_socket_handle_method_using_an_override_is_checked() {
    let found = check("socket-handler", SOCKET_HANDLER);
    assert_eq!(found.len(), 1, "{found:?}");
    let (code, line, col, msg) = &found[0];
    assert_eq!((code.as_str(), *line, *col), ("E0422", 10, 9), "{msg}");
    assert!(
        msg.contains("`Fake.close` uses `Stdout`, which is overridden where the `with_provider[Network]` call is; a socket's `close`"),
        "{msg}"
    );
    run_refuses(
        "socket-handler",
        SOCKET_HANDLER,
        "`Fake.close` uses `Stdout`",
    );
}

/// `karac build` refuses the same program before it links anything.
#[cfg(feature = "llvm")]
#[test]
fn provider_unsupported_call_refuses_the_build() {
    let dir = std::env::temp_dir().join(format!(
        "karac-provider-refusals-build-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (file, exe) = (dir.join("main.kara"), dir.join("main"));
    std::fs::write(&file, SERVE).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "built");
    assert!(err.contains("`Server.serve` is not served"), "{err}");
}

const DYN_OVERRIDDEN: &str = r#"trait Speak {
    fn speak(ref self);
}
struct Dog { }
impl Speak for Dog {
    fn speak(ref self) { println("woof"); }
}
struct Cap { }
impl StdoutProvider for Cap { fn write(mut ref self, s: Str) { } }

fn make() -> dyn Speak { Dog { } }

fn main() {
    with_provider[Stdout](Cap { }, || {
        let d: dyn Speak = Dog { };
        d.speak();
        let m = make();
        m.speak();
        let dog = Dog { };
        let r: ref dyn Speak = ref dog;
        r.speak();
    });
}
"#;

/// A `dyn` value made where `Stdout` is overridden would print through
/// the default, so the coercion is refused: owned, borrowed, and in a
/// callee the override reaches (`E_DYN_PROVIDER`, design.md §9).
#[test]
fn provider_refuses_a_dyn_made_under_an_override_it_would_use() {
    let found = check("dyn-overridden", DYN_OVERRIDDEN);
    let at: Vec<(&str, u64, u64)> = found
        .iter()
        .map(|(c, l, k, _)| (c.as_str(), *l, *k))
        .collect();
    assert_eq!(
        at,
        [("E0423", 11, 26), ("E0423", 15, 28), ("E0423", 20, 32)],
        "{found:?}"
    );
    for (_, _, _, msg) in &found {
        assert!(
            msg.contains(
                "`Dog` as a `dyn Speak` cannot carry a provider, and its `Dog.speak` uses \
                 `Stdout`, overridden where it is made, by a `with_provider[Stdout]`"
            ),
            "{msg}"
        );
        assert!(
            msg.contains("take a generic parameter (`[T: Speak]`)"),
            "{msg}"
        );
    }
    run_refuses("dyn-overridden", DYN_OVERRIDDEN, "E_DYN_PROVIDER");
}

const DYN_PROVIDER_ROOTED: &str = r#"effect resource Db: DbProvider;
trait DbProvider {
    fn get(ref self, k: i64) -> i64;
}
struct Mem { }
impl DbProvider for Mem {
    fn get(ref self, k: i64) -> i64 { k * 2 }
}
trait Look {
    fn look(ref self) -> i64;
}
struct L { k: i64 }
impl Look for L {
    fn look(ref self) -> i64 with reads(Db) { Db.get(self.k) }
}
fn main() {
    with_provider[Db](Mem { }, || {
        let l: dyn Look = L { k: 4 };
        println(l.look());
    });
}
"#;

/// A resource with no default has no provider a `dyn` value could use, so
/// the coercion is refused even inside its `with_provider`.
#[test]
fn provider_refuses_a_dyn_reaching_a_provider_rooted_resource() {
    let found = check("dyn-rooted", DYN_PROVIDER_ROOTED);
    assert_eq!(found.len(), 1, "{found:?}");
    let (code, line, col, msg) = &found[0];
    assert_eq!((code.as_str(), *line, *col), ("E0423", 18, 27), "{msg}");
    assert!(
        msg.contains("its `L.look` uses `Db`, which has no default"),
        "{msg}"
    );
    run_refuses("dyn-rooted", DYN_PROVIDER_ROOTED, "E_DYN_PROVIDER");
}

const DYN_DEFAULTS: &str = r#"trait Speak {
    fn speak(ref self);
}
struct Dog { }
impl Speak for Dog {
    fn speak(ref self) { println("woof"); }
}
struct Quiet { }
impl Speak for Quiet {
    fn speak(ref self) { }
}
struct Cap { }
impl StdoutProvider for Cap { fn write(mut ref self, s: Str) { } }

fn twice[T: Speak](t: ref T) { t.speak(); t.speak(); }

fn main() {
    let d: dyn Speak = Dog { };
    with_provider[Stdout](Cap { }, || {
        d.speak();
        let q: dyn Speak = Quiet { };
        q.speak();
        twice(Dog { });
    });
    println("after");
}
"#;

/// A `dyn` value made outside every override runs with the defaults
/// wherever it is called, one whose methods use no provider may be made
/// anywhere, and a generic parameter takes the override.
#[test]
fn provider_dyn_made_at_the_defaults_keeps_them() {
    let found = check("dyn-defaults", DYN_DEFAULTS);
    assert!(found.is_empty(), "{found:?}");
    let out = karac("dyn-defaults", &["__mir-run"], DYN_DEFAULTS);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "woof\nafter\n");
}
