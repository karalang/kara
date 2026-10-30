//! A `dyn Handler with E` read out of a collection in `self` (an element of
//! `Vec[dyn Handler with E]`, or the payload of an `Option` element bound by
//! `if let Some(h)`) has the effects of the handlers that can flow into the
//! value it is read out of (design.md § Effect variables), so a `par` over
//! two dispatches through effect-free handlers runs, and one through a
//! printing handler is refused, wherever that handler was added.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

const HANDLERS: &str = "pub trait Handler {
    fn handle(self, req: i64) -> i64;
}
struct Double {}
impl Handler for Double {
    fn handle(self, req: i64) -> i64 { req * 2 }
}
struct Logged { n: i64 }
impl Handler for Logged {
    fn handle(self, req: i64) -> i64 { println(f\"req {req}\"); req + self.n }
}
";

const OPTION_ROUTER: &str = "struct Router[with E] {
    routes: Vec[Option[dyn Handler with E]],
}
impl[with E] Router[with E] {
    fn add(mut ref self, h: escaping dyn Handler with E) {
        self.routes.push(Some(h));
    }
    fn dispatch(self, i: i64, req: i64) -> i64 with E {
        if let Some(h) = self.routes[i] {
            return h.handle(req);
        }
        -1
    }
}
";

const VEC_ROUTER: &str = "struct Router[with E] {
    routes: Vec[dyn Handler with E],
}
impl[with E] Router[with E] {
    fn add(mut ref self, h: escaping dyn Handler with E) {
        self.routes.push(h);
    }
    fn dispatch(self, i: i64, req: i64) -> i64 with E {
        let h = ref self.routes[i];
        h.handle(req)
    }
    fn total(self, req: i64) -> i64 with E {
        let mut t = 0;
        for h in self.routes.iter() { t += h.handle(req); }
        t
    }
}
";

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-dyn-effect-var-elements-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, format!("{HANDLERS}\n{src}\n")).unwrap();
    (dir, path)
}

fn run(tag: &str, src: &str) -> (bool, String) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn runs(tag: &str, src: &str, want: &str) {
    let (ok, out) = run(tag, src);
    assert!(ok, "{tag}: must run: {out}");
    assert_eq!(out, want, "{tag}");
}

fn refused(tag: &str, src: &str) {
    let (ok, out) = run(tag, src);
    assert!(!ok, "{tag}: must be refused: {out}");
    assert!(
        out.contains("the `par` conflict: `writes(Stdout)` and `writes(Stdout)`"),
        "{tag}: {out}"
    );
}

#[test]
fn dyn_element_option_payload_through_quiet_handlers_runs_in_par() {
    runs(
        "option_quiet",
        &format!(
            "{OPTION_ROUTER}
fn main() {{
    let mut r = Router {{ routes: Vec.new() }};
    r.add(Double {{}});
    let x = par {{ r.dispatch(0, 4), r.dispatch(0, 5) }};
    println(f\"{{x.0}} {{x.1}}\");
}}"
        ),
        "8 10\n",
    );
}

#[test]
fn dyn_element_indexed_and_iterated_through_quiet_handlers_run_in_par() {
    runs(
        "vec_quiet",
        &format!(
            "{VEC_ROUTER}
fn main() {{
    let mut r = Router {{ routes: Vec.new() }};
    r.add(Double {{}});
    let x = par {{ r.dispatch(0, 4), r.total(5) }};
    println(f\"{{x.0}} {{x.1}}\");
}}"
        ),
        "8 10\n",
    );
}

#[test]
fn dyn_element_through_a_printing_handler_is_refused_in_par() {
    refused(
        "option_here",
        &format!(
            "{OPTION_ROUTER}
fn main() {{
    let mut r = Router {{ routes: Vec.new() }};
    r.add(Double {{}});
    r.add(Logged {{ n: 1 }});
    let x = par {{ r.dispatch(0, 4), r.dispatch(1, 5) }};
    println(f\"{{x.0}} {{x.1}}\");
}}"
        ),
    );
    refused(
        "vec_here",
        &format!(
            "{VEC_ROUTER}
fn main() {{
    let mut r = Router {{ routes: Vec.new() }};
    r.add(Logged {{ n: 1 }});
    let x = par {{ r.dispatch(0, 4), r.total(5) }};
    println(f\"{{x.0}} {{x.1}}\");
}}"
        ),
    );
}

/// The printing handler is added by a callee the router is lent to, or by
/// the function that built the router and handed it back.
#[test]
fn dyn_element_handler_added_elsewhere_is_seen() {
    refused(
        "callee_adds",
        &format!(
            "{OPTION_ROUTER}
fn fill(r: mut ref Router) {{ r.add(Logged {{ n: 1 }}); }}
fn main() {{
    let mut r = Router {{ routes: Vec.new() }};
    fill(mut r);
    let x = par {{ r.dispatch(0, 4), r.dispatch(0, 5) }};
    println(f\"{{x.0}} {{x.1}}\");
}}"
        ),
    );
    refused(
        "built_elsewhere",
        &format!(
            "{OPTION_ROUTER}
fn make() -> Router {{
    let mut r = Router {{ routes: Vec.new() }};
    r.add(Logged {{ n: 1 }});
    r
}}
fn main() {{
    let r = make();
    let x = par {{ r.dispatch(0, 4), r.dispatch(0, 5) }};
    println(f\"{{x.0}} {{x.1}}\");
}}"
        ),
    );
}
