//! An inherent associated function and a trait's function of the same name
//! (B-2026-11-20-65). design.md § Associated functions: the inherent one
//! hides the trait's at `T.name()`, and `Trait.name(args)` reaches the
//! trait's. Lookup took whichever impl came first, so the order of the impl
//! blocks decided which one a call reached. At `e.m()` an associated
//! function is not a method (§ Method resolution), so it does not hide a
//! trait's `m(self)` there; and a bound's impl method with generic
//! parameters of its own is called at an instance in a generic body.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-assoc-fn-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

/// Checks clean, then prints `want` on the interpreter and, with the
/// backend, compiled.
fn runs(src: &str, want: &str) {
    let path = scratch(src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
    if cfg!(feature = "llvm") {
        let exe = path.with_extension("bin");
        let build = karac(&["build", "-o", exe.to_str().unwrap()], &path);
        assert!(build.status.success(), "{}", text(&build));
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), want, "{}", text(&ran));
    }
}

#[test]
fn an_inherent_assoc_fn_hides_a_traits_in_either_impl_order() {
    let inherent_first = r#"trait Put { fn put(ref self, n: i64) -> String; }
trait Get { fn get(n: i64) -> Self; }
struct W { v: i64 }
impl W {
    fn put(text: String) -> String { f"inherent put {text}" }
    fn get(text: String) -> W { W { v: text.len() as i64 } }
}
impl Put for W { fn put(ref self, n: i64) -> String { f"trait put {self.v} {n}" } }
impl Get for W { fn get(n: i64) -> W { W { v: n * 100 } } }
fn main() {
    println(W.put("abc".to_string()));
    let g = W.get("abcd".to_string());
    println(g.v);
    let w = W { v: 1 };
    println(Put.put(w, 2));
    let h: W = Get.get(7);
    println(h.v);
}
"#;
    runs(inherent_first, "inherent put abc\n4\ntrait put 1 2\n700\n");
    let trait_first = r#"trait Put { fn put(ref self, n: i64) -> String; }
trait Get { fn get(n: i64) -> Self; }
struct W { v: i64 }
impl Put for W { fn put(ref self, n: i64) -> String { f"trait put {self.v} {n}" } }
impl Get for W { fn get(n: i64) -> W { W { v: n * 100 } } }
impl W {
    fn put(text: String) -> String { f"inherent put {text}" }
    fn get(text: String) -> W { W { v: text.len() as i64 } }
}
fn main() {
    println(W.put("abc".to_string()));
    let g = W.get("abcd".to_string());
    println(g.v);
    let w = W { v: 1 };
    println(Put.put(w, 2));
    let h: W = Get.get(7);
    println(h.v);
}
"#;
    runs(trait_first, "inherent put abc\n4\ntrait put 1 2\n700\n");
}

#[test]
fn a_method_call_reaches_a_trait_method_past_an_inherent_assoc_fn() {
    let src = r#"trait Put { fn put[T: Display](ref self, n: T) -> String; }
struct W { v: i64 }
impl W {
    fn put(text: String) -> String { f"inherent put {text}" }
}
impl Put for W { fn put[T: Display](ref self, n: T) -> String { f"trait put {self.v} {n}" } }
struct Box2 { w: W }
fn via[P: Put](p: ref P) -> String { p.put(5) }
fn main() {
    let w = W { v: 1 };
    println(w.put(2));
    let b = Box2 { w: W { v: 3 } };
    println(b.w.put(4));
    println(via(w));
    println(W.put("x".to_string()));
}
"#;
    runs(
        src,
        "trait put 1 2\ntrait put 3 4\ntrait put 1 5\ninherent put x\n",
    );
}

#[test]
fn a_bound_reaches_a_generic_impl_fn_past_an_inherent_one() {
    let src = r#"trait Source { fn next_val(mut ref self) -> i64; }
struct Counter { n: i64 }
impl Source for Counter {
    fn next_val(mut ref self) -> i64 {
        self.n += 1;
        self.n
    }
}
trait Build { fn build[S: Source](s: mut ref S) -> Self; }
trait Emit { fn emit[S: Source](ref self, s: mut ref S) -> String; }
struct P { v: i64 }
impl Build for P {
    fn build[S: Source](s: mut ref S) -> P { P { v: s.next_val() * 10 } }
}
impl Emit for P {
    fn emit[S: Source](ref self, s: mut ref S) -> String { f"emit {self.v} {s.next_val()}" }
}
impl P {
    fn build(v: i64) -> P { P { v: v } }
    fn emit(text: String) -> String { f"inherent emit {text}" }
}
fn make[T: Build, S: Source](s: mut ref S) -> T { T.build(s) }
fn say[T: Emit, S: Source](t: ref T, s: mut ref S) -> String { t.emit(s) }
fn main() {
    let mut c = Counter { n: 0 };
    let p: P = make(mut c);
    println(p.v);
    println(say(p, mut c));
    println(p.emit(mut c));
    println(P.build(4).v);
    println(P.emit("x".to_string()));
}
"#;
    runs(src, "10\nemit 10 2\nemit 10 3\n4\ninherent emit x\n");
}
