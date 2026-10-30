//! Closures that reach a provider and are run by someone other than their
//! maker, under `with_provider` (core semantics §14.3, §14.5, §14.9): a
//! `Fn` value that writes the provider (B-2026-10-30-3); a task, a sort's
//! comparison, a `retain` predicate and a `par` branch, which the runtime
//! runs (B-2026-10-30-5); tasks that write the provider, which the check
//! refuses when another task or the spawner uses it meanwhile
//! (B-2026-10-30-6); and a stored callback, which would keep a view of the
//! provider (§14.5).

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

/// Every backend, `karac check` included, refuses `src` naming `why`.
fn check_refuses(tag: &str, src: &str, why: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(!out.status.success(), "{tag} on {b}: ran: {stdout}");
        assert!(
            stderr.contains(why) || stdout.contains(why),
            "{tag} on {b}: {stdout}{stderr}"
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

/// A task that writes the provider holds it `mut ref` until its group
/// drops (§14.9, §9.5), so the spawner's own use meanwhile is refused by
/// the check, here the making of a closure that bumps the clock, and so
/// are `both`'s two tasks, which run that closure.
#[test]
fn provider_tasks_writing_the_provider_are_refused() {
    check_refuses(
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
        "which writes `Clk`, may still run",
    );
}

/// One task may write the provider when nothing else uses it while the
/// group lives; the spawner writes it before the spawn and reads it after
/// the group drops.
#[test]
fn provider_one_task_writes_the_provider() {
    runs(
        "task-writes",
        "fn main() {
    let r = with_provider[Clk](F { t: 0 }, || {
        Clk.bump();
        let a = {
            let tg = TaskGroup.new();
            let h = tg.spawn(|| { Clk.bump(); Clk.now() });
            h.join()
        };
        a * 10 + Clk.now()
    });
    println(f\"{r}\");
}
",
        "22\n",
    );
}

/// Two tasks of one group that write the provider, a looped spawn of one,
/// a task beside a spawner that writes, and a helper that leaves a writing
/// task in the group it is lent: each refused by the check (§14.9).
#[test]
fn provider_tasks_sharing_a_written_provider_are_refused() {
    let cases = [
        (
            "two-tasks",
            "fn main() {
    let r = with_provider[Clk](F { t: 0 }, || {
        let tg = TaskGroup.new();
        let a = tg.spawn(|| { Clk.bump(); Clk.now() });
        let b = tg.spawn(|| { Clk.bump(); Clk.now() });
        a.join() + b.join()
    });
    println(f\"{r}\");
}
",
            "this task uses `Clk` while the task spawned at line 11",
        ),
        (
            "looped",
            "fn main() {
    let r = with_provider[Clk](F { t: 0 }, || {
        let tg = TaskGroup.new();
        let mut hs = Vec.new();
        for _ in 0..3 {
            hs.push(tg.spawn(|| { Clk.bump(); Clk.now() }));
        }
        let mut s = 0;
        while let Some(h) = hs.pop() { s = s + h.join(); }
        s
    });
    println(f\"{r}\");
}
",
            "each time it runs",
        ),
        (
            "spawner-writes",
            "fn main() {
    let r = with_provider[Clk](F { t: 0 }, || {
        let tg = TaskGroup.new();
        let a = tg.spawn(|| Clk.now());
        Clk.bump();
        a.join()
    });
    println(f\"{r}\");
}
",
            "`Clk` is written here while the task spawned at line 11",
        ),
        (
            "lent-group",
            "fn start(tg: ref TaskGroup) -> TaskHandle[i64] {
    tg.spawn(|| { Clk.bump(); Clk.now() })
}
fn main() {
    let r = with_provider[Clk](F { t: 0 }, || {
        let tg = TaskGroup.new();
        let a = start(tg);
        let b = start(tg);
        a.join() + b.join()
    });
    println(f\"{r}\");
}
",
            "this task uses `Clk` while the task spawned at line 14",
        ),
    ];
    for (tag, src, why) in cases {
        check_refuses(tag, src, why);
    }
}

/// A callback stored on a bus, made where `Clk` is bound by the caller's
/// `with_provider`, would keep a view of the provider (§14.5). The check
/// refuses it though the binding is out of sight (B-2026-10-30-7), naming
/// the function it is made in; the borrow checker refuses the store too.
#[test]
fn provider_stored_callback_is_refused() {
    let (tag, src) = (
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
    );
    for b in backends() {
        let out = karac(tag, b, src);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag} on {b}: ran");
        let why = if *b == "check" {
            "a caller of `audit` binds"
        } else {
            "is stored in an escaping closure"
        };
        assert!(stderr.contains(why), "{tag} on {b}: {stderr}");
    }
}
