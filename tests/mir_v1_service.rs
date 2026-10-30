//! The v1 integration pin: the three features v1 adds to services, tested
//! together in one package (`tests/fixtures/v1_service`). A router of
//! `dyn Handler`s, one of which suspends on a TCP read; an
//! `EventBus[with E]` of stored callbacks whose effects are charged where it
//! emits; and `test` blocks running both under `FileSystem` and `Network`
//! fakes (`#[with_provider]`). The program runs alike on the MIR
//! interpreter, the compiled backend and its ASan lane, at one worker and
//! four; its tests pass on both `karac test` executors; and a router of
//! `dyn` handlers made under a fake is refused (`E_DYN_PROVIDER`, E0423).

#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const WANT: &str = "0  -> 200 ok
event route=0 status=200
1 pear -> 200 5
event route=1 status=200
2 apple -> 200 12
event route=2 status=200
1 plum -> 404 no plum
event route=1 status=404
7 x -> 400 no route
event route=7 status=400
ok 3
stock served 1
";

/// A copy of the package in a scratch directory, removed on drop: `main`
/// writes its catalog file in the working directory.
struct Package(PathBuf);

impl Package {
    fn new(tag: &str) -> Package {
        let dir =
            std::env::temp_dir().join(format!("karac-v1-service-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1_service");
        for f in ["kara.toml", "src/main.kara", "src/main_test.kara"] {
            std::fs::copy(from.join(f), dir.join(f)).unwrap();
        }
        Package(dir)
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn karac(dir: &Path, threads: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_THREADS", threads)
        .env_remove("KARAC_TEST_JIT")
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Whether `cc -fsanitize=address` links and runs here.
fn asan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    let dir = Package::new("asan-probe");
    let c = dir.0.join("probe.c");
    let exe = dir.0.join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").unwrap();
    Command::new("cc")
        .arg("-fsanitize=address")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

#[test]
fn v1_service_runs_alike_on_every_backend() {
    let pkg = Package::new("run");
    let mut backends = vec!["__mir-run", "__mir-llvm-run"];
    if asan_available() {
        backends.push("__mir-llvm-asan");
    } else {
        eprintln!("skipping the ASan lane: no ASan-capable cc");
    }
    for threads in ["1", "4"] {
        for b in &backends {
            let out = karac(&pkg.0, threads, &[b, "src/main.kara"]);
            let err = String::from_utf8_lossy(&out.stderr);
            if *b != "__mir-run" && err.contains("Linker failed") {
                // A stale archive panics here; a missing one skips.
                let _ = common::link_or_skip(Err(err.into_owned()));
                return;
            }
            assert_eq!(
                (
                    out.status.code(),
                    String::from_utf8_lossy(&out.stdout).as_ref()
                ),
                (Some(0), WANT),
                "{b} at KARAC_THREADS={threads}, stderr:\n{err}"
            );
        }
    }
}

/// The summary line of a `karac test` run.
fn summary(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find(|l| l.starts_with("{\"type\":\"summary\""))
        .unwrap_or_else(|| {
            panic!(
                "no summary: {stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
        .to_string()
}

#[test]
fn v1_service_tests_pass_under_fakes() {
    let pkg = Package::new("test");
    for threads in ["1", "4"] {
        for lane in [&["test"][..], &["test", "--interp"][..]] {
            let out = karac(&pkg.0, threads, lane);
            let s = summary(&out);
            assert!(
                s.contains("\"total\":4,\"passed\":4,\"failed\":0"),
                "{lane:?} at KARAC_THREADS={threads}: {s}\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }
}

/// The fakes are what the cases reach: a wrong expectation of what each
/// fake answers fails the case that reads it, on both executors.
#[test]
fn v1_service_fakes_answer_what_the_tests_read() {
    let pkg = Package::new("control");
    let test = pkg.0.join("src/main_test.kara");
    let text = std::fs::read_to_string(&test).unwrap();
    let wrong = text
        .replace("assert_eq(r.body, \"41\");", "assert_eq(r.body, \"42\");")
        .replace(
            "\"served\").unwrap(), \"1\")",
            "\"served\").unwrap(), \"2\")",
        );
    assert_ne!(wrong, text);
    std::fs::write(&test, wrong).unwrap();
    for lane in [&["test"][..], &["test", "--interp"][..]] {
        let out = karac(&pkg.0, "1", lane);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            summary(&out).contains("\"total\":4,\"passed\":2,\"failed\":2"),
            "{lane:?}: {stdout}"
        );
        for name in [
            "stock asks the fake network",
            "serve runs under the fake, its bus charged where it emits",
        ] {
            let key = format!("\"type\":\"test_fail\",\"test\":\"{name}\"");
            assert!(stdout.contains(&key), "{lane:?} {name}: {stdout}");
        }
    }
}

/// A `dyn Handler` made under the `Network` fake has no room for it: the
/// run is refused at the coercion, naming the generic parameter to take.
#[test]
fn v1_service_dyn_router_under_a_fake_is_refused() {
    let pkg = Package::new("dyn");
    let test = pkg.0.join("src/main_test.kara");
    let mut text = std::fs::read_to_string(&test).unwrap();
    let line = text.lines().count() + 5;
    text.push_str(
        "\n#[with_provider(Network, FakeStock.new)]
test \"a router of dyn handlers under the fake\" {
    let mut routes: Vec[dyn Handler] = Vec.new();
    routes.push(Stock { addr: \"stock:1\".to_string() });
    let router = Router { routes: routes };
    let r = router.dispatch(Req { route: 0, body: \"apple\".to_string() });
    assert_eq(r.status, 200);
}
",
    );
    std::fs::write(&test, text).unwrap();
    for lane in [&["test"][..], &["test", "--interp"][..]] {
        let out = karac(&pkg.0, "1", lane);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_ne!(out.status.code(), Some(0), "{lane:?}: {stdout}");
        assert!(
            stdout.contains(&format!(
                "\"code\":\"E0423\",\"category\":\"effects\",\"file\":\"src/main_test.kara\",\"line\":{line},"
            )),
            "{lane:?}: {stdout}"
        );
        assert!(
            stdout.contains("`Stock` as a `dyn Handler` cannot carry a provider")
                && stdout.contains("take a generic parameter (`[T: Handler]`)"),
            "{lane:?}: {stdout}"
        );
        assert!(!stdout.contains("test_pass"), "{lane:?}: {stdout}");
    }
}

/// A callback that uses the fake cannot be stored on the bus: it would
/// keep a view of the provider (core semantics §14.5, §14.7). The refusal
/// names the test file and its line, not the package's combined source
/// (B-2026-10-30-8).
#[test]
fn v1_service_stored_callback_under_a_fake_names_its_file() {
    let pkg = Package::new("stored");
    let test = pkg.0.join("src/main_test.kara");
    let mut text = std::fs::read_to_string(&test).unwrap();
    let line = text.lines().count() + 5;
    text.push_str(
        "\n#[with_provider(FileSystem, MemFs.with_catalog)]
test \"a stored callback that uses the fake\" {
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(|e| {
        let _ = fs.write(f\"event-{e.route}\", f\"{e.status}\");
    });
    bus.emit(Event { route: 3, status: 404 });
}
",
    );
    std::fs::write(&test, text).unwrap();
    for lane in [&["test"][..], &["test", "--interp"][..]] {
        let out = karac(&pkg.0, "1", lane);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_ne!(out.status.code(), Some(0), "{lane:?}: {stdout}");
        assert!(
            stdout.contains(&format!("src/main_test.kara:{line}:")),
            "{lane:?}: {stdout}"
        );
        assert!(!stdout.contains("test_pass"), "{lane:?}: {stdout}");
    }
}
