//! A socket error the interpreter's readiness probe takes off a socket is
//! the error the read reports (B-2026-11-10-15).
//!
//! A read that waits peeks the socket without blocking to learn when it is
//! ready. A reset (TCP) or a refused datagram (connected UDP) is a pending
//! error the peek consumes, so the read that followed used to find nothing
//! wrong: it read an end of stream, or waited out its timeout, where a
//! compiled build reports the error.

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn run(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "karac-interp-socket-errors-i-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(feature = "llvm")]
fn compiled(src: &str) -> Option<String> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!(
        "karac-interp-socket-errors-b-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).current_dir(&dir).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    assert!(out.status.success(), "{:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The accepted side closes with the client's line unread, so the kernel
/// resets the connection: the client reads the answer, then the reset.
const RESET: &str = r#"fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let _ = c.write_all("data\n".bytes());
    let _ = c.shutdown_write();
    {
        let conn = l.accept().unwrap();
        let _ = conn.write_all("503\n".bytes());
    }
    let mut buf = [0u8; 64];
    loop {
        match c.read(mut buf) {
            Ok(0) => {
                println("eof");
                break;
            }
            Ok(n) => println(f"got {n}"),
            Err(e) => {
                println(f"err {e}");
                break;
            }
        }
    }
}
"#;

const RESET_OUT: &str = "got 4\nerr os error 104\n";

/// A datagram to a port nobody holds comes back refused, which a
/// connected socket's next `recv` reports.
const REFUSED: &str = r#"fn main() {
    let gone = UdpSocket.bind("127.0.0.1:0").unwrap();
    let addr = gone.local_addr().unwrap();
    drop(gone);
    let s = UdpSocket.bind("127.0.0.1:0").unwrap();
    let _ = s.set_read_timeout(Some(2000));
    let _ = s.connect(addr);
    let _ = s.send("ping".bytes());
    let mut buf = [0u8; 16];
    match s.recv(mut buf) {
        Ok(n) => println(f"got {n}"),
        Err(e) => println(f"err {e}"),
    }
}
"#;

const REFUSED_OUT: &str = "err connection refused\n";

#[test]
fn interp_reads_a_reset_the_readiness_peek_took() {
    assert_eq!(run(RESET), RESET_OUT);
}

#[test]
fn interp_recv_reports_a_refusal_the_readiness_peek_took() {
    assert_eq!(run(REFUSED), REFUSED_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_reads_a_reset_as_the_interp_does() {
    if let Some(out) = compiled(RESET) {
        assert_eq!(out, RESET_OUT);
    }
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_recv_reports_a_refusal_as_the_interp_does() {
    if let Some(out) = compiled(REFUSED) {
        assert_eq!(out, REFUSED_OUT);
    }
}
