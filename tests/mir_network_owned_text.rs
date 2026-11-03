//! `Client.get(url)` / `Client.post(url, body)` under a `Network` fake
//! with a `String` url or body (core semantics §14.12, io.md § HTTP
//! client, "Under an override"). The call passed `&url` to the library's
//! `__get(.., url: own String)`, which read the reference as the `String`
//! it owns: the interpreter stopped with "a String place holds no box",
//! a built binary crashed, and ASan reported a stack-buffer-overflow in
//! `Fh.__get` (B-2026-11-03-18). A literal url, which the call already
//! formatted into a `String`, worked. Pinned on the interpreter, a built
//! binary and the ASan lane.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-network-owned-text-{name}-{}",
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

const FAKE: &str = r#"struct Fh { n: i64 }
impl NetworkProvider for Fh {
    fn send(mut ref self, req: Request) -> Result[Response, HttpError] {
        self.n += 1;
        let text = f"{req.method()} {req.url()} [{req.body()}] #{self.n}";
        Ok(Response { status: 200, body: text })
    }
}
fn show(r: Result[Response, HttpError]) {
    match r {
        Ok(r) => println(f"{r.status()} {r.body()}"),
        Err(e) => println(e.message()),
    }
}
"#;

fn asan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    let dir = Scratch::new("asan-probe");
    let c = dir.path().join("probe.c");
    let exe = dir.path().join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=address")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

/// The program's stdout on the interpreter, as a built binary and on the
/// ASan lane, each where it can run here.
fn every_lane(name: &str, main: &str, want: &str) {
    let dir = Scratch::new(name);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, format!("{FAKE}{main}")).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();

    let out = Command::new(karac)
        .args(["__mir-run", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{name}: interpreter: {err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{name}: interpreter"
    );

    let exe = dir.path().join("main");
    let built = Command::new(karac)
        .args(["build", f, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_some() {
        let out = Command::new(&exe).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{name}: binary: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: binary");
    }

    if !asan_available() {
        eprintln!("{name}: skipping the ASan lane: no ASan-capable cc");
        return;
    }
    let out = Command::new(karac)
        .args(["__mir-llvm-asan", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "{name}: ASan:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: ASan");
}

#[test]
fn client_get_and_post_take_a_string_url_under_a_fake() {
    every_lane(
        "strings",
        r#"fn main() {
    with_provider[Network](Fh { n: 0 }, || {
        let c = Client.new();
        let url = "http://x/a".to_string();
        show(c.get(url));
        show(c.get(f"http://x/{2 + 1}"));
        let u = "http://x/p".to_string();
        let body = "payload".to_string();
        show(c.post(u, body));
        show(c.post("http://x/q", "lit".to_string()));
    });
}
"#,
        "200 GET http://x/a [] #1\n\
         200 GET http://x/3 [] #2\n\
         200 POST http://x/p [payload] #3\n\
         200 POST http://x/q [lit] #4\n",
    );
}

/// Control: literal texts, which worked before.
#[test]
fn client_get_and_post_take_literals_under_a_fake() {
    every_lane(
        "literals",
        r#"fn main() {
    with_provider[Network](Fh { n: 0 }, || {
        let c = Client.new();
        show(c.get("http://x/l"));
        show(c.post("http://x/m", "b"));
    });
}
"#,
        "200 GET http://x/l [] #1\n200 POST http://x/m [b] #2\n",
    );
}
