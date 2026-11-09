//! B-2026-11-09-34, found building the tus_upload app: a program that
//! declares its own `Response` in the shape io.md § HTTP server documents
//! (`status`, `body`, `headers: Vec[(String, String)]`) shares that type
//! with the client, so `Client.get` and `RequestBuilder.send` return it too.
//! The compiled client wrote only the library's two fields, at indices 0 and
//! 1, so the program's `headers` was never written and its drop freed a
//! garbage pointer (`free(): invalid pointer`, or a segfault), and a
//! reordered declaration read the status from the body. Both backends now
//! place the fields by name and fill `headers` with the response's headers.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

/// A server on a free loopback port answering `n` requests with a 200,
/// a body naming the request line's method, and two headers.
fn server(n: usize) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().take(n) {
            let Ok(mut s) = s else { continue };
            let mut buf = [0u8; 4096];
            let mut got = Vec::new();
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => got.extend_from_slice(&buf[..k]),
                }
            }
            let line = String::from_utf8_lossy(&got);
            let method = line.split(' ').next().unwrap_or("").to_string();
            let body = format!("hello {method}");
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Why: offset\r\nX-Count: 2\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(reply.as_bytes());
        }
    });
    port
}

fn program(port: u16) -> String {
    format!(
        "struct Response {{
    headers: Vec[(String, String)],
    body: String,
    status: i64,
}}

fn show(label: Str, r: Result[Response, HttpError]) {{
    match r {{
        Ok(resp) => {{
            let mut xs: Vec[String] = resp.headers.iter().filter(|(k, _)| k.to_lowercase().starts_with(\"x-\")).map(|(k, v)| f\"{{k.to_lowercase()}}={{v}}\").collect();
            xs.sort();
            let why = resp.header(\"X-Why\".to_string()) ?? \"-\".to_string();
            println(f\"{{label}}: {{resp.status}} {{resp.status()}} [{{resp.body}}] [{{resp.body()}}] {{xs.join(\";\")}} {{why}} {{resp.headers().len() == resp.headers.len()}}\");
        }}
        Err(e) => println(f\"{{label}}: error {{e.message()}}\"),
    }}
}}

fn main() {{
    let c = Client.new();
    show(\"get\", c.get(\"http://127.0.0.1:{port}/a\".to_string()));
    show(\"send\", c.request(\"PATCH\".to_string(), \"http://127.0.0.1:{port}/b\".to_string()).send());
    let own = Response {{ headers: Vec.new(), body: \"mine\".to_string(), status: 7 }};
    println(f\"own: {{own.status}} {{own.body}} {{own.headers.len()}}\");
}}
"
    )
}

const WANT: &str = "get: 200 200 [hello GET] [hello GET] x-count=2;x-why=offset offset true
send: 200 200 [hello PATCH] [hello PATCH] x-count=2;x-why=offset offset true
own: 7 mine 0
";

#[test]
fn a_client_response_fills_a_programs_own_response_by_name_interpreted() {
    let out = karac::mir::lower::run_source(&program(server(2)))
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn a_client_response_fills_a_programs_own_response_by_name_compiled() {
    let src = program(server(2));
    let lowered = karac::mir::lower::build_source(&src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-own-response-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let Some(out) = out else { return };
    assert!(
        out.status.success(),
        "{:?}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT);
}
