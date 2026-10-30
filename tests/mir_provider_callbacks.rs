//! Closures that reach a provider and are run by someone other than their
//! maker, under `with_provider` (core semantics §14.3, §14.5, §14.9): a
//! `Fn` value that writes the provider (B-2026-10-30-3); a task, a sort's
//! comparison, a `retain` predicate and a `par` branch, which the runtime
//! runs (B-2026-10-30-5); a task that writes the provider, refused until
//! the check keeps tasks from racing on it (B-2026-10-30-6); and a stored
//! callback, which would keep a view of the provider (§14.5).

use std::process::{Command, Output, Stdio};

const CLK: &str = "effect resource Clk: ClkP;
trait ClkP { fn now(ref self) -> i64; fn bump(mut ref self) {} }
struct F { t: i64 }
impl ClkP for F {
    fn now(ref self) -> i64 { self.t }
    fn bump(mut ref self) { self.t = self.t + 1; }
}
";

/// `karac <backend> <file>` on `CLK` followed by `src`.
fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-provider-callbacks-{tag}-{backend}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, format!("{CLK}{src}")).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_THREADS", "4")
        .arg(backend)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["check", "__mir-run", "__mir-llvm-run"]
    } else {
        &["check", "__mir-run"]
    }
}

/// `src` checks and prints `want` on every backend.
fn runs(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        // `karac check` reports to stderr and prints nothing on success.
        let want = if *b == "check" { "" } else { want };
        assert_eq!(
            (out.status.code(), stdout.as_ref()),
            (Some(0), want),
            "{tag} on {b}: {stderr}"
        );
    }
}

/// `src` checks, and every backend refuses it naming `why`.
fn refused(tag: &str, src: &str, why: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        if *b == "check" {
            assert!(out.status.success(), "{tag}: {stdout}{stderr}");
            continue;
        }
        assert!(!out.status.success(), "{tag} on {b}: ran: {stdout}");
        assert!(stdout.is_empty(), "{tag} on {b}: printed {stdout}");
        assert!(stderr.contains(why), "{tag} on {b}: {stderr}");
    }
}

/// A `Fn` value lends its environment shared, and one that bumps the clock
/// still writes the provider it carries, between direct uses of it.
#[test]
fn provider_fn_value_writes_the_provider() {
    runs(
        "fn-writes",
        "fn twice(g: Fn() -> i64) -> i64 { g() + g() }
fn apply[G: Fn() -> i64](g: G) -> i64 { g() }
fn main() {
    let r = with_provider[Clk](F { t: 0 }, || twice(|| { Clk.bump(); Clk.now() }));
    let s = with_provider[Clk](F { t: 10 }, || {
        let g = || { Clk.bump(); Clk.now() };
        let a = twice(g);
        Clk.bump();
        a + twice(g) + Clk.now()
    });
    let t = with_provider[Clk](F { t: 20 }, || {
        let a = apply(|| { Clk.bump(); Clk.now() });
        a + Clk.now()
    });
    println(f\"{r} {s} {t}\");
}
",
        "3 67 42\n",
    );
}

/// Tasks that read the provider share it (§14.9): the spawned closures,
/// one through a function, carry it.
#[test]
fn provider_tasks_read_the_provider() {
    runs(
        "tasks-read",
        "fn tick() -> i64 { Clk.now() }
fn main() {
    let r = with_provider[Clk](F { t: 4 }, || {
        let tg = TaskGroup.new();
        let a = tg.spawn(|| tick() + 1);
        let b = tg.spawn(|| Clk.now() * 2);
        a.join() + b.join()
    });
    println(f\"{r}\");
}
",
        "13\n",
    );
}

/// A comparison, a predicate and `par` branches the runtime runs reach
/// the provider bound where they are made.
#[test]
fn provider_reached_from_natives_and_par() {
    runs(
        "natives",
        "fn main() {
    let r = with_provider[Clk](F { t: 3 }, || {
        let mut v = [5, 1, 4];
        v.sort_by(|a, b| (a * Clk.now()).cmp(b * Clk.now()));
        let mut w = [2, 9, 7];
        w.retain(|x| x > Clk.now());
        let (p, q) = par { Clk.now(), Clk.now() + 1 };
        f\"{v} {w} {p} {q}\"
    });
    println(r);
}
",
        "[1, 4, 5] [9, 7] 3 4\n",
    );
}

/// Two tasks bumping the clock would race on the provider; the check does
/// not refuse them yet (PREDICTS B-2026-10-30-6), so every backend does.
#[test]
fn provider_tasks_writing_the_provider_are_refused() {
    refused(
        "tasks-write",
        "fn both(g: Fn() -> i64) -> i64 {
    let tg = TaskGroup.new();
    let a = tg.spawn(|| g());
    let b = tg.spawn(|| g());
    a.join() + b.join()
}
fn main() {
    let r = with_provider[Clk](F { t: 0 }, || {
        let tg = TaskGroup.new();
        let a = tg.spawn(|| { Clk.bump(); Clk.now() });
        a.join() + both(|| { Clk.bump(); Clk.now() })
    });
    println(f\"{r}\");
}
",
        "a task that writes `Clk`'s provider is not run yet",
    );
}

/// A callback stored on a bus, made where `Clk` is bound by the caller's
/// `with_provider`, would keep a view of the provider (§14.5). The check
/// misses it, since the binding is not in sight (PREDICTS
/// B-2026-10-30-7); the borrow checker refuses the store.
#[test]
fn provider_stored_callback_is_refused() {
    refused(
        "stored",
        "struct Bus[with E] { hs: Vec[Fn() -> i64 with E] }
impl[with E] Bus[with E] {
    fn reg(mut ref self, h: escaping Fn() -> i64 with E) { self.hs.push(h); }
    fn run(ref self) -> i64 with E { let mut s = 0; for h in self.hs.iter() { s = s + h(); } s }
}
fn audit() -> i64 with reads(Clk) {
    let mut bus = Bus { hs: Vec.new() };
    bus.reg(|| Clk.now());
    bus.run()
}
fn main() {
    println(f\"{with_provider[Clk](F { t: 4 }, || audit())}\");
}
",
        "is stored in an escaping closure",
    );
}
