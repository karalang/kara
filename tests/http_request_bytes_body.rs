//! A request body that is bytes (docs/library/io.md § HTTP client,
//! B-2026-11-25-80): `RequestBuilder.body_bytes` and `Request.with_bytes`
//! send a `Vec[u8]` as it is, `Content-Length` its count and no
//! `Content-Type` added, on both backends and under a `Network` override.
//! Before them `body` took text only, so a client could not upload a
//! binary object. Also `String.eq_ignore_ascii_case` (B-2026-11-25-81).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Answers each request with `cl=<Content-Length> ct=<Content-Type or
/// none> hex=<the body's bytes in hex>`.
fn echo_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut out = conn.try_clone().expect("clone");
                let mut r = BufReader::new(conn);
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let (mut cl, mut ct) = (None, "none".to_string());
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap_or((&h, ""));
                    match k.trim().to_ascii_lowercase().as_str() {
                        "content-length" => cl = v.trim().parse::<usize>().ok(),
                        "content-type" => ct = v.trim().to_string(),
                        _ => {}
                    }
                }
                let mut body = vec![0u8; cl.unwrap_or(0)];
                let _ = r.read_exact(&mut body);
                let hex: String = body.iter().map(|b| format!("{b:02x}")).collect();
                let text = format!("cl={} ct={ct} hex={hex}", cl.map_or(-1, |n| n as i64));
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = out.write_all(resp.as_bytes());
            });
        }
    });
    port
}

fn run(mode: &str, tag: &str, src: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("karac-req-bytes-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{mode} {tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const CLIENT_SRC: &str = r#"
fn show(r: Result[Response, HttpError]) {
    match r {
        Ok(resp) => println(resp.body()),
        Err(e) => println(f"err {e.message()}"),
    }
}

fn main() {
    let c = Client.new();
    let small: Vec[u8] = [255u8, 0u8, 65u8].to_vec();
    let mut blob: Vec[u8] = Vec.new();
    for i in 0..256 {
        blob.push(i as u8);
    }
    show(c.request("PUT", "http://127.0.0.1:PORT/a").body_bytes(small).send());
    show(c.request("PUT", "http://127.0.0.1:PORT/b").body_bytes(blob).send());
    show(c.request("POST", "http://127.0.0.1:PORT/c").body("text").body_bytes([1u8, 2u8].to_vec()).send());
    show(c.request("POST", "http://127.0.0.1:PORT/d").body_bytes([1u8, 2u8].to_vec()).body("xy").send());
}
"#;

fn client_sends_bytes(mode: &str) {
    let port = echo_server();
    let got = run(
        mode,
        "client",
        &CLIENT_SRC.replace("PORT", &port.to_string()),
    );
    let blob: String = (0..=255u8).map(|b| format!("{b:02x}")).collect();
    let want = format!(
        "cl=3 ct=none hex=ff0041\ncl=256 ct=none hex={blob}\ncl=2 ct=none hex=0102\ncl=2 ct=none hex=7879\n"
    );
    assert_eq!(got, want, "{mode}");
}

#[test]
fn interp_body_bytes_sends_the_bytes_as_they_are() {
    client_sends_bytes("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_body_bytes_sends_the_bytes_as_they_are() {
    client_sends_bytes("__mir-llvm-run");
}

const BUILT_SRC: &str = r#"
struct Echo { n: i64 }

impl NetworkProvider for Echo {
    fn send(mut ref self, req: Request) -> Result[Response, HttpError] {
        self.n = self.n + 1;
        let raw = req.bytes();
        let mut t = f"{req.method()} n={raw.len()}:";
        for b in raw {
            t = t + f" {b}";
        }
        Ok(Response { status: 200, body: t })
    }
}

fn main() {
    with_provider[Network](Echo { n: 0 }, || {
        let c = Client.new();
        match c.request("PUT", "http://api.test/o").body_bytes([255u8, 0u8, 7u8].to_vec()).send() {
            Ok(r) => println(r.body()),
            Err(e) => println(e.message()),
        }
    });
    let req = Request.new("POST", "/x").with_bytes([255u8, 65u8].to_vec());
    let b = req.bytes();
    let mut pts = "";
    for ch in req.body().chars() {
        pts = f"{pts} {ch as u32}";
    }
    println(f"built n={b.len()} {b[0]} {b[1]} body{pts}");
    let req2 = Request.new("POST", "/y").with_body("ab").with_bytes([1u8].to_vec());
    let req3 = Request.new("POST", "/z").with_bytes([1u8].to_vec()).with_body("ab");
    println(f"last {req2.bytes().len()} {req3.body()}");
}
"#;

const BUILT_WANT: &str = "PUT n=3: 255 0 7\nbuilt n=2 255 65 body 65533 65\nlast 1 ab\n";

#[test]
fn interp_with_bytes_and_an_override_keep_the_bytes() {
    assert_eq!(run("__mir-run", "built", BUILT_SRC), BUILT_WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_with_bytes_and_an_override_keep_the_bytes() {
    assert_eq!(run("__mir-llvm-run", "built", BUILT_SRC), BUILT_WANT);
}

const EQ_SRC: &str = r#"
fn main() {
    let a = "Content-Type";
    let s: String = "CONTENT-type".to_string();
    println(f"{a.eq_ignore_ascii_case("content-type")} {s.eq_ignore_ascii_case(a)} {"É".eq_ignore_ascii_case("é")} {"ab".eq_ignore_ascii_case("abc")} {"x-Amz".eq_ignore_ascii_case("X-AMZ")}");
}
"#;

#[test]
fn interp_str_eq_ignore_ascii_case_is_rusts() {
    assert_eq!(
        run("__mir-run", "eq", EQ_SRC),
        "true true false false true\n"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_str_eq_ignore_ascii_case_is_rusts() {
    assert_eq!(
        run("__mir-llvm-run", "eq", EQ_SRC),
        "true true false false true\n"
    );
}

fn check_and_fix(tag: &str, src: &str) -> (String, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-req-bytes-fix-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = |cmd: &str| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .current_dir(&dir)
            .output()
            .unwrap()
    };
    let before = karac("check");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&before.stdout),
        String::from_utf8_lossy(&before.stderr)
    );
    let _ = karac("fix");
    let after = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (said, after)
}

#[test]
fn a_vec_u8_given_to_body_is_fixed_to_body_bytes() {
    let (said, after) = check_and_fix(
        "vec",
        "fn main() {\n    let c = Client.new();\n    let v: Vec[u8] = [1u8, 2u8].to_vec();\n    let _ = c.request(\"PUT\", \"http://127.0.0.1:1/\").body(v).send();\n}\n",
    );
    assert!(said.contains("bytes go in with `body_bytes`"), "{said}");
    assert!(after.contains(".body_bytes(v).send()"), "{after}");
}

#[test]
fn a_byte_slice_given_to_body_names_body_bytes_and_to_vec() {
    let (said, _) = check_and_fix(
        "slice",
        "fn send_it(c: ref Client, v: Slice[u8]) {\n    let _ = c.request(\"PUT\", \"http://127.0.0.1:1/\").body(v).send();\n}\n\nfn main() {\n    let c = Client.new();\n    let v: Vec[u8] = [1u8].to_vec();\n    send_it(c, v.as_slice());\n}\n",
    );
    assert!(said.contains("write `.body_bytes(x.to_vec())`"), "{said}");
}
