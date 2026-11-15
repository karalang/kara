//! docs/library/io.md § HTTP client, on both backends:
//!
//! - a 3xx the client does not follow (a 300, a 304, a 302 with no
//!   `Location`) is outside 2xx, so an `Err` that keeps the response, where
//!   it was an `Ok` (B-2026-11-15-134);
//! - a sixth redirect is not followed: the `Err` keeps that 3xx response
//!   and is `<asked-for url>: too many redirects`, where it dropped the
//!   response and named the last `Location` (B-2026-11-15-135);
//! - a `timeout` below 1 ms is an `Err` with no response, sent nowhere,
//!   where it meant no timeout at all (B-2026-11-15-136).

use std::io::{BufRead, BufReader, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A loopback server answering one request per connection, by its path,
/// counting the requests it was sent.
fn server() -> (u16, std::sync::Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    let seen = std::sync::Arc::new(AtomicUsize::new(0));
    let count = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let count = count.clone();
            std::thread::spawn(move || answer(conn, &count));
        }
    });
    (port, seen)
}

fn answer(conn: std::net::TcpStream, seen: &AtomicUsize) {
    let mut out = conn.try_clone().expect("clone");
    let mut r = BufReader::new(conn);
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    seen.fetch_add(1, Ordering::SeqCst);
    let target = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
    }
    let reply = |status: &str, extra: &str, body: &str| {
        format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let text = if let Some(k) = target.strip_prefix("/redir/") {
        match k.parse::<u32>().unwrap_or(0) {
            0 => reply("200 OK", "", "landed"),
            k => reply(
                "302 Found",
                &format!("Location: /redir/{}\r\nX-Hop: {k}\r\n", k - 1),
                "redirect body",
            ),
        }
    } else {
        match target.as_str() {
            "/nolocation" => reply("302 Found", "", "no location"),
            "/multiple" => reply("300 Multiple Choices", "Location: /redir/0\r\n", "choose"),
            "/notmodified" => reply("304 Not Modified", "ETag: \"v1\"\r\n", ""),
            _ => reply("404 Not Found", "", ""),
        }
    };
    let _ = out.write_all(text.as_bytes());
}

const SRC: &str = r#"
fn show(label: String, r: Result[Response, HttpError]) {
    match r {
        Ok(resp) => println(f"{label}: Ok {resp.status()} {resp.body()}"),
        Err(e) => println(f"{label}: Err {e.status()} loc={e.header("location")} hop={e.header("x-hop")} etag={e.header("etag")} body={e.body()} msg={e.message()}"),
    }
}

fn main() {
    let base = "http://127.0.0.1:PORT";
    let c = Client.new();
    show("five", c.get(f"{base}/redir/5"));
    show("six", c.get(f"{base}/redir/6"));
    show("nolocation", c.get(f"{base}/nolocation"));
    show("300", c.get(f"{base}/multiple"));
    show("304", c.get(f"{base}/notmodified"));
    show("t0", c.request("GET", f"{base}/redir/0").timeout(0).send());
    show("tneg", c.request("GET", f"{base}/redir/0").timeout(-5).send());
    show("t1", c.request("GET", f"{base}/redir/0").timeout(5000).send());
}
"#;

fn want(port: u16) -> String {
    let base = format!("http://127.0.0.1:{port}");
    format!(
        "five: Ok 200 landed\n\
         six: Err Some(302) loc=Some(/redir/0) hop=Some(1) etag=None body=Some(redirect body) \
         msg={base}/redir/6: too many redirects\n\
         nolocation: Err Some(302) loc=None hop=None etag=None body=Some(no location) \
         msg={base}/nolocation: status code 302\n\
         300: Err Some(300) loc=Some(/redir/0) hop=None etag=None body=Some(choose) \
         msg={base}/multiple: status code 300\n\
         304: Err Some(304) loc=None hop=None etag=Some(\"v1\") body=Some() \
         msg={base}/notmodified: status code 304\n\
         t0: Err None loc=None hop=None etag=None body=None \
         msg={base}/redir/0: timeout must be at least 1 ms\n\
         tneg: Err None loc=None hop=None etag=None body=None \
         msg={base}/redir/0: timeout must be at least 1 ms\n\
         t1: Ok 200 landed\n"
    )
}

fn check(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let (port, seen) = server();
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-3xx-floor-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC.replace("PORT", &port.to_string())).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want(port), "{mode}");
    // Six requests for `five` (five redirects, then the 200), six for
    // `six` (its sixth 302 is the answer), and one each for the 302, 300,
    // 304 and `t1`: the refused timeouts sent nothing.
    assert_eq!(seen.load(Ordering::SeqCst), 6 + 6 + 4, "{mode}");
}

#[test]
fn interp_unfollowed_3xx_redirect_cap_and_timeout_floor() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_unfollowed_3xx_redirect_cap_and_timeout_floor() {
    check("__mir-llvm-run");
}
