//! A task the handler spawns reads the request the handler is answering
//! (B-2026-11-20-54). The served request lived in per-task state, so `req.path()`
//! inside `group.spawn(..)` read an empty request: "" for the path, no
//! headers, no body. The handler is now given the request as a value of
//! the program's own, as `Request.new` makes one.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SRC: &str = r#"
import std.process.{exit};

fn handle(req: Request) -> Response {
    let g = TaskGroup.new();
    let h = g.spawn(|| {
        let mut q = "";
        for (k, v) in req.query() {
            q = f"{q}{k}={v};";
        }
        let tag = req.header("x-tag").unwrap_or("-");
        f"{req.method()} {req.path()} q={q} tag={tag} body={req.body()}"
    });
    let inner = h.join();
    Response { status: 200, body: f"outer={req.path()} inner=[{inner}]" }
}

fn main() {
    let g = TaskGroup.new();
    let _s = g.spawn(|| { let _ = Server.serve("127.0.0.1:PORT", |r| handle(r)); });
    sleep_ms(300);
    let c = Client.new();
    match c.request("POST", "http://127.0.0.1:PORT/abc?x=1").header("X-Tag", "t7").body("hi").send() {
        Ok(r) => println(r.body),
        Err(e) => println(e.message()),
    }
    exit(0);
}
"#;

const WANT: &str = "outer=/abc inner=[POST /abc q=x=1; tag=t7 body=hi]\n";

fn check(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("port").port()
    };
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-served-task-{}-{n}", std::process::id()));
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
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT, "{mode}");
}

#[test]
fn interp_task_spawned_by_a_handler_reads_its_request() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_task_spawned_by_a_handler_reads_its_request() {
    check("__mir-llvm-run");
}
