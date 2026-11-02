//! What a 0.8.0 program meets in 0.8.1 under the `+ CrossTask` marker
//! (core semantics §6.3). A plain `dyn` value given where the marked form
//! is expected is refused naming the marker, with the fix-it at the local's
//! declaration (B-2026-11-02-13). A collection of erased functions that a
//! call hands back is opaque, as the bare function is, so it does not slip
//! into a marked slot, and neither does a `let mut` one filled by `push`
//! (B-2026-11-02-14).

use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("karac-marker-upgrade-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn karac(cmd: &str, path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(path)
        .output()
        .unwrap()
}

const ROUTER: &str = "trait Handler { fn handle(ref self) -> i64; }
struct K { n: i64 }
impl Handler for K { fn handle(ref self) -> i64 { self.n } }
struct Router { routes: Vec[dyn Handler + CrossTask] }
";

#[test]
fn a_plain_dyn_vec_into_a_marked_slot_names_the_marker() {
    let src = format!(
        "{ROUTER}fn main() {{
    let mut routes: Vec[dyn Handler] = Vec.new();
    routes.push(K {{ n: 1 }});
    let r = Router {{ routes: routes }};
    println(r.routes[0].handle());
}}
"
    );
    let (dir, path) = fixture("names", &src);
    let out = karac("check", &path);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("expected 'Vec[dyn Handler + CrossTask]', found 'Vec[dyn Handler]'")
            && err.contains("a `dyn` type without `+ CrossTask` does not become one with it")
            && err.contains(
                "help: write `+ CrossTask` on the `dyn` where the value's type is declared"
            ),
        "{err}"
    );
    let out = karac("fix", &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("let mut routes: Vec[dyn Handler + CrossTask] = Vec.new();"),
        "fix wrote:\n{fixed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac("check", &path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_plain_dyn_from_a_call_names_the_marker_without_a_fix() {
    let src = format!(
        "{ROUTER}fn mk() -> Vec[dyn Handler] {{ let mut v: Vec[dyn Handler] = Vec.new(); v.push(K {{ n: 1 }}); v }}
fn main() {{
    let r = Router {{ routes: mk() }};
    println(r.routes[0].handle());
}}
"
    );
    let (dir, path) = fixture("call", &src);
    let out = karac("check", &path);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("a `dyn` type without `+ CrossTask` does not become one with it"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const BUS: &str = "shared struct S { mut n: i64 }
struct Bus { hs: Vec[Fn() -> i64 + CrossTask] }
";

#[test]
fn an_erased_fn_vec_a_call_returns_does_not_enter_a_marked_slot() {
    let src = format!(
        "{BUS}fn mk(s: own S) -> Vec[Fn() -> i64] {{
    let mut v: Vec[Fn() -> i64] = Vec.new();
    v.push(|| {{ s.n = s.n + 1; s.n }});
    v
}}
fn main() {{
    let b = Bus {{ hs: mk(S {{ n: 0 }}) }};
    let g = TaskGroup.new();
    let x = g.spawn(|| (b.hs[0])());
    println(x.join());
}}
"
    );
    let (dir, path) = fixture("fnvec", &src);
    let out = karac("check", &path);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("E_NOT_CROSS_TASK")
            && err.contains("a value coerced into a field whose type is `+ CrossTask`"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_local_unmarked_fn_vec_into_a_marked_slot_takes_the_marker_at_its_declaration() {
    let src = format!(
        "{BUS}fn main() {{
    let mut hs: Vec[Fn() -> i64] = Vec.new();
    hs.push(|| 1);
    let b = Bus {{ hs: hs }};
    let g = TaskGroup.new();
    let x = g.spawn(|| (b.hs[0])());
    println(x.join());
}}
"
    );
    let (dir, path) = fixture("local", &src);
    let out = karac("check", &path);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("E_NOT_CROSS_TASK")
            && err.contains("a value coerced into a field whose type is `+ CrossTask`"),
        "{err}"
    );
    let out = karac("fix", &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("let mut hs: Vec[Fn() -> i64 + CrossTask] = Vec.new();"),
        "fix wrote:\n{fixed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac("check", &path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The shape that raced: a `shared` handle reached through a closure pushed
/// onto a local `Vec[Fn()]`, which then entered a marked field.
const LEAKY_PUSH: &str = "fn main() {
    let s = S { n: 0 };
    let mut hs: Vec[Fn() -> i64MARK] = Vec.new();
    hs.push(|| { s.n = s.n + 1; s.n });
    let b = Bus { hs: hs };
    let g = TaskGroup.new();
    let x = g.spawn(|| (b.hs[0])());
    let y = g.spawn(|| (b.hs[0])());
    println(x.join() + y.join());
}
";

#[test]
fn a_leaking_closure_pushed_onto_a_local_fn_vec_does_not_reach_a_marked_slot() {
    let src = format!("{BUS}{}", LEAKY_PUSH.replace("MARK", ""));
    let (dir, path) = fixture("leakypush", &src);
    let out = karac("check", &path);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("E_NOT_CROSS_TASK"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_leaking_closure_pushed_onto_a_marked_local_fn_vec_is_refused_at_the_push() {
    let src = format!("{BUS}{}", LEAKY_PUSH.replace("MARK", " + CrossTask"));
    let (dir, path) = fixture("leakypushmarked", &src);
    let out = karac("check", &path);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("an element whose type is `+ CrossTask`") && err.contains("captures `s`"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
