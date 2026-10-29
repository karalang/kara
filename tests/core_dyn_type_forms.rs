//! The `dyn` type forms of design.md § `dyn Trait`: an effect clause
//! (`dyn Handler with E`), `dyn Trait + CrossTask`, and `escaping dyn` on a
//! parameter that the function stores. A `dyn Trait` is not `CrossTask`,
//! since its concrete type is not in the type; `dyn Trait + CrossTask` is,
//! and each coercion into it checks the concrete type. A trait with
//! `CrossTask` among its supertraits makes every `dyn` of it `CrossTask`.
//! A task may still view a plain `dyn` value ("a view follows the view
//! rules").

use std::process::Command;

fn karac(cmd: &str, name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-dynforms-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn dyn_type_forms_check_and_run() {
    for (name, src, want) in [
        (
            "escaping_router",
            r#"trait Handler {
    fn handle(self, req: i64) -> i64;
}

struct Double {}
impl Handler for Double {
    fn handle(self, req: i64) -> i64 { req * 2 }
}

struct Add { n: i64 }
impl Handler for Add {
    fn handle(self, req: i64) -> i64 { req + self.n }
}

struct Router[with E] {
    routes: Vec[(String, dyn Handler with E)],
}

impl[with E] Router[with E] {
    fn add(mut ref self, path: Str, h: escaping dyn Handler with E) {
        self.routes.push((path.to_string(), h));
    }
    fn dispatch(self, path: Str, req: i64) -> i64 with E {
        for (p, h) in self.routes.iter() {
            if p == path { return h.handle(req); }
        }
        -1
    }
}

fn main() {
    let mut r = Router { routes: Vec.new() };
    r.add("/double", Double {});
    r.add("/add", Add { n: 5 });
    println(r.dispatch("/double", 21));
    println(r.dispatch("/add", 1));
    println(r.dispatch("/none", 1));
}
"#,
            "42
6
-1
",
        ),
        (
            "crosstask_view_in_task",
            r#"trait Shape {
    fn area(self) -> i64;
}
struct Sq { s: i64 }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }

fn main() {
    let shapes: Vec[dyn Shape + CrossTask] = [Sq { s: 2 }, Sq { s: 3 }];
    let tg = TaskGroup.new();
        let h = tg.spawn(|| {
        let mut t = 0;
        for s in shapes.iter() { t = t + s.area(); }
        t
    });
    let total = h.join();
    println(total);
}
"#,
            "13
",
        ),
        (
            "crosstask_sent",
            r#"trait Shape {
    fn area(self) -> i64;
}
struct Sq { s: i64 }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }

fn main() {
    let (tx, rx) = channel(4);
    let s: dyn Shape + CrossTask = Sq { s: 3 };
    let _ = tx.send(s);
    let got: dyn Shape + CrossTask = rx.recv().unwrap();
    println(got.area());
}
"#,
            "9
",
        ),
        (
            "crosstask_supertrait",
            r#"trait Job: CrossTask {
    fn run(self) -> i64;
}
struct J { n: i64 }
impl Job for J { fn run(self) -> i64 { self.n } }
fn main() {
    let jobs: Vec[dyn Job] = [J { n: 4 }, J { n: 5 }];
    let tg = TaskGroup.new();
        let h = tg.spawn(|| {
        let mut t = 0;
        for j in jobs.iter() { t = t + j.run(); }
        t
    });
    let total = h.join();
    println(total);
}
"#,
            "9
",
        ),
    ] {
        let (ok, _, err) = karac("check", name, src);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}

#[test]
fn dyn_type_forms_are_refused_where_the_spec_refuses_them() {
    for (name, src, wants) in [
        (
            "plain_dyn_sent",
            r#"trait Shape {
    fn area(self) -> i64;
}
struct Sq { s: i64 }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }

fn main() {
    let (tx, rx) = channel(4);
    let s: dyn Shape = Sq { s: 3 };
    let _ = tx.send(s);
    let got: dyn Shape = rx.recv().unwrap();
    println(got.area());
}
"#,
            &["E_NOT_CROSS_TASK", "write `dyn Shape + CrossTask`"][..],
        ),
        (
            "task_local_coerced_into_crosstask",
            r#"trait Shape {
    fn area(self) -> i64;
}
shared struct Cell { v: i64 }
struct Holder { c: Cell }
impl Shape for Holder { fn area(self) -> i64 { self.c.v } }

fn main() {
    let shapes: Vec[dyn Shape + CrossTask] = [Holder { c: Cell { v: 2 } }];
    println(shapes.len());
}
"#,
            &["`Holder` is not `CrossTask`", "through field 'c'"][..],
        ),
        (
            "second_trait",
            r#"trait Shape {
    fn area(self) -> i64;
}
fn f(s: ref dyn Shape + Display) -> i64 { s.area() }
fn main() {}
"#,
            &["E_DYN_SECOND_TRAIT", "`Display` cannot be added"][..],
        ),
        (
            "crosstask_supertrait_on_task_local",
            r#"trait Job: CrossTask {
    fn run(self) -> i64;
}
shared struct Cell { v: i64 }
struct J { c: Cell }
impl Job for J { fn run(self) -> i64 { self.c.v } }
fn main() {}
"#,
            &["E_NOT_CROSS_TASK", "requires `J: CrossTask`"][..],
        ),
        (
            "escaping_dyn_off_a_parameter",
            r#"trait Shape {
    fn area(self) -> i64;
}
struct Holder { s: escaping dyn Shape }
fn main() {}
"#,
            &["only accepted on a parameter's type"][..],
        ),
    ] {
        let (ok, _, err) = karac("check", name, src);
        assert!(!ok, "{name}: expected a refusal");
        for want in wants {
            assert!(err.contains(want), "{name}: missing {want:?} in {err}");
        }
    }
}

#[test]
fn the_formatter_keeps_every_dyn_form() {
    let src = "trait Handler {
    fn handle(self, req: i64) -> i64;
}

struct Router[with E] {
    routes: Vec[(String, dyn Handler + CrossTask with E)],
}

impl[with E] Router[with E] {
    fn add(mut ref self, path: Str, h: escaping dyn Handler + CrossTask with E) {
        self.routes.push((path.to_string(), h));
    }
}
";
    let (ok, out, err) = karac("fmt", "fmt", src);
    assert!(ok, "{err}");
    assert!(
        out.contains("routes: Vec[(String, dyn Handler + CrossTask with E)],"),
        "{out}"
    );
    assert!(
        out.contains("h: escaping dyn Handler + CrossTask with E)"),
        "{out}"
    );
}
